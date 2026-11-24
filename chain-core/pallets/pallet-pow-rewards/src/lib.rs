#![cfg_attr(not(feature = "std"), no_std)]

//! Consensus-owned PoW reward accounting and credit primitive.
//!
//! This pallet deliberately has no dispatchable mint call. A runtime-provided
//! source must authenticate the beneficiary already committed to the block's
//! PoW pre-runtime digest, and a runtime-provided schedule must quote issuance.
//! The pallet owns the once-per-height ledger, lifetime cap and exact cumulative
//! 70/30 split. It is not wired into the Eagle runtime yet.

extern crate alloc;

#[cfg_attr(feature = "svy-live-split", path = "semiannual_schedule.rs")]
pub mod block_height_schedule;
pub mod gregorian;
pub mod weights;
pub use pallet::*;

pub const AI_PERCENT: u128 = if cfg!(feature = "svy-live-split") { 80 } else { 30 };

#[cfg(feature = "runtime-benchmarks")]
mod benchmarking;

/// Weight reserved during `on_initialize` for mandatory reward finalization.
pub trait WeightInfo {
    fn issue_current_block() -> frame_support::weights::Weight;
}

pub struct RewardContext<AccountId> {
    pub beneficiary: AccountId,
    pub timestamp_seconds: u64,
}

#[cfg(test)]
mod tests {
    use super::*;
    use frame_support::{derive_impl, parameter_types, traits::ConstU128};
    use sp_runtime::{BuildStorage, DispatchError};

    type Block = frame_system::mocking::MockBlock<Test>;

    frame_support::construct_runtime!(
        pub enum Test {
            System: frame_system,
            Balances: pallet_balances,
            PowRewards: crate,
            AiRouter: pallet_ai_router,
        }
    );

    #[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
    impl frame_system::Config for Test {
        type Block = Block;
        type AccountId = u64;
        type AccountData = pallet_balances::AccountData<u128>;
    }

    #[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
    impl pallet_balances::Config for Test {
        type AccountStore = System;
        type Balance = u128;
        type ExistentialDeposit = ConstU128<1>;
    }

    parameter_types! {
        pub const AiPool: u64 = 99;
        pub const LifetimeCap: u128 = 1_000;
    }

    pub struct RouterWeights;
    impl pallet_ai_router::WeightInfo for RouterWeights {
        fn report() -> frame_support::weights::Weight { Default::default() }
        fn route(_: u32) -> frame_support::weights::Weight { Default::default() }
        fn settle() -> frame_support::weights::Weight { Default::default() }
    }

    #[cfg(feature = "svy-live-split")]
    pub struct TaskIssuance;
    #[cfg(feature = "svy-live-split")]
    impl pallet_ai_router::TaskFunding<Test> for TaskIssuance {
        fn reserve(maximum: u128) -> frame_support::dispatch::DispatchResult {
            PowRewards::reserve_ai_budget(maximum)
        }
        fn finish(miner: &u64, maximum: u128, actual: u128) -> frame_support::dispatch::DispatchResult {
            PowRewards::finish_ai_budget(miner, maximum, actual)
        }
    }

    impl pallet_ai_router::Config for Test {
        type Currency = Balances;
        #[cfg(feature = "svy-live-split")]
        type Funding = TaskIssuance;
        #[cfg(not(feature = "svy-live-split"))]
        type Funding = pallet_ai_router::Prefunded;
        type DevFundingEnabled = frame_support::traits::ConstBool<false>;
        type RequireDeviceApproval = frame_support::traits::ConstBool<true>;
        type RewardAccount = AiPool;
        type TaskReward = ConstU128<10>;
        type TaskOrigin = frame_system::EnsureRoot<u64>;
        type MaxNodes = frame_support::traits::ConstU32<5>;
        type HeartbeatTtl = frame_support::traits::ConstU64<5>;
        type WeightInfo = RouterWeights;
    }

