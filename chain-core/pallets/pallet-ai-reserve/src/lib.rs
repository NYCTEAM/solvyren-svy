//! Solvyren AI reserve, stage S4 (ISOLATED PALLET CODE).
//!
//! Scope and boundaries:
//! - Implements the draft calls 12-15 (`reserve_ai_job`, `commit_ai_result`, `settle_ai_job`,
//!   `cancel_uncommitted_ai_job`) and the T-B timeout call 16 (`refund_timed_out_ai_job`).
//! - Tested only on the mock runtime in `mock.rs`. This crate is NOT part of any Runtime
//!   composition, is not referenced by `chain-core/runtime`, and does not touch the legacy
//!   `pallet-ai-router` calls 0-11 or its storage (`NextTaskId`, `ActiveTasks`, ...).
//! - No new issuance: payments only move balance that is already reserved on `RewardAccount`.
//! - v1 rules: cached billing forbidden, rates read-only (genesis), no role rotation (G1),
//!   no on-chain rate changes (G2), no dispute adjudication (G3). There is no `Disputed` state.
//! - `SettleWindow` is a pallet constant; the mock value 20 is a TEST value, not a mainnet parameter.
//! - Weights are placeholders, not benchmarked.
#![cfg_attr(not(feature = "std"), no_std)]

extern crate alloc;

pub use pallet::*;

#[cfg(test)]
mod mock;
#[cfg(test)]
mod tests;

#[frame_support::pallet]
pub mod pallet {
    use alloc::vec::Vec;
    use frame_support::sp_runtime::traits::{CheckedAdd, CheckedSub, Saturating, Zero};
    use frame_support::{
        pallet_prelude::*,
        traits::{BalanceStatus, Currency, EnsureOrigin, ReservableCurrency, SortedMembers},
        transactional,
    };
    use frame_system::pallet_prelude::*;
    use sp_io::hashing::blake2_256;

    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;
    pub type TermsOf<T> =
        AiJobTerms<<T as frame_system::Config>::AccountId, BlockNumberFor<T>>;
    pub type AiJobOf<T> = AiJob<
        <T as frame_system::Config>::AccountId,
        BlockNumberFor<T>,
        BalanceOf<T>,
    >;

    /// Maximum members of each role set (genesis only in v1).
    pub const MAX_ROLE_MEMBERS: u32 = 4;
    /// Token prices are atoms per one million tokens.
    pub const TOKENS_PER_PRICE_UNIT: u128 = 1_000_000;

