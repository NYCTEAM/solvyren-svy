//! Isolated characterization of development privileges. No live node or signer.
use codec::Encode;
use eagle_emission_plan::CAP;
use frame_support::{
    derive_impl,
    traits::{ConstU128, Contains},
    weights::Weight,
};
use sp_runtime::{
    traits::{Dispatchable, Hash},
    BuildStorage,
};

type Block = frame_system::mocking::MockBlock<Test>;
frame_support::construct_runtime!(pub enum Test {
    System: frame_system,
    Balances: pallet_balances,
    Sudo: pallet_sudo,
});

pub struct RejectMonetaryWrites;
impl Contains<RuntimeCall> for RejectMonetaryWrites {
    fn contains(call: &RuntimeCall) -> bool {
        !matches!(
            call,
            RuntimeCall::Balances(pallet_balances::Call::force_set_balance { .. })
                | RuntimeCall::Balances(pallet_balances::Call::force_adjust_total_issuance { .. })
                | RuntimeCall::System(frame_system::Call::set_storage { .. })
        )
    }
}
#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
    type Block = Block;
    type AccountData = pallet_balances::AccountData<u128>;
    type BaseCallFilter = RejectMonetaryWrites;
}
#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Test {
    type AccountStore = System;
    type Balance = u128;
    type ExistentialDeposit = ConstU128<10>;
}
impl pallet_sudo::Config for Test {
    type RuntimeEvent = RuntimeEvent;
    type RuntimeCall = RuntimeCall;
    type WeightInfo = ();
}
fn ext() -> sp_io::TestExternalities {
    let mut storage = frame_system::GenesisConfig::<Test>::default()
        .build_storage()
        .unwrap();
    pallet_balances::GenesisConfig::<Test> {
        balances: vec![(1, 2980), (2, 10), (99, 10)],
        ..Default::default()
    }
    .assimilate_storage(&mut storage)
    .unwrap();
    pallet_sudo::GenesisConfig::<Test> { key: Some(1) }
        .assimilate_storage(&mut storage)
        .unwrap();
    let mut external: sp_io::TestExternalities = storage.into();
    external.execute_with(|| System::set_block_number(1));
    external
}
fn balance_call() -> RuntimeCall {
    RuntimeCall::Balances(pallet_balances::Call::force_set_balance {
        who: 1,
        new_free: CAP,
    })
}
fn wrap_sudo(call: RuntimeCall, unchecked: bool) -> RuntimeCall {
    if unchecked {
        RuntimeCall::Sudo(pallet_sudo::Call::sudo_unchecked_weight {
            call: Box::new(call),
            weight: Weight::zero(),
        })
    } else {
        RuntimeCall::Sudo(pallet_sudo::Call::sudo {
            call: Box::new(call),
        })
    }
}
fn snapshot() -> Vec<u8> {
    (
        System::account(1),
        System::account(2),
        System::account(99),
        Balances::total_issuance(),
        pallet_sudo::Key::<Test>::get(),
        System::authorized_upgrade(),
        System::events(),
    )
        .encode()
}
fn assert_sudo_inner_success() {
    assert!(System::events().iter().any(|record| matches!(
        &record.event,
        RuntimeEvent::Sudo(pallet_sudo::Event::Sudid {
            sudo_result: Ok(())
        })
    )));
}

#[test]
fn both_sudo_variants_bypass_filter_that_rejects_direct_signed_balance_call() {
    for unchecked in [false, true] {
        ext().execute_with(|| {
            assert!(!RejectMonetaryWrites::contains(&balance_call()));
            let before = snapshot();
            let error = balance_call()
                .dispatch(RuntimeOrigin::signed(1))
                .unwrap_err();
            assert_eq!(
                error.error,
                frame_system::Error::<Test>::CallFiltered.into()
            );
            assert_eq!(snapshot(), before);
            let outer = wrap_sudo(balance_call(), unchecked);
            assert!(RejectMonetaryWrites::contains(&outer));
            assert!(outer.dispatch(RuntimeOrigin::signed(1)).is_ok());
            assert_sudo_inner_success();
            assert_eq!(Balances::free_balance(1), CAP);
            assert_eq!(Balances::total_issuance(), CAP + 20);
        });
    }
}

#[test]
fn generated_root_dispatch_also_bypasses_base_filter() {
    ext().execute_with(|| {
        assert!(!RejectMonetaryWrites::contains(&balance_call()));
        assert!(balance_call().dispatch(RuntimeOrigin::root()).is_ok());
        assert_eq!(Balances::total_issuance(), CAP + 20);
    });
}