    #[cfg(feature = "svy-live-split")]
    #[test]
    fn ai_is_issued_only_on_acceptance_within_committed_budget() {
        use frame_support::{assert_noop, assert_ok};
        external(100).execute_with(|| {
            System::set_block_number(1);
            assert_ok!(PowRewards::issue_current_block());
            assert_eq!(Balances::total_issuance(), 131);
            assert_eq!(Balances::free_balance(7), 31);
            assert_eq!(Balances::free_balance(99), 0);
            let device = frame_support::BoundedVec::try_from(vec![1]).unwrap();
            let worker = RuntimeOrigin::signed(8);
            let admin = RuntimeOrigin::signed(3);
            pallet_ai_router::AiOperator::<Test>::put(3);
            assert_noop!(AiRouter::register_node(worker.clone(), 16, device.clone()),
                pallet_ai_router::Error::<Test>::DeviceNotApproved);
            assert_ok!(AiRouter::set_device_approval(admin.clone(), 8, Some(device.clone())));
            assert_ok!(AiRouter::register_node(worker.clone(), 16, device));
            assert_ok!(AiRouter::heartbeat(worker.clone(), 16, true));
            assert_noop!(AiRouter::submit_ai_task_with_reward(admin.clone(), 1, 8, 71), Error::<Test>::Cap);
            assert_ok!(AiRouter::submit_ai_task_with_reward(admin.clone(), 1, 8, 60));
            assert_ok!(AiRouter::settle_task(admin.clone(), 0));
            assert_eq!(AiCommitted::<Test>::get(), 0);
            assert_ok!(AiRouter::heartbeat(worker.clone(), 16, true));
            assert_ok!(AiRouter::submit_ai_task_with_reward(admin.clone(), 1, 8, 60));
            assert_ok!(AiRouter::submit_result(worker, 1, [7; 32]));
            assert_noop!(AiRouter::verify_result(admin.clone(), 1, [8; 32]), pallet_ai_router::Error::<Test>::NoResult);
            assert_eq!(Balances::total_issuance(), 131);
            assert_ok!(AiRouter::verify_result_with_reward(admin.clone(), 1, [7; 32], 24));
            assert_noop!(AiRouter::verify_result(admin, 1, [7; 32]), pallet_ai_router::Error::<Test>::NoResult);
            assert_eq!(AiCommitted::<Test>::get(), 0);
            assert_eq!(Balances::free_balance(8), 24);
            System::set_block_number(2);
            assert_ok!(PowRewards::issue_current_block());
            let ledger = PowRewards::ledger().unwrap();
            assert_eq!((ledger.scheduled_issued, ledger.base_issued, ledger.ai_issued), (202, 61, 24));
            assert_eq!(Balances::total_issuance(), 185);
            System::assert_last_event(Event::AiBudgetRecorded {
                height: 2, added: 71, cumulative: 141, issued: 24, committed: 0, available: 117,
            }.into());
            assert_ok!(AiRouter::heartbeat(RuntimeOrigin::signed(8), 16, true));
            assert_ok!(AiRouter::submit_ai_task_with_reward(RuntimeOrigin::signed(3), 1, 8, 97));
            System::set_block_number(3);
            assert_ok!(PowRewards::issue_current_block());
            System::assert_last_event(Event::AiBudgetRecorded {
                height: 3, added: 71, cumulative: 212, issued: 24, committed: 97, available: 91,
            }.into());
        });
    }

    pub struct Source;
    impl RewardSource<Test> for Source {
        fn context() -> Result<RewardContext<u64>, DispatchError> {
            Ok(RewardContext {
                beneficiary: 7,
                timestamp_seconds: System::block_number() * 10,
            })
        }
    }

    pub struct Schedule;
    impl EmissionSchedule for Schedule {
        fn quote(
            height: u64,
            _timestamp_seconds: u64,
            _last_timestamp_seconds: u64,
            _scheduled_issued: u128,
            _genesis_issued: u128,
        ) -> Result<u128, DispatchError> {
            Ok(match height {
                10 => 1,
                11 => 0,
                _ => 101,
            })
        }
    }

    impl Config for Test {
        type Currency = Balances;
        type AiPool = AiPool;
        type LifetimeCap = LifetimeCap;
        type Source = Source;
        type Schedule = Schedule;
        type WeightInfo = ();
        type BenchmarkHelper = ();
    }