    /// Rate used for hashing. `version` is deliberately NOT a field here: it lives on the
    /// registry entry so that `rate_id = blake2_256(TokenRate.encode())` is version-free.
    #[derive(
        Clone,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
        serde::Serialize,
        serde::Deserialize,
    )]
    pub struct TokenRate {
        pub model_digest: [u8; 32],
        pub tokenizer_digest: [u8; 32],
        pub runtime_digest: [u8; 32],
        /// Atoms per one million uncached input tokens.
        pub input_per_million: u128,
        /// v1: must be 0, and receipts must report `cached_tokens == 0`.
        pub cached_per_million: u128,
        pub output_per_million: u128,
        pub max_input_tokens: u32,
        pub max_output_tokens: u32,
    }

    impl TokenRate {
        /// On-chain rate key: `blake2_256(TokenRate.encode())` (SCALE, no version).
        pub fn rate_id(&self) -> [u8; 32] {
            blake2_256(&self.encode())
        }
    }

    /// Registry entry. The version is metadata of the entry, not part of the rate hash.
    #[derive(
        Clone,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
        serde::Serialize,
        serde::Deserialize,
    )]
    pub struct RateEntry {
        pub rate: TokenRate,
        pub version: u16,
    }

    #[derive(
        Clone,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
    )]
    pub struct AiJobTerms<AccountId, BlockNumber> {
        /// Must equal this chain's `block_hash(0)`.
        pub genesis: [u8; 32],
        /// Permanently unique; a redispatch must use a new commitment (attempt number).
        pub request_commitment: [u8; 32],
        /// Only this account may call 13.
        pub worker: AccountId,
        /// Must not equal `RewardAccount`.
        pub beneficiary: AccountId,
        /// `blake2_256(TokenRate.encode())`; must be registered.
        pub rate_id: [u8; 32],
        pub expires_at: BlockNumber,
    }

    #[derive(
        Clone,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
    )]
    pub struct UsageReceipt {
        /// Permanently unique once consumed by a settlement.
        pub receipt_id: [u8; 32],
        /// `blake2_256(terms.encode())` over on-chain SCALE terms.
        pub terms_digest: [u8; 32],
        /// Must equal the digest committed with call 13.
        pub result_digest: [u8; 32],
        pub execution_evidence_digest: [u8; 32],
        pub input_tokens: u32,
        pub cached_tokens: u32,
        pub output_tokens: u32,
    }

    /// There is intentionally no `Disputed` variant (G3 adjudication is not implemented).
    #[derive(
        Clone,
        Copy,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
    )]
    pub enum AiJobState {
        Reserved,
        Committed,
        Settled,
        Cancelled,
        Refunded,
    }

    impl AiJobState {
        pub fn is_terminal(&self) -> bool {
            matches!(self, Self::Settled | Self::Cancelled | Self::Refunded)
        }
    }

    #[derive(
        Clone,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
    )]
    pub struct AiJob<AccountId, BlockNumber, Balance> {
        pub terms: AiJobTerms<AccountId, BlockNumber>,
        pub max_amount: Balance,
        pub state: AiJobState,
        pub commit: Option<[u8; 32]>,
        /// Block of call 13; the T-B window is measured from here.
        pub commit_block: Option<BlockNumber>,
        pub paid: Balance,
        pub receipt_id: Option<[u8; 32]>,
        /// `blake2_256(receipt.encode())` of the consumed receipt, for recovered replays.
        pub receipt_digest: Option<[u8; 32]>,
    }

    #[derive(
        Clone,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
        Default,
    )]
    pub struct AiTotals<Balance> {
        /// Currently reserved for non-terminal jobs.
        pub reserved: Balance,
        /// Cumulative paid to beneficiaries.
        pub paid: Balance,
        /// Cumulative returned to the reserve's free balance (settle remainder, cancel, T-B).
        pub refunded: Balance,
    }

    #[pallet::config]
    pub trait Config: frame_system::Config<RuntimeEvent: From<Event<Self>>> {
        type Currency: ReservableCurrency<Self::AccountId>;
        /// Calls 12 and 15. A runtime would use `EnsureSignedBy<SchedulerMembers<Self>, _>`.
        type SchedulerOrigin: EnsureOrigin<Self::RuntimeOrigin, Success = Self::AccountId>;
        /// Call 14. A runtime would use `EnsureSignedBy<AttestorMembers<Self>, _>`.
        type AttestorOrigin: EnsureOrigin<Self::RuntimeOrigin, Success = Self::AccountId>;
        /// The reserve account (`eag/ai00` in the spec). Funds are reserved on it.
        #[pallet::constant]
        type RewardAccount: Get<Self::AccountId>;
        #[pallet::constant]
        type MaxLeaseBlocks: Get<BlockNumberFor<Self>>;
        /// T-B settle window counted from the commit block. Test value only in the mock.
        #[pallet::constant]
        type SettleWindow: Get<BlockNumberFor<Self>>;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    pub type Schedulers<T: Config> =
        StorageValue<_, BoundedVec<T::AccountId, ConstU32<MAX_ROLE_MEMBERS>>, ValueQuery>;
    #[pallet::storage]
    pub type Attestors<T: Config> =
        StorageValue<_, BoundedVec<T::AccountId, ConstU32<MAX_ROLE_MEMBERS>>, ValueQuery>;
    /// Read-only in v1 (written only at genesis).
    #[pallet::storage]
    pub type AiRates<T> = StorageMap<_, Blake2_128Concat, [u8; 32], RateEntry, OptionQuery>;
    /// Independent of the legacy `NextTaskId`.
    #[pallet::storage]
    pub type NextAiJobId<T> = StorageValue<_, u64, ValueQuery>;
    #[pallet::storage]
    pub type AiJobs<T: Config> = StorageMap<_, Blake2_128Concat, u64, AiJobOf<T>, OptionQuery>;
    /// Permanent: request commitments are never released.
    #[pallet::storage]
    pub type AiRequestJobs<T> = StorageMap<_, Blake2_128Concat, [u8; 32], u64, OptionQuery>;
    /// At most one non-terminal job per worker; removed on any terminal transition.
    #[pallet::storage]
    pub type OpenJobOf<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, u64, OptionQuery>;
    /// Permanent: consumed receipts are never released.
    #[pallet::storage]
    pub type UsedReceipts<T> = StorageMap<_, Blake2_128Concat, [u8; 32], u64, OptionQuery>;
    #[pallet::storage]
    pub type AiReserveTotals<T: Config> = StorageValue<_, AiTotals<BalanceOf<T>>, ValueQuery>;

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        pub schedulers: Vec<T::AccountId>,
        pub attestors: Vec<T::AccountId>,
        pub rates: Vec<RateEntry>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            let reward = T::RewardAccount::get();
            let schedulers = Pallet::<T>::sorted_role_set(&self.schedulers, &reward, "scheduler");
            let attestors = Pallet::<T>::sorted_role_set(&self.attestors, &reward, "attestor");
            for s in schedulers.iter() {
                assert!(
                    attestors.binary_search(s).is_err(),
                    "genesis: schedulers and attestors must not overlap"
                );
            }
            for entry in self.rates.iter() {
                let rate = &entry.rate;
                assert!(rate.cached_per_million == 0, "genesis: v1 forbids cached billing");
                assert!(
                    rate.max_input_tokens > 0 && rate.max_output_tokens > 0,
                    "genesis: rate caps must be positive"
                );
                assert!(
                    Pallet::<T>::quote(rate, rate.max_input_tokens, 0, rate.max_output_tokens)
                        .is_ok(),
                    "genesis: rate maximum overflows"
                );
                let id = rate.rate_id();
                assert!(!AiRates::<T>::contains_key(id), "genesis: duplicate rate");
                AiRates::<T>::insert(id, entry);
            }
            Schedulers::<T>::put(
                BoundedVec::try_from(schedulers).expect("genesis: too many schedulers"),
            );
            Attestors::<T>::put(
                BoundedVec::try_from(attestors).expect("genesis: too many attestors"),
            );
        }
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        AiJobReserved {
            job_id: u64,
            worker: T::AccountId,
            beneficiary: T::AccountId,
            rate_id: [u8; 32],
            max_amount: BalanceOf<T>,
        },
        AiResultCommitted {
            job_id: u64,
            result_digest: [u8; 32],
        },
        AiJobSettled {
            job_id: u64,
            beneficiary: T::AccountId,
            paid: BalanceOf<T>,
            refunded: BalanceOf<T>,
            receipt_id: [u8; 32],
        },
        AiJobCancelled {
            job_id: u64,
            refunded: BalanceOf<T>,
        },
        /// T-B: committed job not settled within `SettleWindow`; full reservation returned.
        AiJobRefunded {
            job_id: u64,
            refunded: BalanceOf<T>,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        WrongGenesis,
        UnknownRate,
        RequestAlreadyBound,
        WorkerBusy,
        InvalidBeneficiary,
        LeaseTooLong,
        Expired,
        NotWorker,
        NotReserved,
        NotCommitted,
        AlreadyTerminal,
        ResultMismatch,
        TermsMismatch,
        ReceiptAlreadyUsed,
        CachedNotAllowed,
        CachedExceedsInput,
        UsageAboveCap,
        Overflow,
        IncompletePayment,
        /// T-B: refund requested while the settle window is still open.
        NotTimedOut,
        /// T-B: settlement attempted after the settle window closed.
        SettleWindowClosed,
        /// No job with this id.
        UnknownJob,
        /// A digest that must be non-zero was zero.
        InvalidDigest,
        /// Reserving the maximum would leave the reserve below the existential deposit.
        InsufficientReserve,
        /// Worker equals the reserve account.
        InvalidWorker,
        /// Worker is a scheduler or attestor (S2 aligned role separation).
        RoleOverlap,
    }

    /// Placeholder weight (not benchmarked; S5 must benchmark against the target runtime).
    fn placeholder_weight<T: frame_system::Config>(reads: u64, writes: u64) -> Weight {
        Weight::from_parts(50_000_000, 4_096)
            .saturating_add(T::DbWeight::get().reads_writes(reads, writes))
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// 12: scheduler reserves the rate maximum on `RewardAccount`.
        /// Replaying identical terms for a bound request is recovered (Ok, no change, no event).
        #[pallet::call_index(12)]
        #[pallet::weight(placeholder_weight::<T>(9, 6))]
        #[transactional]
        pub fn reserve_ai_job(origin: OriginFor<T>, terms: TermsOf<T>) -> DispatchResult {
            T::SchedulerOrigin::ensure_origin(origin)?;
            if let Some(existing) = AiRequestJobs::<T>::get(terms.request_commitment) {
                let job = AiJobs::<T>::get(existing).ok_or(Error::<T>::UnknownJob)?;
                ensure!(job.terms == terms, Error::<T>::RequestAlreadyBound);
                return Ok(());
            }
            ensure!(terms.request_commitment != [0u8; 32], Error::<T>::InvalidDigest);
            let genesis = Self::genesis_hash();
            ensure!(
                genesis != [0u8; 32] && terms.genesis == genesis,
                Error::<T>::WrongGenesis
            );
            let entry = AiRates::<T>::get(terms.rate_id).ok_or(Error::<T>::UnknownRate)?;
            let reward = T::RewardAccount::get();
            ensure!(terms.beneficiary != reward, Error::<T>::InvalidBeneficiary);
            ensure!(terms.worker != reward, Error::<T>::InvalidWorker);
            ensure!(!Self::is_role(&terms.worker), Error::<T>::RoleOverlap);
            ensure!(
                !OpenJobOf::<T>::contains_key(&terms.worker),
                Error::<T>::WorkerBusy
            );
            let now = frame_system::Pallet::<T>::block_number();
            ensure!(terms.expires_at > now, Error::<T>::Expired);
            ensure!(
                terms.expires_at <= now.saturating_add(T::MaxLeaseBlocks::get()),
                Error::<T>::LeaseTooLong
            );
            let max_amount = Self::to_balance(Self::quote(
                &entry.rate,
                entry.rate.max_input_tokens,
                0,
                entry.rate.max_output_tokens,
            )?)?;
            let needed = max_amount
                .checked_add(&T::Currency::minimum_balance())
                .ok_or(Error::<T>::Overflow)?;
            ensure!(
                T::Currency::free_balance(&reward) >= needed,
                Error::<T>::InsufficientReserve
            );
            T::Currency::reserve(&reward, max_amount)?;
            let job_id = NextAiJobId::<T>::get();
            let next_id = job_id.checked_add(1).ok_or(Error::<T>::Overflow)?;
            AiReserveTotals::<T>::try_mutate(|t| -> DispatchResult {
                t.reserved = t
                    .reserved
                    .checked_add(&max_amount)
                    .ok_or(Error::<T>::Overflow)?;
                Ok(())
            })?;
            let event = Event::<T>::AiJobReserved {
                job_id,
                worker: terms.worker.clone(),
                beneficiary: terms.beneficiary.clone(),
                rate_id: terms.rate_id,
                max_amount,
            };
            AiRequestJobs::<T>::insert(terms.request_commitment, job_id);
            OpenJobOf::<T>::insert(&terms.worker, job_id);
            AiJobs::<T>::insert(
                job_id,
                AiJob {
                    terms,
                    max_amount,
                    state: AiJobState::Reserved,
                    commit: None,
                    commit_block: None,
                    paid: Zero::zero(),
                    receipt_id: None,
                    receipt_digest: None,
                },
            );
            NextAiJobId::<T>::put(next_id);
            Self::deposit_event(event);
            Ok(())
        }

        /// 13: the worker commits its result digest before `expires_at` (inclusive).
        /// Re-committing the same digest is recovered; a different digest is `ResultMismatch`.
        #[pallet::call_index(13)]
        #[pallet::weight(placeholder_weight::<T>(1, 1))]
        #[transactional]
        pub fn commit_ai_result(
            origin: OriginFor<T>,
            job_id: u64,
            result_digest: [u8; 32],
        ) -> DispatchResult {
            let who = ensure_signed(origin)?;
            let mut job = AiJobs::<T>::get(job_id).ok_or(Error::<T>::UnknownJob)?;
            ensure!(who == job.terms.worker, Error::<T>::NotWorker);
            if let Some(existing) = job.commit {
                ensure!(existing == result_digest, Error::<T>::ResultMismatch);
                return Ok(());
            }
            ensure!(result_digest != [0u8; 32], Error::<T>::InvalidDigest);
            match job.state {
                AiJobState::Reserved => {}
                AiJobState::Committed => return Err(Error::<T>::NotReserved.into()),
                _ => return Err(Error::<T>::AlreadyTerminal.into()),
            }
            let now = frame_system::Pallet::<T>::block_number();
            ensure!(now <= job.terms.expires_at, Error::<T>::Expired);
            job.state = AiJobState::Committed;
            job.commit = Some(result_digest);
            job.commit_block = Some(now);
            AiJobs::<T>::insert(job_id, job);
            Self::deposit_event(Event::<T>::AiResultCommitted { job_id, result_digest });
            Ok(())
        }

        /// 14: attestor settles with a usage receipt; the amount is computed on-chain.
        /// Fund movement copies the order of pallet-ai-router `pay_verified_result`
        /// without calling it. Replaying the identical receipt on a settled job is recovered.
        #[pallet::call_index(14)]
        #[pallet::weight(placeholder_weight::<T>(6, 6))]
        #[transactional]
        pub fn settle_ai_job(
            origin: OriginFor<T>,
            job_id: u64,
            receipt: UsageReceipt,
        ) -> DispatchResult {
            T::AttestorOrigin::ensure_origin(origin)?;
            let mut job = AiJobs::<T>::get(job_id).ok_or(Error::<T>::UnknownJob)?;
            match job.state {
                AiJobState::Committed => {}
                AiJobState::Settled => {
                    let same = job.receipt_digest == Some(blake2_256(&receipt.encode()))
                        && UsedReceipts::<T>::get(receipt.receipt_id) == Some(job_id);
                    ensure!(same, Error::<T>::AlreadyTerminal);
                    return Ok(());
                }
                AiJobState::Reserved => return Err(Error::<T>::NotCommitted.into()),
                AiJobState::Cancelled | AiJobState::Refunded => {
                    return Err(Error::<T>::AlreadyTerminal.into())
                }
            }
            let now = frame_system::Pallet::<T>::block_number();
            let commit_block = job.commit_block.ok_or(Error::<T>::NotCommitted)?;
            ensure!(
                now < commit_block.saturating_add(T::SettleWindow::get()),
                Error::<T>::SettleWindowClosed
            );
            ensure!(
                receipt.terms_digest == blake2_256(&job.terms.encode()),
                Error::<T>::TermsMismatch
            );
            ensure!(
                Some(receipt.result_digest) == job.commit,
                Error::<T>::ResultMismatch
            );
            ensure!(receipt.receipt_id != [0u8; 32], Error::<T>::InvalidDigest);
            ensure!(
                !UsedReceipts::<T>::contains_key(receipt.receipt_id),
                Error::<T>::ReceiptAlreadyUsed
            );
            let entry = AiRates::<T>::get(job.terms.rate_id).ok_or(Error::<T>::UnknownRate)?;
            let paid = Self::to_balance(Self::quote(
                &entry.rate,
                receipt.input_tokens,
                receipt.cached_tokens,
                receipt.output_tokens,
            )?)?;
            ensure!(paid <= job.max_amount, Error::<T>::UsageAboveCap);
            let reward = T::RewardAccount::get();
            let remainder = T::Currency::repatriate_reserved(
                &reward,
                &job.terms.beneficiary,
                paid,
                BalanceStatus::Free,
            )?;
            ensure!(remainder.is_zero(), Error::<T>::IncompletePayment);
            let unused = job.max_amount.checked_sub(&paid).ok_or(Error::<T>::Overflow)?;
            ensure!(
                T::Currency::unreserve(&reward, unused).is_zero(),
                Error::<T>::IncompletePayment
            );
            UsedReceipts::<T>::insert(receipt.receipt_id, job_id);
            AiReserveTotals::<T>::try_mutate(|t| -> DispatchResult {
                t.reserved = t
                    .reserved
                    .checked_sub(&job.max_amount)
                    .ok_or(Error::<T>::Overflow)?;
                t.paid = t.paid.checked_add(&paid).ok_or(Error::<T>::Overflow)?;
                t.refunded = t.refunded.checked_add(&unused).ok_or(Error::<T>::Overflow)?;
                Ok(())
            })?;
            OpenJobOf::<T>::remove(&job.terms.worker);
            let beneficiary = job.terms.beneficiary.clone();
            job.state = AiJobState::Settled;
            job.paid = paid;
            job.receipt_id = Some(receipt.receipt_id);
            job.receipt_digest = Some(blake2_256(&receipt.encode()));
            AiJobs::<T>::insert(job_id, job);
            Self::deposit_event(Event::<T>::AiJobSettled {
                job_id,
                beneficiary,
                paid,
                refunded: unused,
                receipt_id: receipt.receipt_id,
            });
            Ok(())
        }

        /// 15: release an uncommitted reservation in full.
        /// Scheduler at any time; any signed account once `now > expires_at` (aligned S2 rule).
        /// Replaying on an already cancelled job is recovered.
        #[pallet::call_index(15)]
        #[pallet::weight(placeholder_weight::<T>(4, 3))]
        #[transactional]
        pub fn cancel_uncommitted_ai_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            let job = AiJobs::<T>::get(job_id).ok_or(Error::<T>::UnknownJob)?;
            let now = frame_system::Pallet::<T>::block_number();
            if T::SchedulerOrigin::try_origin(origin.clone()).is_err() {
                ensure_signed(origin)?;
                ensure!(now > job.terms.expires_at, DispatchError::BadOrigin);
            }
            match job.state {
                AiJobState::Reserved => {}
                AiJobState::Cancelled => return Ok(()),
                AiJobState::Committed => return Err(Error::<T>::NotReserved.into()),
                AiJobState::Settled | AiJobState::Refunded => {
                    return Err(Error::<T>::AlreadyTerminal.into())
                }
            }
            Self::release_full(job_id, job, AiJobState::Cancelled)?;
            Ok(())
        }

        /// 16 (T-B, added by the S4 assignment): any signed account may return the full
        /// reservation of a Committed job once `now >= commit_block + SettleWindow`.
        /// paid = 0, state Refunded, no receipt consumed, no Disputed state.
        #[pallet::call_index(16)]
        #[pallet::weight(placeholder_weight::<T>(4, 3))]
        #[transactional]
        pub fn refund_timed_out_ai_job(origin: OriginFor<T>, job_id: u64) -> DispatchResult {
            ensure_signed(origin)?;
            let job = AiJobs::<T>::get(job_id).ok_or(Error::<T>::UnknownJob)?;
            match job.state {
                AiJobState::Committed => {}
                AiJobState::Reserved => return Err(Error::<T>::NotCommitted.into()),
                AiJobState::Settled | AiJobState::Cancelled | AiJobState::Refunded => {
                    return Err(Error::<T>::AlreadyTerminal.into())
                }
            }
            let commit_block = job.commit_block.ok_or(Error::<T>::NotCommitted)?;
            let now = frame_system::Pallet::<T>::block_number();
            ensure!(
                now >= commit_block.saturating_add(T::SettleWindow::get()),
                Error::<T>::NotTimedOut
            );
            Self::release_full(job_id, job, AiJobState::Refunded)?;
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        /// v1 quote: floor once per job; cached billing forbidden.
        pub fn quote(
            rate: &TokenRate,
            input: u32,
            cached: u32,
            output: u32,
        ) -> Result<u128, Error<T>> {
            ensure!(cached <= input, Error::<T>::CachedExceedsInput);
            ensure!(
                rate.cached_per_million == 0 && cached == 0,
                Error::<T>::CachedNotAllowed
            );
            ensure!(
                input <= rate.max_input_tokens && output <= rate.max_output_tokens,
                Error::<T>::UsageAboveCap
            );
            let a = ((input - cached) as u128)
                .checked_mul(rate.input_per_million)
                .ok_or(Error::<T>::Overflow)?;
            let b = (output as u128)
                .checked_mul(rate.output_per_million)
                .ok_or(Error::<T>::Overflow)?;
            Ok(a.checked_add(b).ok_or(Error::<T>::Overflow)? / TOKENS_PER_PRICE_UNIT)
        }

        /// This chain's `block_hash(0)` as 32 bytes (zero if unavailable).
        pub fn genesis_hash() -> [u8; 32] {
            let hash = frame_system::Pallet::<T>::block_hash(BlockNumberFor::<T>::zero());
            let bytes: &[u8] = hash.as_ref();
            let mut out = [0u8; 32];
            if bytes.len() == 32 {
                out.copy_from_slice(bytes);
            }
            out
        }

        pub fn terms_digest(terms: &TermsOf<T>) -> [u8; 32] {
            blake2_256(&terms.encode())
        }

        fn is_role(who: &T::AccountId) -> bool {
            Schedulers::<T>::get().contains(who) || Attestors::<T>::get().contains(who)
        }

        fn to_balance(amount: u128) -> Result<BalanceOf<T>, Error<T>> {
            BalanceOf::<T>::try_from(amount).map_err(|_| Error::<T>::Overflow)
        }

        /// Full unreserve for cancel (15) and T-B refund (16). No receipt is consumed.
        fn release_full(job_id: u64, mut job: AiJobOf<T>, state: AiJobState) -> DispatchResult {
            let reward = T::RewardAccount::get();
            ensure!(
                T::Currency::unreserve(&reward, job.max_amount).is_zero(),
                Error::<T>::IncompletePayment
            );
            AiReserveTotals::<T>::try_mutate(|t| -> DispatchResult {
                t.reserved = t
                    .reserved
                    .checked_sub(&job.max_amount)
                    .ok_or(Error::<T>::Overflow)?;
                t.refunded = t
                    .refunded
                    .checked_add(&job.max_amount)
                    .ok_or(Error::<T>::Overflow)?;
                Ok(())
            })?;
            OpenJobOf::<T>::remove(&job.terms.worker);
            let refunded = job.max_amount;
            job.state = state;
            job.paid = Zero::zero();
            AiJobs::<T>::insert(job_id, job);
            match state {
                AiJobState::Refunded => {
                    Self::deposit_event(Event::<T>::AiJobRefunded { job_id, refunded })
                }
                _ => Self::deposit_event(Event::<T>::AiJobCancelled { job_id, refunded }),
            }
            Ok(())
        }

        fn sorted_role_set(
            members: &[T::AccountId],
            reward: &T::AccountId,
            label: &str,
        ) -> Vec<T::AccountId> {
            let mut set: Vec<T::AccountId> = members.to_vec();
            set.sort();
            let before = set.len();
            set.dedup();
            assert!(set.len() == before, "genesis: duplicate {} entry", label);
            assert!(
                set.len() <= MAX_ROLE_MEMBERS as usize,
                "genesis: too many {} entries",
                label
            );
            assert!(
                !set.contains(reward),
                "genesis: {} set must not contain RewardAccount",
                label
            );
            set
        }
    }

    /// `SortedMembers` adapter over `Schedulers` (sorted at genesis) for `EnsureSignedBy`.
    pub struct SchedulerMembers<T>(core::marker::PhantomData<T>);
    impl<T: Config> SortedMembers<T::AccountId> for SchedulerMembers<T> {
        fn sorted_members() -> Vec<T::AccountId> {
            Schedulers::<T>::get().into_inner()
        }
    }

    /// `SortedMembers` adapter over `Attestors` (sorted at genesis) for `EnsureSignedBy`.
    pub struct AttestorMembers<T>(core::marker::PhantomData<T>);
    impl<T: Config> SortedMembers<T::AccountId> for AttestorMembers<T> {
        fn sorted_members() -> Vec<T::AccountId> {
            Attestors::<T>::get().into_inner()
        }
    }
}
