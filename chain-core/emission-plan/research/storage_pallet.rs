//! Isolated storage-backed issuance candidate. Not linked into the live Runtime.
//! No dispatchable calls or autonomous hook. A future consensus adapter must
//! authenticate the author before calling issue_for_author. Root/internal currency
//! bypass and untrusted state imports are outside this prototype's guarantees.
pub use pallet::*;
#[frame_support::pallet]
pub mod pallet {
    use codec::DecodeAll;
    use eagle_emission_plan::{
        advance_stored_reward, quote_block, BLOCKS_PER_PERIOD, CAP, INITIAL_REWARD,
    };
    use frame_support::{
        pallet_prelude::*,
        traits::{Currency, Imbalance},
        transactional,
    };
    #[derive(
        Clone, Encode, Decode, DecodeWithMemTracking, TypeInfo, MaxEncodedLen, Debug, PartialEq, Eq,
    )]
    pub struct StateData {
        pub version: u16,
        pub last: u64,
        pub genesis: u128,
        pub cumulative: u128,
        pub base: u128,
        pub ai: u128,
        pub period: u32,
        pub reward: u128,
    }
    #[derive(
        Clone, Encode, Decode, DecodeWithMemTracking, TypeInfo, MaxEncodedLen, Debug, PartialEq, Eq,
    )]
    pub struct AcceptanceData {
        pub version: u16,
        pub height: u32,
        pub header: [u8; 108],
        pub work: [u8; 32],
        pub policy: [u8; 32],
    }
    #[pallet::config]
    pub trait Config: frame_system::Config<RuntimeEvent: From<Event<Self>>> {
        type Currency: Currency<Self::AccountId, Balance = u128>;
        #[pallet::constant]
        type AiPool: Get<Self::AccountId>;
    }
    const STORAGE_VERSION: StorageVersion = StorageVersion::new(2);
    #[pallet::pallet]
    #[pallet::storage_version(STORAGE_VERSION)]
    pub struct Pallet<T>(_);
    #[pallet::storage]
    #[pallet::getter(fn state)]
    pub type Ledger<T: Config> = StorageValue<_, StateData, OptionQuery>;
    #[pallet::storage]
    #[pallet::getter(fn acceptance)]
    pub type Acceptance<T: Config> = StorageValue<_, AcceptanceData, OptionQuery>;
    #[pallet::genesis_config]
    pub struct GenesisConfig<T: Config> {
        pub genesis_issued: u128,
        pub _marker: PhantomData<T>,
    }
    impl<T: Config> Default for GenesisConfig<T> {
        fn default() -> Self {
            Self {
                genesis_issued: 0,
                _marker: PhantomData,
            }
        }
    }
    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            assert!(self.genesis_issued <= CAP, "genesis exceeds cumulative cap");
            assert_eq!(
                T::Currency::total_issuance(),
                self.genesis_issued,
                "genesis currency must precede issuance ledger and match"
            );
            Ledger::<T>::put(StateData {
                version: 1,
                last: 0,
                genesis: self.genesis_issued,
                cumulative: self.genesis_issued,
                base: 0,
                ai: 0,
                period: 0,
                reward: INITIAL_REWARD,
            });
        }
    }
    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        Issued {
            height: u64,
            author: T::AccountId,
            base: u128,
            ai: u128,
        },
    }
    #[pallet::error]
    pub enum Error<T> {
        Missing,
        Height,
        State,
        UntrackedIssuance,
        Reward,
        Credit,
    }
    impl<T: Config> Pallet<T> {
        pub fn read_acceptance() -> Result<AcceptanceData, DispatchError> {
            let raw = frame_support::storage::unhashed::get_raw(&Acceptance::<T>::hashed_key())
                .ok_or(Error::<T>::Missing)?;
            ensure!(
                raw.len() == AcceptanceData::max_encoded_len(),
                Error::<T>::State
            );
            let a = AcceptanceData::decode_all(&mut &raw[..]).map_err(|_| Error::<T>::State)?;
            ensure!(
                a.version == 1 && a.policy != [0; 32] && a.work != [0; 32],
                Error::<T>::State
            );
            Ok(a)
        }
        /// Trusted bootstrap only; no external call, replacement or reset path.
        pub fn initialize_acceptance(anchor: AcceptanceData) -> DispatchResult {
            ensure!(!Acceptance::<T>::exists(), Error::<T>::State);
            ensure!(
                anchor.version == 1
                    && anchor.height == 0
                    && anchor.policy != [0; 32]
                    && anchor.work != [0; 32],
                Error::<T>::State
            );
            ensure!(
                Ledger::<T>::get().ok_or(Error::<T>::Missing)?.last == 0,
                Error::<T>::Height
            );
            Acceptance::<T>::put(anchor);
            Ok(())
        }
        /// Host/consensus verification must precede this trusted Runtime entry. It
        /// checks the exact prior record and atomically writes acceptance + issuance.
        #[transactional]
        pub fn issue_accepted(
            expected: &AcceptanceData,
            next: AcceptanceData,
            author: &T::AccountId,
        ) -> DispatchResult {
            ensure!(&Self::read_acceptance()? == expected, Error::<T>::State);
            ensure!(
                next.version == 1
                    && next.policy == expected.policy
                    && expected.height.checked_add(1) == Some(next.height),
                Error::<T>::State
            );
            ensure!(
                u64::from(next.height)
                    == Ledger::<T>::get()
                        .ok_or(Error::<T>::Missing)?
                        .last
                        .checked_add(1)
                        .ok_or(Error::<T>::Height)?,
                Error::<T>::Height
            );
            let id = sp_io::hashing::sha2_256(&sp_io::hashing::sha2_256(&expected.header));
            ensure!(
                next.header[4..36] == id
                    && next
                        .work
                        .iter()
                        .rev()
                        .cmp(expected.work.iter().rev())
                        .is_gt(),
                Error::<T>::State
            );
            Acceptance::<T>::put(next);
            Self::issue_inner(author)
        }
        fn deposit_exact(account: &T::AccountId, amount: u128) -> DispatchResult {
            let credit = T::Currency::deposit_creating(account, amount);
            let got = credit.peek();
            drop(credit);
            ensure!(got == amount, Error::<T>::Credit);
            Ok(())
        }
        /// Trusted Runtime entry only; no extrinsic or Root override is provided.
        /// Bounded arithmetic and storage access, with both credits/counters/events in
        /// one FRAME transaction. Cached reward correctness follows trusted history.
        #[transactional]
        pub fn issue_for_author(author: &T::AccountId) -> DispatchResult {
            // Presence, not successful decoding: malformed acceptance must fail closed.
            ensure!(!Acceptance::<T>::exists(), Error::<T>::State);
            Self::issue_inner(author)
        }
        // Only the legacy pre-acceptance path or checked joint commit may reach this.
        #[transactional]
        fn issue_inner(author: &T::AccountId) -> DispatchResult {
            let height: u64 = frame_system::Pallet::<T>::block_number()
                .try_into()
                .map_err(|_| Error::<T>::Height)?;
            let raw = frame_support::storage::unhashed::get_raw(&Ledger::<T>::hashed_key())
                .ok_or(Error::<T>::Missing)?;
            ensure!(raw.len() == StateData::max_encoded_len(), Error::<T>::State);
            let mut s = StateData::decode_all(&mut &raw[..]).map_err(|_| Error::<T>::State)?;
            ensure!(
                s.version == 1
                    && s.cumulative <= CAP
                    && s.reward <= INITIAL_REWARD
                    && s.genesis
                        .checked_add(s.base)
                        .and_then(|v| v.checked_add(s.ai))
                        == Some(s.cumulative),
                Error::<T>::State
            );
            ensure!(s.last.checked_add(1) == Some(height), Error::<T>::Height);
            let expected_cache = if s.last == 0 {
                0
            } else {
                u32::try_from((s.last - 1) as u128 / BLOCKS_PER_PERIOD)
                    .map_err(|_| Error::<T>::State)?
            };
            ensure!(s.period == expected_cache, Error::<T>::State);
            ensure!(
                T::Currency::total_issuance() <= s.cumulative,
                Error::<T>::UntrackedIssuance
            );
            let period = u32::try_from((height - 1) as u128 / BLOCKS_PER_PERIOD)
                .map_err(|_| Error::<T>::Reward)?;
            ensure!(period < 10000, Error::<T>::Reward);
            if period != s.period {
                ensure!(s.period.checked_add(1) == Some(period), Error::<T>::State);
                s.reward =
                    advance_stored_reward(s.reward, s.period).map_err(|_| Error::<T>::Reward)?;
                s.period = period;
            }
            let split = quote_block(s.reward, s.cumulative).map_err(|_| Error::<T>::Reward)?;
            s.last = height;
            s.cumulative = s
                .cumulative
                .checked_add(split.issued)
                .ok_or(Error::<T>::State)?;
            s.base = s.base.checked_add(split.base).ok_or(Error::<T>::State)?;
            s.ai = s.ai.checked_add(split.ai).ok_or(Error::<T>::State)?;
            Self::deposit_exact(author, split.base)?;
            Self::deposit_exact(&T::AiPool::get(), split.ai)?;
            Ledger::<T>::put(s);
            Self::deposit_event(Event::Issued {
                height,
                author: author.clone(),
                base: split.base,
                ai: split.ai,
            });
            Ok(())
        }
    }
}