    fn external(genesis: u128) -> sp_io::TestExternalities {
        let mut storage = frame_system::GenesisConfig::<Test>::default()
            .build_storage()
            .unwrap();
        pallet_balances::GenesisConfig::<Test> {
            balances: alloc::vec![(1, genesis)],
            ..Default::default()
        }
        .assimilate_storage(&mut storage)
        .unwrap();
        GenesisConfig::<Test> {
            genesis_issued: genesis,
            initial_timestamp_seconds: 0,
            _marker: Default::default(),
        }
        .assimilate_storage(&mut storage)
        .unwrap();
        storage.into()
    }

    #[test]
    fn one_atom_deposit_allows_500_persistent_accounts_in_mock_state() {
        external(999).execute_with(|| {
            System::set_block_number(1);
            let before = frame_system::Account::<Test>::iter().count();
            for account in 1_000u64..1_500 {
                assert!(
                    Balances::transfer_keep_alive(RuntimeOrigin::signed(1), account, 1).is_ok()
                );
                assert_eq!(Balances::free_balance(account), 1);
            }
            let after = frame_system::Account::<Test>::iter().count();
            assert_eq!(after - before, 500);
            assert_eq!(Balances::free_balance(1), 499);
            assert_eq!(Balances::total_issuance(), 999);
            assert_eq!(PowRewards::ledger().unwrap().lifetime_issued, 999);
        });
    }

    #[test]
    fn one_atom_to_new_miner_and_zero_reward_keep_ledger_consistent() {
        external(100).execute_with(|| {
            Ledger::<Test>::mutate(|ledger| ledger.as_mut().unwrap().last_height = 9);
            System::set_block_number(10);
            assert_eq!(Balances::free_balance(7), 0);
            assert!(PowRewards::issue_current_block().is_ok());
            assert_eq!(Balances::free_balance(7), 1);
            assert_eq!(Balances::free_balance(99), 0);
            assert_eq!(Balances::total_issuance(), 101);
            System::set_block_number(11);
            assert!(PowRewards::issue_current_block().is_ok());
            let ledger = PowRewards::ledger().unwrap();
            assert_eq!(ledger.last_height, 11);
            assert_eq!(ledger.scheduled_issued, 1);
            assert_eq!(ledger.base_issued, 1);
            assert_eq!(ledger.ai_issued, 0);
            assert_eq!(Balances::total_issuance(), 101);
        });
    }

    #[cfg(not(feature = "svy-live-split"))]
    #[test]
    fn credits_once_per_height_with_exact_cumulative_split() {
        external(100).execute_with(|| {
            System::set_block_number(1);
            assert!(PowRewards::issue_current_block().is_ok());
            assert_eq!(Balances::free_balance(7), 101 - 101 * crate::AI_PERCENT / 100);
            assert_eq!(Balances::free_balance(99), 101 * crate::AI_PERCENT / 100);
            assert_eq!(Balances::total_issuance(), 201);
            assert!(PowRewards::issue_current_block().is_err());
            System::set_block_number(2);
            assert!(PowRewards::issue_current_block().is_ok());
            assert_eq!(Balances::free_balance(7), 202 - 202 * crate::AI_PERCENT / 100);
            assert_eq!(Balances::free_balance(99), 202 * crate::AI_PERCENT / 100);
            let ledger = PowRewards::ledger().unwrap();
            assert_eq!(ledger.scheduled_issued, 202);
            assert_eq!(ledger.base_issued + ledger.ai_issued, 202);
            assert_eq!(ledger.ai_issued, 202 * crate::AI_PERCENT / 100);
        });
    }

    #[test]
    fn cap_failure_rolls_back_all_currency_and_ledger_changes() {
        external(950).execute_with(|| {
            System::set_block_number(1);
            let before = PowRewards::ledger().unwrap();
            assert!(PowRewards::issue_current_block().is_err());
            assert_eq!(PowRewards::ledger().unwrap(), before);
            assert_eq!(Balances::total_issuance(), 950);
            assert_eq!(Balances::free_balance(7), 0);
            assert_eq!(Balances::free_balance(99), 0);
        });
    }

