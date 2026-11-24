use crate::{incinerator_rehearsal::DEAD, AccountId, Balance, Balances, BalancesCall, Incinerator, RuntimeCall, RuntimeOrigin};
#[cfg(feature = "incinerator-rehearsal")]
use crate::incinerator_rehearsal::Event;
#[cfg(feature = "native-dead-mainnet-drill")]
use pallet::Event;
use codec::{Decode, DecodeWithMemTracking, Encode};
use frame_support::{
    dispatch::{DispatchInfo, PostDispatchInfo},
    traits::{
        fungible::Mutate,
        tokens::{Fortitude, Precision, Preservation},
    },
    weights::Weight,
};
use scale_info::TypeInfo;
use sp_runtime::{
    traits::{Implication, TransactionExtension},
    transaction_validity::{
        InvalidTransaction, TransactionSource, TransactionValidityError, ValidTransaction,
    },
    DispatchResult, MultiAddress,
};

#[derive(Encode, Decode, DecodeWithMemTracking, Clone, Eq, PartialEq, TypeInfo, Debug, Default)]
pub struct NativeIncinerator;

#[cfg(feature = "native-dead-mainnet-drill")]
impl pallet::Config for crate::Runtime {}

#[cfg(feature = "native-dead-mainnet-drill")]
#[frame_support::pallet]
pub mod pallet {
    use frame_support::pallet_prelude::*;

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    #[pallet::config]
    pub trait Config: frame_system::Config<RuntimeEvent: From<Event<Self>>> {}

    #[pallet::event]
    #[pallet::generate_deposit(pub(crate) fn deposit_event)]
    pub enum Event<T: Config> {
        NativeSvyBurned { from: T::AccountId, amount: u128 },
    }
}

fn transfers_to_dead(call: &RuntimeCall) -> bool {
    matches!(
        call,
        RuntimeCall::Balances(
            BalancesCall::transfer_keep_alive { dest: MultiAddress::Id(dest), .. }
            | BalancesCall::transfer_allow_death { dest: MultiAddress::Id(dest), .. }
            | BalancesCall::transfer_all { dest: MultiAddress::Id(dest), .. }
        ) if dest == &DEAD
    )
}

impl TransactionExtension<RuntimeCall> for NativeIncinerator {
    const IDENTIFIER: &'static str = "NativeIncinerator";
    type Implicit = ();
    type Val = Option<AccountId>;
    type Pre = Option<(AccountId, Balance)>;

    fn weight(&self, call: &RuntimeCall) -> Weight {
        if transfers_to_dead(call) {
            Weight::from_parts(100_000_000, 8_000)
        } else {
            Weight::zero()
        }
    }

    fn validate(
        &self,
        origin: RuntimeOrigin,
        call: &RuntimeCall,
        _: &DispatchInfo,
        _: usize,
        _: (),
        _: &impl Implication,
        _: TransactionSource,
    ) -> Result<(ValidTransaction, Self::Val, RuntimeOrigin), TransactionValidityError> {
        let signer = frame_system::ensure_signed(origin.clone()).ok();
        if signer.as_ref() == Some(&DEAD) {
            return Err(InvalidTransaction::Call.into());
        }
        Ok((
            ValidTransaction::default(),
            signer.filter(|_| transfers_to_dead(call)),
            origin,
        ))
    }

    fn prepare(
        self,
        signer: Self::Val,
        _: &RuntimeOrigin,
        _: &RuntimeCall,
        _: &DispatchInfo,
        _: usize,
    ) -> Result<Self::Pre, TransactionValidityError> {
        Ok(signer.map(|from| (from, Balances::free_balance(&DEAD))))
    }

    fn post_dispatch_details(
        pre: Self::Pre,
        _: &DispatchInfo,
        _: &PostDispatchInfo,
        _: usize,
        result: &DispatchResult,
    ) -> Result<Weight, TransactionValidityError> {
        if let (Some((from, before)), Ok(())) = (pre, result) {
            let amount = Balances::free_balance(&DEAD)
                .checked_sub(before)
                .ok_or(InvalidTransaction::Call)?;
            if amount > 0 {
                let burned = Balances::burn_from(
                    &DEAD,
                    amount,
                    Preservation::Expendable,
                    Precision::Exact,
                    Fortitude::Polite,
                )
                .map_err(|_| InvalidTransaction::Call)?;
                if burned != amount || Balances::free_balance(&DEAD) != before {
                    return Err(InvalidTransaction::Call.into());
                }
                #[cfg(feature = "native-dead-mainnet-drill")]
                {
                    crate::PowRewards::record_destroyed(burned);
                    if crate::PowRewards::burn_accounting_broken() {
                        return Err(InvalidTransaction::Call.into());
                    }
                }
                Incinerator::deposit_event(Event::NativeSvyBurned { from, amount });
            }
        }
        Ok(Weight::zero())
    }
}
