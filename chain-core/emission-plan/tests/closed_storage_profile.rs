//! Real shared issuance storage plus default-deny external calls, in isolated
//! FRAME externalities only. No deployed Runtime, node, GPU or calendar policy.
use codec::{DecodeAll, Encode};
use eagle_emission_plan::{CAP, INITIAL_REWARD};
use frame_support::{
    derive_impl,
    traits::{ConstU128, ConstU64, Contains, Currency, Imbalance},
};
use sp_runtime::{traits::Dispatchable, BuildStorage};
#[path = "../research/closed_monetary_calls.rs"]
mod closed_calls;
#[path = "../research/storage_pallet.rs"]
mod emission;

type Block = frame_system::mocking::MockBlock<Test>;
frame_support::construct_runtime!(pub enum Test {
    System: frame_system,
    Balances: pallet_balances,
    Emission: emission,
});
define_closed_monetary_calls!(pub struct ClosedCalls for RuntimeCall);
#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
    type Block = Block;
    type AccountData = pallet_balances::AccountData<u128>;
    type BaseCallFilter = ClosedCalls;
}
#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Test {
    type AccountStore = System;
    type Balance = u128;
    type ExistentialDeposit = ConstU128<10>;
}
impl emission::Config for Test {
    type Currency = Balances;
    type AiPool = ConstU64<99>;
}
fn external(genesis: u128) -> sp_io::TestExternalities {
    let mut storage = frame_system::GenesisConfig::<Test>::default()
        .build_storage()
        .unwrap();
    pallet_balances::GenesisConfig::<Test> {
        balances: vec![(1, genesis - 20), (2, 10), (99, 10)],
        ..Default::default()
    }
    .assimilate_storage(&mut storage)
    .unwrap();
    emission::GenesisConfig::<Test> {
        genesis_issued: genesis,
        ..Default::default()
    }
    .assimilate_storage(&mut storage)
    .unwrap();
    let mut ext: sp_io::TestExternalities = storage.into();
    ext.execute_with(|| {
        System::set_block_number(1);
        let mut work = [0; 32];
        work[0] = 2;
        Emission::initialize_acceptance(emission::AcceptanceData {
            version: 1,
            height: 0,
            header: [0; 108],
            work,
            policy: [7; 32],
        })
        .unwrap();
    });
    ext
}
fn snapshot() -> Vec<u8> {
    (
        Emission::state(),
        Emission::read_acceptance().unwrap(),
        Balances::total_issuance(),
        System::account(1),
        System::account(2),
        System::account(99),
        System::events(),
        System::authorized_upgrade(),
        sp_io::storage::get(b":code").map(|v| v.to_vec()),
    )
        .encode()
}
fn successor(previous: &emission::AcceptanceData) -> emission::AcceptanceData {
    let mut next = previous.clone();
    next.height += 1;
    next.work[0] += 2;
    let id = sp_io::hashing::sha2_256(&sp_io::hashing::sha2_256(&previous.header));
    next.header[4..36].copy_from_slice(&id);
    next
}
fn issue() {
    let old = Emission::read_acceptance().unwrap();
    Emission::issue_accepted(&old, successor(&old), &2).unwrap();
}

