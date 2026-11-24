use frame_support::pallet_prelude::*;
use frame_system::pallet_prelude::*;
use sp_runtime::AccountId32;

pub const DEAD: AccountId32 = AccountId32::new([
    0x24, 0x5f, 0x7e, 0xd7, 0xba, 0x04, 0xb0, 0x4b, 0xa5, 0xbf, 0x75, 0xda, 0x4e, 0x8a, 0xf4, 0xc7,
    0x8b, 0x7f, 0x9b, 0xf6, 0xfc, 0x71, 0xc1, 0xe4, 0xeb, 0xf7, 0xf4, 0xa6, 0x98, 0x00, 0x00, 0x24,
]);

pub use pallet::*;

#[frame_support::pallet]
pub mod pallet {
    use super::*;

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    #[pallet::config]
    pub trait Config:
        frame_system::Config<AccountId = AccountId32, RuntimeEvent: From<Event<Self>>>
        + pallet_balances::Config<Balance = u128>
    {
    }

    /// True marks LP shares. Their effective issuance includes permanently retired shares.
    #[pallet::storage]
    pub type Assets<T> = StorageMap<_, Blake2_128Concat, u32, bool>;
    #[pallet::storage]
    pub type EffectiveIssuance<T> = StorageMap<_, Blake2_128Concat, u32, u128, ValueQuery>;
    #[pallet::storage]
    pub type Retired<T> = StorageMap<_, Blake2_128Concat, u32, u128, ValueQuery>;
    #[pallet::storage]
    pub type Holdings<T> =
        StorageDoubleMap<_, Blake2_128Concat, u32, Blake2_128Concat, AccountId32, u128, ValueQuery>;
    #[pallet::storage]
    pub type Approvals<T> = StorageDoubleMap<
        _,
        Blake2_128Concat,
        (u32, AccountId32),
        Blake2_128Concat,
        AccountId32,
        u128,
        ValueQuery,
    >;