    #[test]
    fn initialize_reserves_nonzero_finalize_weight() {
        use frame_support::traits::Hooks;

        external(100).execute_with(|| {
            let reserved = <PowRewards as Hooks<u64>>::on_initialize(1);
            assert_ne!(reserved, frame_support::weights::Weight::zero());
            assert_eq!(reserved, <() as crate::WeightInfo>::issue_current_block());
        });
    }

    #[test]
    fn rejects_self_consistent_ledger_with_wrong_cumulative_split() {
        external(100).execute_with(|| {
            let mut corrupted = PowRewards::ledger().unwrap();
            corrupted.last_height = 1;
            corrupted.lifetime_issued = 201;
            corrupted.scheduled_issued = 101;
            corrupted.base_issued = 70;
            corrupted.ai_issued = 31;
            crate::pallet::Ledger::<Test>::put(&corrupted);
            System::set_block_number(2);

            assert!(PowRewards::issue_current_block().is_err());
            assert_eq!(PowRewards::ledger().unwrap(), corrupted);
            assert_eq!(Balances::total_issuance(), 100);
            assert_eq!(Balances::free_balance(7), 0);
            assert_eq!(Balances::free_balance(99), 0);
        });
    }

    #[test]
    fn rejects_untracked_burn_before_issuance() {
        external(100).execute_with(|| {
            assert!(Balances::burn(RuntimeOrigin::signed(1), 10, false).is_ok());
            assert_eq!(Balances::total_issuance(), 90);
            let before = PowRewards::ledger().unwrap();
            System::set_block_number(1);

            assert!(PowRewards::issue_current_block().is_err());
            assert_eq!(PowRewards::ledger().unwrap(), before);
            assert_eq!(Balances::total_issuance(), 90);
            assert_eq!(Balances::free_balance(7), 0);
            assert_eq!(Balances::free_balance(99), 0);
        });
    }

    #[test]
    fn tracked_destruction_does_not_reopen_lifetime_cap() {
        external(100).execute_with(|| {
            assert!(Balances::burn(RuntimeOrigin::signed(1), 10, false).is_ok());
            PowRewards::record_destroyed(10);
            System::set_block_number(1);

            assert!(PowRewards::issue_current_block().is_ok());
            let ledger = PowRewards::ledger().unwrap();
            assert_eq!(ledger.genesis_issued, 100);
            assert_eq!(ledger.scheduled_issued, 101);
            assert_eq!(ledger.lifetime_issued, if cfg!(feature = "svy-live-split") { 131 } else { 201 });
            assert_eq!(ledger.destroyed_issued, 10);
            assert_eq!(Balances::total_issuance(), ledger.lifetime_issued - 10);
            assert_eq!(
                Balances::total_issuance() + ledger.destroyed_issued,
                ledger.lifetime_issued
            );
        });
    }
}

pub trait RewardSource<T: Config> {
    fn context() -> Result<RewardContext<T::AccountId>, sp_runtime::DispatchError>;
}

pub trait EmissionSchedule {
    fn quote(
        height: u64,
        timestamp_seconds: u64,
        last_timestamp_seconds: u64,
        scheduled_issued: u128,
        genesis_issued: u128,
    ) -> Result<u128, sp_runtime::DispatchError>;
}

/// Runtime-specific setup required to benchmark the real reward source and
/// schedule without exposing a dispatchable mint call.
pub trait BenchmarkHelper<T: Config> {
    fn setup(beneficiary: T::AccountId) -> sp_runtime::DispatchResult;

    /// Synthetic high parent for measuring bounded height-schedule arithmetic.
    /// Only implemented by the isolated block-rewards benchmark runtime.
    fn setup_high_height(_: T::AccountId, _: u32) -> sp_runtime::DispatchResult {
        Err(sp_runtime::DispatchError::Other(
            "high-height reward benchmark is not configured",
        ))
    }

    /// Prepare the next valid reward block after both the beneficiary and the
    /// configured AI pool have already received a prior scheduled reward.
    fn setup_existing_accounts(_: T::AccountId) -> sp_runtime::DispatchResult {
        Err(sp_runtime::DispatchError::Other(
            "existing-account reward benchmark is not configured",
        ))
    }