#[test]
fn sudo_raw_storage_write_can_replace_total_without_any_balance_credit() {
    ext().execute_with(|| {
        let key = pallet_balances::TotalIssuance::<Test>::hashed_key().to_vec();
        let call = RuntimeCall::System(frame_system::Call::set_storage {
            items: vec![(key, (CAP + 1).encode())],
        });
        assert!(!RejectMonetaryWrites::contains(&call));
        let before = snapshot();
        let error = call.clone().dispatch(RuntimeOrigin::signed(1)).unwrap_err();
        assert_eq!(
            error.error,
            frame_system::Error::<Test>::CallFiltered.into()
        );
        assert_eq!(snapshot(), before);
        assert!(wrap_sudo(call, false)
            .dispatch(RuntimeOrigin::signed(1))
            .is_ok());
        assert_sudo_inner_success();
        assert_eq!(Balances::total_issuance(), CAP + 1);
        assert_eq!(
            Balances::free_balance(1) + Balances::free_balance(2) + Balances::free_balance(99),
            3000
        );
    });
}

#[test]
fn non_key_caller_and_removed_key_cannot_use_sudo() {
    ext().execute_with(|| {
        let before = snapshot();
        assert!(wrap_sudo(balance_call(), false)
            .dispatch(RuntimeOrigin::signed(2))
            .is_err());
        assert_eq!(snapshot(), before);
        assert!(Sudo::remove_key(RuntimeOrigin::signed(1)).is_ok());
        assert!(pallet_sudo::Key::<Test>::get().is_none());
        let removed = snapshot();
        for unchecked in [false, true] {
            assert!(wrap_sudo(balance_call(), unchecked)
                .dispatch(RuntimeOrigin::signed(1))
                .is_err());
            assert_eq!(snapshot(), removed);
        }
    });
}

#[test]
fn removing_sudo_key_does_not_revoke_either_prior_upgrade_authorization() {
    for unchecked in [false, true] {
        ext().execute_with(|| {
            let code_hash = <Test as frame_system::Config>::Hashing::hash(b"isolated pending code");
            let call = if unchecked {
                RuntimeCall::System(frame_system::Call::authorize_upgrade_without_checks {
                    code_hash,
                })
            } else {
                RuntimeCall::System(frame_system::Call::authorize_upgrade { code_hash })
            };
            let before = snapshot();
            assert!(call.clone().dispatch(RuntimeOrigin::signed(2)).is_err());
            assert_eq!(snapshot(), before);
            assert!(wrap_sudo(call, false)
                .dispatch(RuntimeOrigin::signed(1))
                .is_ok());
            let authorization = System::authorized_upgrade().unwrap();
            assert_eq!(authorization.code_hash(), &code_hash);
            assert_eq!(authorization.encode(), (code_hash, !unchecked).encode());
            assert!(Sudo::remove_key(RuntimeOrigin::signed(1)).is_ok());
            assert!(pallet_sudo::Key::<Test>::get().is_none());
            assert_eq!(
                System::authorized_upgrade().encode(),
                Some(authorization).encode()
            );
        });
    }
}

#[test]
fn previously_authorized_unchecked_code_can_be_stored_after_sudo_key_removal() {
    for unsigned in [false, true] {
        ext().execute_with(|| {
            // Minimal WASM header only: not an executable Eagle Runtime.
            // Exercise FRAME's default storage setter, not a mock OnSetCode hook.
            let code = vec![0, 97, 115, 109, 1, 0, 0, 0];
            let code_hash = <Test as frame_system::Config>::Hashing::hash(&code);
            assert!(
                System::authorize_upgrade_without_checks(RuntimeOrigin::root(), code_hash).is_ok()
            );
            assert!(Sudo::remove_key(RuntimeOrigin::signed(1)).is_ok());
            let authorization = System::authorized_upgrade();
            let previous_code = sp_io::storage::get(b":code");
            let wrong =
                RuntimeCall::System(frame_system::Call::apply_authorized_upgrade { code: vec![1] });
            assert_eq!(
                wrong.dispatch(RuntimeOrigin::signed(2)).unwrap_err().error,
                frame_system::Error::<Test>::Unauthorized.into()
            );
            assert_eq!(
                System::authorized_upgrade().encode(),
                authorization.encode()
            );
            assert_eq!(sp_io::storage::get(b":code"), previous_code);
            let call = RuntimeCall::System(frame_system::Call::apply_authorized_upgrade {
                code: code.clone(),
            });
            let origin = if unsigned {
                RuntimeOrigin::none()
            } else {
                RuntimeOrigin::signed(2)
            };
            assert!(call.dispatch(origin).is_ok());
            assert_eq!(
                sp_io::storage::get(b":code").unwrap().as_ref(),
                code.as_slice()
            );
            assert!(System::authorized_upgrade().is_none());
            let repeat = RuntimeCall::System(frame_system::Call::apply_authorized_upgrade {
                code: code.clone(),
            });
            assert_eq!(
                repeat.dispatch(RuntimeOrigin::signed(2)).unwrap_err().error,
                frame_system::Error::<Test>::NothingAuthorized.into()
            );
            assert_eq!(
                sp_io::storage::get(b":code").unwrap().as_ref(),
                code.as_slice()
            );
        });
    }
}