#[test]
fn encoded_external_monetary_storage_and_upgrade_calls_are_denied_without_mutation() {
    external(3000).execute_with(|| {
        let key = pallet_balances::TotalIssuance::<Test>::hashed_key().to_vec();
        let calls = vec![
            RuntimeCall::Balances(pallet_balances::Call::force_set_balance {
                who: 1,
                new_free: CAP,
            }),
            RuntimeCall::Balances(pallet_balances::Call::force_adjust_total_issuance {
                direction: pallet_balances::AdjustmentDirection::Increase,
                delta: 1,
            }),
            RuntimeCall::Balances(pallet_balances::Call::burn {
                value: 1,
                keep_alive: true,
            }),
            RuntimeCall::System(frame_system::Call::set_storage {
                items: vec![(key.clone(), (CAP + 1).encode())],
            }),
            RuntimeCall::System(frame_system::Call::kill_storage {
                keys: vec![key.clone()],
            }),
            RuntimeCall::System(frame_system::Call::kill_prefix {
                prefix: key,
                subkeys: 1,
            }),
            RuntimeCall::System(frame_system::Call::set_code { code: vec![] }),
            RuntimeCall::System(frame_system::Call::set_code_without_checks { code: vec![] }),
            RuntimeCall::System(frame_system::Call::authorize_upgrade {
                code_hash: Default::default(),
            }),
            RuntimeCall::System(frame_system::Call::authorize_upgrade_without_checks {
                code_hash: Default::default(),
            }),
            RuntimeCall::System(frame_system::Call::apply_authorized_upgrade { code: vec![] }),
            RuntimeCall::System(frame_system::Call::remark { remark: vec![] }),
        ];
        for call in calls {
            assert!(!ClosedCalls::contains(&call));
            for signed in [true, false] {
                let decoded = RuntimeCall::decode_all(&mut &call.encode()[..]).unwrap();
                let origin = if signed {
                    RuntimeOrigin::signed(1)
                } else {
                    RuntimeOrigin::none()
                };
                let before = snapshot();
                assert_eq!(
                    decoded.dispatch(origin).unwrap_err().error,
                    frame_system::Error::<Test>::CallFiltered.into()
                );
                assert_eq!(snapshot(), before);
            }
        }
        // The actual Emission pallet has no dispatchable mint. No Sudo or
        // arbitrary nested-dispatch outer pallet exists in this composition.
        assert!(RuntimeCall::decode_all(&mut &[2, 0][..]).is_err());
        for index in 3..=255u8 {
            assert!(RuntimeCall::decode_all(&mut &[index, 0][..]).is_err());
        }
    });
}

#[test]
fn permitted_no_dust_transfers_require_signed_origin_and_preserve_real_issuance_counters() {
    for keep_alive in [false, true] {
        external(3000).execute_with(|| {
            let call = if keep_alive {
                RuntimeCall::Balances(pallet_balances::Call::transfer_keep_alive {
                    dest: 2,
                    value: 100,
                })
            } else {
                RuntimeCall::Balances(pallet_balances::Call::transfer_allow_death {
                    dest: 2,
                    value: 100,
                })
            };
            assert!(ClosedCalls::contains(&call));
            let before = snapshot();
            assert!(call.clone().dispatch(RuntimeOrigin::none()).is_err());
            assert_eq!(snapshot(), before);
            let ledger = Emission::state();
            let anchor = Emission::read_acceptance().unwrap();
            assert!(call.dispatch(RuntimeOrigin::signed(1)).is_ok());
            assert_eq!(Balances::free_balance(2), 110);
            assert_eq!(Balances::total_issuance(), 3000);
            assert_eq!(Emission::state(), ledger);
            assert_eq!(Emission::read_acceptance().unwrap(), anchor);
            issue();
            let state = Emission::state().unwrap();
            assert_eq!(state.cumulative, 3000 + INITIAL_REWARD);
            assert_eq!(state.ai, INITIAL_REWARD * 30 / 100);
            assert_eq!(state.base + state.ai, INITIAL_REWARD);
            let after = snapshot();
            assert!(Emission::issue_accepted(&anchor, successor(&anchor), &2).is_err());
            assert_eq!(snapshot(), after);
        });
    }
}