    /// Prepare the next valid reward block with an existing beneficiary whose
    /// Balances account still uses the legacy account-reference logic. This is
    /// benchmark-only setup for the automatic `ensure_upgraded` branch reached
    /// by `deposit_creating`; production reward logic does not manufacture or
    /// migrate account state itself.
    fn setup_legacy_beneficiary(_: T::AccountId) -> sp_runtime::DispatchResult {
        Err(sp_runtime::DispatchError::Other(
            "legacy-beneficiary reward benchmark is not configured",
        ))
    }
}

impl<T: Config> BenchmarkHelper<T> for () {
    fn setup(_: T::AccountId) -> sp_runtime::DispatchResult {
        Err(sp_runtime::DispatchError::Other(
            "reward benchmark helper is not configured",
        ))
    }
}

#[frame_support::pallet]
pub mod pallet {
    use super::{EmissionSchedule, RewardSource, WeightInfo as _};
    use codec::DecodeAll;
    use frame_support::{
        pallet_prelude::*,
        traits::{Currency, Imbalance},
        transactional,
    };
    use frame_system::pallet_prelude::BlockNumberFor;

    #[derive(
        Clone, Encode, Decode, DecodeWithMemTracking, TypeInfo, MaxEncodedLen, Debug, PartialEq, Eq,
    )]
    pub struct LedgerState {
        pub version: u16,
        pub last_height: u64,
        pub last_timestamp_seconds: u64,
        pub genesis_issued: u128,
        pub lifetime_issued: u128,
        /// Supply destroyed after issuance. This never reopens lifetime cap
        /// headroom; it only reconciles circulating supply to gross issuance.
        pub destroyed_issued: u128,
        pub scheduled_issued: u128,
        pub base_issued: u128,
        pub ai_issued: u128,
    }

    #[pallet::config]
    pub trait Config: frame_system::Config<RuntimeEvent: From<Event<Self>>> {
        type Currency: Currency<Self::AccountId, Balance = u128>;
        #[pallet::constant]
        type AiPool: Get<Self::AccountId>;
        #[pallet::constant]
        type LifetimeCap: Get<u128>;
        type Source: RewardSource<Self>;
        type Schedule: EmissionSchedule;
        type WeightInfo: crate::WeightInfo;
        type BenchmarkHelper: crate::BenchmarkHelper<Self>;
    }

    const STORAGE_VERSION: StorageVersion = StorageVersion::new(2);

    #[pallet::pallet]
    #[pallet::storage_version(STORAGE_VERSION)]
    pub struct Pallet<T>(_);

    #[pallet::storage]
    #[pallet::getter(fn ledger)]
    pub type Ledger<T: Config> = StorageValue<_, LedgerState, OptionQuery>;

    /// Outstanding task commitments, denominated in unissued reward units.
    #[pallet::storage]
    pub type AiCommitted<T> = StorageValue<_, u128, ValueQuery>;

    /// Sticky fail-closed marker set when a configured destruction handler
    /// cannot append to the authenticated gross-destruction counter.
    #[pallet::storage]
    #[pallet::getter(fn burn_accounting_broken)]
    pub type BurnAccountingBroken<T: Config> = StorageValue<_, bool, ValueQuery>;

    #[pallet::genesis_config]
    pub struct GenesisConfig<T: Config> {
        pub genesis_issued: u128,
        pub initial_timestamp_seconds: u64,
        pub _marker: PhantomData<T>,
    }

    impl<T: Config> Default for GenesisConfig<T> {
        fn default() -> Self {
            Self {
                genesis_issued: 0,
                initial_timestamp_seconds: 0,
                _marker: PhantomData,
            }
        }
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            assert!(
                self.genesis_issued <= T::LifetimeCap::get(),
                "genesis exceeds cap"
            );
            assert_eq!(
                T::Currency::total_issuance(),
                self.genesis_issued,
                "currency genesis and reward ledger differ"
            );
            Ledger::<T>::put(LedgerState {
                version: 2,
                last_height: 0,
                last_timestamp_seconds: self.initial_timestamp_seconds,
                genesis_issued: self.genesis_issued,
                lifetime_issued: self.genesis_issued,
                destroyed_issued: 0,
                scheduled_issued: 0,
                base_issued: 0,
                ai_issued: 0,
            });
        }
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        Issued {
            height: u64,
            beneficiary: T::AccountId,
            base: u128,
            ai: u128,
            lifetime_issued: u128,
        },
        /// End-of-block AI budget in atomic units, including unissued carryover.
        AiBudgetRecorded {
            height: u64,
            added: u128,
            cumulative: u128,
            issued: u128,
            committed: u128,
            available: u128,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        MissingLedger,
        InvalidLedger,
        UnexpectedHeight,
        InvalidTimestamp,
        UntrackedIssuance,
        Schedule,
        Cap,
        Credit,
        BurnAccounting,
    }

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(_: BlockNumberFor<T>) -> Weight {
            T::WeightInfo::issue_current_block()
        }

        fn on_finalize(_: BlockNumberFor<T>) {
            Self::issue_current_block().expect("PoW reward issuance must succeed atomically");
        }
    }

    impl<T: Config> Pallet<T> {
        /// Record supply destruction observed by a runtime-configured,
        /// non-dispatchable handler. Any malformed/missing ledger or overflow
        /// sets a sticky marker so block finalization fails closed.
        pub fn record_destroyed(amount: u128) {
            if amount == 0 {
                return;
            }
            let recorded = Ledger::<T>::try_mutate(|maybe_state| -> DispatchResult {
                let state = maybe_state.as_mut().ok_or(Error::<T>::MissingLedger)?;
                ensure!(state.version == 2, Error::<T>::InvalidLedger);
                state.destroyed_issued = state
                    .destroyed_issued
                    .checked_add(amount)
                    .ok_or(Error::<T>::BurnAccounting)?;
                Ok(())
            });
            if recorded.is_err() {
                BurnAccountingBroken::<T>::put(true);
            }
        }

        fn exact_credit(account: &T::AccountId, amount: u128) -> DispatchResult {
            if amount == 0 {
                return Ok(());
            }
            let imbalance = T::Currency::deposit_creating(account, amount);
            let credited = imbalance.peek();
            drop(imbalance);
            ensure!(credited == amount, Error::<T>::Credit);
            Ok(())
        }

        #[cfg(feature = "svy-live-split")]
        pub fn reserve_ai_budget(amount: u128) -> DispatchResult {
            let state = Ledger::<T>::get().ok_or(Error::<T>::MissingLedger)?;
            let unissued = state.scheduled_issued.checked_sub(state.base_issued)
                .and_then(|budget| budget.checked_sub(state.ai_issued)).ok_or(Error::<T>::Cap)?;
            AiCommitted::<T>::try_mutate(|committed| {
                *committed = committed.checked_add(amount).filter(|next| *next <= unissued)
                    .ok_or(Error::<T>::Cap)?;
                Ok(())
            })
        }

        /// The router calls this inside the same transaction as task completion or cancellation.
        #[cfg(feature = "svy-live-split")]
        #[transactional]
        pub fn finish_ai_budget(miner: &T::AccountId, reserved: u128, actual: u128) -> DispatchResult {
            ensure!(actual <= reserved, Error::<T>::Cap);
            AiCommitted::<T>::try_mutate(|value| -> DispatchResult {
                *value = value.checked_sub(reserved).ok_or(Error::<T>::Cap)?;
                Ok(())
            })?;
            Ledger::<T>::try_mutate(|ledger| -> DispatchResult {
                let state = ledger.as_mut().ok_or(Error::<T>::MissingLedger)?;
                state.ai_issued = state.ai_issued.checked_add(actual).ok_or(Error::<T>::Cap)?;
                ensure!(state.base_issued.checked_add(state.ai_issued)
                    .is_some_and(|issued| issued <= state.scheduled_issued), Error::<T>::Cap);
                state.lifetime_issued = state.lifetime_issued.checked_add(actual)
                    .filter(|issued| *issued <= T::LifetimeCap::get()).ok_or(Error::<T>::Cap)?;
                Self::exact_credit(miner, actual)
            })
        }

        #[transactional]
        pub fn issue_current_block() -> DispatchResult {
            let raw = frame_support::storage::unhashed::get_raw(&Ledger::<T>::hashed_key())
                .ok_or(Error::<T>::MissingLedger)?;
            ensure!(
                raw.len() == LedgerState::max_encoded_len(),
                Error::<T>::InvalidLedger
            );
            let mut state =
                LedgerState::decode_all(&mut &raw[..]).map_err(|_| Error::<T>::InvalidLedger)?;
            let ai_budget = state.scheduled_issued.checked_mul(crate::AI_PERCENT)
                .ok_or(Error::<T>::Cap)? / 100;
            ensure!(
                state.version == 2
                    && state.genesis_issued.checked_add(state.base_issued)
                        .and_then(|base| base.checked_add(state.ai_issued)) == Some(state.lifetime_issued)
                    && state.base_issued == state.scheduled_issued - ai_budget
                    && state.ai_issued <= ai_budget
                    && (cfg!(feature = "svy-live-split") || state.ai_issued == ai_budget)
                    && state.lifetime_issued <= T::LifetimeCap::get(),
                Error::<T>::InvalidLedger
            );
            ensure!(
                !BurnAccountingBroken::<T>::get(),
                Error::<T>::BurnAccounting
            );
            // Lifetime issuance is gross and never reduced by destruction.
            // The runtime must route every permitted destruction path through
            // `record_destroyed`; any untracked mint or burn fails this check.
            ensure!(
                T::Currency::total_issuance().checked_add(state.destroyed_issued)
                    == Some(state.lifetime_issued),
                Error::<T>::UntrackedIssuance
            );
            let height: u64 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .map_err(|_| Error::<T>::UnexpectedHeight)?;
            ensure!(
                state.last_height.checked_add(1) == Some(height),
                Error::<T>::UnexpectedHeight
            );
            let context = T::Source::context()?;
            ensure!(
                context.timestamp_seconds >= state.last_timestamp_seconds,
                Error::<T>::InvalidTimestamp
            );
            let requested = T::Schedule::quote(
                height,
                context.timestamp_seconds,
                state.last_timestamp_seconds,
                state.scheduled_issued,
                state.genesis_issued,
            )
            .map_err(|_| Error::<T>::Schedule)?;
            let remaining = T::LifetimeCap::get()
                .checked_sub(state.genesis_issued)
                .and_then(|available| available.checked_sub(state.scheduled_issued))
                .ok_or(Error::<T>::Cap)?;
            ensure!(requested <= remaining, Error::<T>::Cap);
            let next_scheduled = state
                .scheduled_issued
                .checked_add(requested)
                .ok_or(Error::<T>::Cap)?;
            let next_ai = next_scheduled.checked_mul(crate::AI_PERCENT).ok_or(Error::<T>::Cap)? / 100;
            let added_budget = next_ai
                .checked_sub(ai_budget)
                .ok_or(Error::<T>::InvalidLedger)?;
            let base = requested.checked_sub(added_budget).ok_or(Error::<T>::InvalidLedger)?;
            Self::exact_credit(&context.beneficiary, base)?;
            let ai = if cfg!(feature = "svy-live-split") { 0 } else { added_budget };
            Self::exact_credit(&T::AiPool::get(), ai)?;
            state.last_height = height;
            state.last_timestamp_seconds = context.timestamp_seconds;
            state.lifetime_issued = state
                .lifetime_issued
                .checked_add(base + ai)
                .ok_or(Error::<T>::Cap)?;
            state.scheduled_issued = next_scheduled;
            state.base_issued = state.base_issued.checked_add(base).ok_or(Error::<T>::Cap)?;
            state.ai_issued += ai;
            Ledger::<T>::put(&state);
            Self::deposit_event(Event::Issued {
                height,
                beneficiary: context.beneficiary,
                base,
                ai,
                lifetime_issued: state.lifetime_issued,
            });
            #[cfg(feature = "svy-live-split")]
            {
                let committed = AiCommitted::<T>::get();
                let available = next_ai.checked_sub(state.ai_issued)
                    .and_then(|unused| unused.checked_sub(committed))
                    .ok_or(Error::<T>::InvalidLedger)?;
                Self::deposit_event(Event::AiBudgetRecorded {
                    height, added: added_budget, cumulative: next_ai, issued: state.ai_issued,
                    committed, available,
                });
            }
            Ok(())
        }
    }
}