    #[pallet::event]
    #[pallet::generate_deposit(pub(crate) fn deposit_event)]
    pub enum Event<T: Config> {
        AssetBurned {
            asset_id: u32,
            from: AccountId32,
            amount: u128,
        },
        LiquidityRetired {
            asset_id: u32,
            from: AccountId32,
            amount: u128,
        },
        NativeSvyBurned {
            from: AccountId32,
            amount: u128,
        },
        HistoricalNativeSvyBurned {
            amount: u128,
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        ReservedAccount,
        UnknownAsset,
        ExistingAsset,
        InvalidAmount,
        InsufficientBalance,
        InsufficientApproval,
        Arithmetic,
        HistoricalBalanceMismatch,
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        /// Fixture funding is restricted to the rehearsal chain's root.
        #[pallet::call_index(0)]
        #[pallet::weight(Weight::from_parts(50_000_000, 4_000))]
        pub fn create_fixture(
            origin: OriginFor<T>,
            asset_id: u32,
            liquidity: bool,
            owner: AccountId32,
            amount: u128,
        ) -> DispatchResult {
            ensure_root(origin)?;
            ensure!(owner != DEAD, Error::<T>::ReservedAccount);
            ensure!(amount > 0, Error::<T>::InvalidAmount);
            ensure!(
                !Assets::<T>::contains_key(asset_id),
                Error::<T>::ExistingAsset
            );
            Assets::<T>::insert(asset_id, liquidity);
            EffectiveIssuance::<T>::insert(asset_id, amount);
            Holdings::<T>::insert(asset_id, owner, amount);
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(Weight::from_parts(80_000_000, 8_000))]
        #[frame_support::transactional]
        pub fn transfer(
            origin: OriginFor<T>,
            asset_id: u32,
            to: AccountId32,
            amount: u128,
        ) -> DispatchResult {
            let from = ensure_signed(origin)?;
            Self::move_asset(asset_id, &from, &to, amount)
        }

        #[pallet::call_index(2)]
        #[pallet::weight(Weight::from_parts(50_000_000, 4_000))]
        pub fn approve(
            origin: OriginFor<T>,
            asset_id: u32,
            delegate: AccountId32,
            amount: u128,
        ) -> DispatchResult {
            let from = ensure_signed(origin)?;
            ensure!(
                from != DEAD && delegate != DEAD,
                Error::<T>::ReservedAccount
            );
            ensure!(
                Assets::<T>::contains_key(asset_id),
                Error::<T>::UnknownAsset
            );
            if amount == 0 {
                Approvals::<T>::remove((asset_id, from), delegate);
            } else {
                Approvals::<T>::insert((asset_id, from), delegate, amount);
            }
            Ok(())
        }

        #[pallet::call_index(3)]
        #[pallet::weight(Weight::from_parts(100_000_000, 10_000))]
        #[frame_support::transactional]
        pub fn transfer_approved(
            origin: OriginFor<T>,
            asset_id: u32,
            from: AccountId32,
            to: AccountId32,
            amount: u128,
        ) -> DispatchResult {
            let delegate = ensure_signed(origin)?;
            ensure!(
                from != DEAD && delegate != DEAD,
                Error::<T>::ReservedAccount
            );
            let remaining = Approvals::<T>::get((asset_id, &from), &delegate)
                .checked_sub(amount)
                .ok_or(Error::<T>::InsufficientApproval)?;
            Self::move_asset(asset_id, &from, &to, amount)?;
            if remaining == 0 {
                Approvals::<T>::remove((asset_id, from), delegate);
            } else {
                Approvals::<T>::insert((asset_id, from), delegate, remaining);
            }
            Ok(())
        }

        /// Review the exact spendable balance before destroying a legacy deposit.
        #[pallet::call_index(4)]
        #[pallet::weight(Weight::from_parts(120_000_000, 8_000))]
        #[frame_support::transactional]
        pub fn burn_historical_native(origin: OriginFor<T>, expected: u128) -> DispatchResult {
            ensure_root(origin)?;
            ensure!(
                expected > 0 && pallet_balances::Pallet::<T>::free_balance(&DEAD) == expected,
                Error::<T>::HistoricalBalanceMismatch
            );
            use frame_support::traits::{
                fungible::Mutate,
                tokens::{Fortitude, Precision, Preservation},
            };
            let burned = pallet_balances::Pallet::<T>::burn_from(
                &DEAD,
                expected,
                Preservation::Expendable,
                Precision::Exact,
                Fortitude::Polite,
            )?;
            ensure!(burned == expected, Error::<T>::HistoricalBalanceMismatch);
            Self::deposit_event(Event::HistoricalNativeSvyBurned { amount: burned });
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        fn move_asset(
            asset_id: u32,
            from: &AccountId32,
            to: &AccountId32,
            amount: u128,
        ) -> DispatchResult {
            ensure!(from != &DEAD, Error::<T>::ReservedAccount);
            ensure!(amount > 0 && from != to, Error::<T>::InvalidAmount);
            let liquidity = Assets::<T>::get(asset_id).ok_or(Error::<T>::UnknownAsset)?;
            let remaining = Holdings::<T>::get(asset_id, from)
                .checked_sub(amount)
                .ok_or(Error::<T>::InsufficientBalance)?;
            if to == &DEAD {
                if liquidity {
                    Retired::<T>::try_mutate(asset_id, |retired| -> DispatchResult {
                        let issued = EffectiveIssuance::<T>::get(asset_id);
                        *retired = retired
                            .checked_add(amount)
                            .filter(|retired| *retired <= issued)
                            .ok_or(Error::<T>::Arithmetic)?;
                        Ok(())
                    })?;
                    Self::deposit_event(Event::LiquidityRetired {
                        asset_id,
                        from: from.clone(),
                        amount,
                    });
                } else {
                    EffectiveIssuance::<T>::try_mutate(asset_id, |issued| -> DispatchResult {
                        *issued = issued.checked_sub(amount).ok_or(Error::<T>::Arithmetic)?;
                        Ok(())
                    })?;
                    Self::deposit_event(Event::AssetBurned {
                        asset_id,
                        from: from.clone(),
                        amount,
                    });
                }
            } else {
                Holdings::<T>::try_mutate(asset_id, to, |balance| -> DispatchResult {
                    *balance = balance.checked_add(amount).ok_or(Error::<T>::Arithmetic)?;
                    Ok(())
                })?;
            }
            if remaining == 0 {
                Holdings::<T>::remove(asset_id, from);
            } else {
                Holdings::<T>::insert(asset_id, from, remaining);
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{Incinerator, Runtime, RuntimeOrigin, System};
    use frame_support::{assert_noop, assert_ok};

    #[test]
    fn reserved_account_cannot_transfer_approve_delegate_or_receive_fixture() {
        sp_io::TestExternalities::default().execute_with(|| {
            System::set_block_number(1);
            let user = AccountId32::new([1; 32]);
            let delegate = AccountId32::new([2; 32]);
            let signed = |who: &AccountId32| RuntimeOrigin::signed(who.clone());
            assert_ok!(Incinerator::create_fixture(
                RuntimeOrigin::root(),
                1,
                false,
                user.clone(),
                100
            ));
            assert_ok!(Incinerator::create_fixture(
                RuntimeOrigin::root(),
                2,
                true,
                user.clone(),
                100
            ));
            assert_ok!(Incinerator::transfer(signed(&user), 1, DEAD, 40));
            assert_ok!(Incinerator::transfer(signed(&user), 2, DEAD, 40));
            assert_eq!(EffectiveIssuance::<Runtime>::get(1), 60);
            assert_eq!(EffectiveIssuance::<Runtime>::get(2), 100);
            assert_eq!(Retired::<Runtime>::get(2), 40);
            assert_eq!(Holdings::<Runtime>::get(1, DEAD), 0);
            assert_eq!(Holdings::<Runtime>::get(2, DEAD), 0);
            assert_noop!(
                Incinerator::transfer(signed(&DEAD), 1, user.clone(), 1),
                Error::<Runtime>::ReservedAccount
            );
            assert_noop!(
                Incinerator::approve(signed(&DEAD), 1, user.clone(), 1),
                Error::<Runtime>::ReservedAccount
            );
            assert_noop!(
                Incinerator::approve(signed(&user), 1, DEAD, 1),
                Error::<Runtime>::ReservedAccount
            );
            assert_noop!(
                Incinerator::transfer_approved(signed(&delegate), 2, DEAD, user.clone(), 1),
                Error::<Runtime>::ReservedAccount
            );
            assert_noop!(
                Incinerator::transfer_approved(signed(&DEAD), 2, user.clone(), delegate, 1),
                Error::<Runtime>::ReservedAccount
            );
            assert_noop!(
                Incinerator::create_fixture(RuntimeOrigin::root(), 3, false, DEAD, 1),
                Error::<Runtime>::ReservedAccount
            );
            assert_noop!(
                Incinerator::transfer(signed(&user), 1, DEAD, 61),
                Error::<Runtime>::InsufficientBalance
            );
            assert_eq!(EffectiveIssuance::<Runtime>::get(1), 60);
        });
    }
}