#[test]
fn allowed_death_transfer_dust_burn_does_not_reopen_cumulative_issuance_headroom() {
    // Actual fixture: ED=10, DustRemoval=() from Balances TestDefaultConfig,
    // account1=genesis-20, existing account2=10, existing pool99=10.
    // With genesis3000, transferring2975 leaves5, which is reaped and burned.
    for genesis in [3000, CAP - 5] {
        external(genesis).execute_with(|| {
            assert_eq!(Balances::minimum_balance(), 10);
            assert_eq!(Balances::free_balance(1), genesis - 20);
            assert_eq!(Balances::free_balance(2), 10);
            assert_eq!(Balances::free_balance(99), 10);
            let ledger = Emission::state();
            let anchor = Emission::read_acceptance().unwrap();
            let amount = genesis - 25;
            let call = RuntimeCall::Balances(pallet_balances::Call::transfer_allow_death {
                dest: 2,
                value: amount,
            });
            assert!(ClosedCalls::contains(&call));
            assert!(call.dispatch(RuntimeOrigin::signed(1)).is_ok());
            assert_eq!(Balances::free_balance(1), 0);
            assert_eq!(Balances::free_balance(2), 10 + amount);
            assert_eq!(Balances::total_issuance(), genesis - 5);
            assert_eq!(Emission::state(), ledger);
            assert_eq!(Emission::read_acceptance().unwrap(), anchor);
            issue();
            // The next credit is clipped against gross cumulative issuance,
            // never against the now lower circulating stock after dust removal.
            let scheduled = if genesis == 3000 { INITIAL_REWARD } else { 5 };
            let state = Emission::state().unwrap();
            assert_eq!(state.cumulative, genesis + scheduled);
            assert_eq!(state.base + state.ai, scheduled);
            assert_eq!(Balances::total_issuance(), genesis - 5 + scheduled);
            if genesis == CAP - 5 {
                assert_eq!(state.cumulative, CAP);
                assert_eq!(Balances::total_issuance(), CAP - 5);
                System::set_block_number(2);
                issue();
                let state = Emission::state().unwrap();
                assert_eq!(
                    (state.last, state.cumulative, state.base, state.ai),
                    (2, CAP, 4, 1)
                );
                assert_eq!(Balances::total_issuance(), CAP - 5);
            }
        });
    }
}

#[test]
fn real_cap_clip_and_failed_second_credit_roll_back_acceptance_balances_and_events() {
    external(CAP - 5).execute_with(|| {
        // Internal Root intentionally bypasses the external filter. Remove the
        // pool so its one-unit credit is below ED, forcing actual credit failure.
        Balances::force_set_balance(RuntimeOrigin::root(), 99, 0).unwrap();
        let old = Emission::read_acceptance().unwrap();
        let before = snapshot();
        assert!(Emission::issue_accepted(&old, successor(&old), &2).is_err());
        assert_eq!(snapshot(), before);
    });
    external(CAP - 5).execute_with(|| {
        issue();
        let state = Emission::state().unwrap();
        assert_eq!((state.cumulative, state.base, state.ai), (CAP, 4, 1));
        System::set_block_number(2);
        issue();
        let state = Emission::state().unwrap();
        assert_eq!(
            (state.last, state.cumulative, state.base, state.ai),
            (2, CAP, 4, 1)
        );
        assert_eq!(Balances::total_issuance(), CAP);
    });
}

#[test]
fn internal_root_and_burn_remint_remain_explicitly_outside_filter_guarantee() {
    external(3000).execute_with(|| {
        let call = RuntimeCall::Balances(pallet_balances::Call::force_set_balance {
            who: 1,
            new_free: CAP,
        });
        assert!(!ClosedCalls::contains(&call));
        assert!(call.dispatch(RuntimeOrigin::root()).is_ok());
        assert_eq!(Balances::total_issuance(), CAP + 20);
        let old = Emission::read_acceptance().unwrap();
        let before = snapshot();
        assert!(Emission::issue_accepted(&old, successor(&old), &2).is_err());
        assert_eq!(snapshot(), before);
    });
    external(CAP - 5).execute_with(|| {
        issue();
        let ledger = Emission::state();
        let (burn, remaining) = Balances::slash(&1, 50);
        assert_eq!(remaining, 0);
        drop(burn);
        let credit = Balances::deposit_creating(&1, 50);
        assert_eq!(credit.peek(), 50);
        drop(credit);
        assert_eq!(Balances::total_issuance(), CAP);
        assert_eq!(Emission::state(), ledger); // This hidden gross remint is not solved.
        System::set_block_number(2);
        issue();
        assert_eq!(Emission::state().unwrap().cumulative, CAP);
    });
}
