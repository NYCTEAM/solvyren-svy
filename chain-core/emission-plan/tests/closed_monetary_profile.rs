//! Isolated closed-origin production-profile candidate; never deployed.
//! Internal mint remains trusted Runtime code. This is not a full cap proof.
use codec::{DecodeAll, Encode};
use eagle_emission_plan::{
    accounting::{PlanningCheckpoint, PlanningLedger},
    Split, CAP,
};
use frame_support::{
    derive_impl,
    storage::{with_transaction, TransactionOutcome},
    traits::{ConstU128, Contains, Currency, Imbalance},
};
use sp_runtime::{traits::Dispatchable, BuildStorage, DispatchError};
type Block = frame_system::mocking::MockBlock<Test>;
// No Sudo, governance, scheduler, proxy or arbitrary nested dispatch pallet is
// installed. This exact outer-call composition is part of the trust assumption.
pub struct ClosedCalls;
impl Contains<RuntimeCall> for ClosedCalls {
    fn contains(call: &RuntimeCall) -> bool {
        matches!(
            call,
            RuntimeCall::System(frame_system::Call::remark { .. })
                | RuntimeCall::System(frame_system::Call::remark_with_event { .. })
                | RuntimeCall::Balances(pallet_balances::Call::transfer_keep_alive { .. })
        )
    }
}
frame_support::construct_runtime!(pub enum Test {System:frame_system,Balances:pallet_balances});
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
const KEY: &[u8] = b":eagle:test-only:closed-profile:v1";
type EncodedCheckpoint = (u16, u64, u128, u128, u128, u128);
fn store(cp: PlanningCheckpoint) {
    sp_io::storage::set(
        KEY,
        &(
            cp.version,
            cp.last_height,
            cp.genesis_issued,
            cp.cumulative_issued,
            cp.base_issued,
            cp.ai_issued,
        )
            .encode(),
    );
}
fn load() -> Result<PlanningCheckpoint, DispatchError> {
    let raw = sp_io::storage::get(KEY).ok_or(DispatchError::Other("checkpoint missing"))?;
    let (version, last_height, genesis_issued, cumulative_issued, base_issued, ai_issued) =
        EncodedCheckpoint::decode_all(&mut &raw[..])
            .map_err(|_| DispatchError::Other("checkpoint encoding"))?;
    Ok(PlanningCheckpoint {
        version,
        last_height,
        genesis_issued,
        cumulative_issued,
        base_issued,
        ai_issued,
    })
}
fn ext(genesis: u128) -> sp_io::TestExternalities {
    let mut storage = frame_system::GenesisConfig::<Test>::default()
        .build_storage()
        .unwrap();
    pallet_balances::GenesisConfig::<Test> {
        balances: vec![(1, genesis - 20), (2, 10), (99, 10)],
        ..Default::default()
    }
    .assimilate_storage(&mut storage)
    .unwrap();
    let mut external: sp_io::TestExternalities = storage.into();
    external.execute_with(|| {
        store(PlanningLedger::new(genesis).unwrap().checkpoint());
        System::set_block_number(1);
    });
    external
}
fn snapshot() -> (Vec<u8>, u128, u128, u128, u128) {
    (
        sp_io::storage::get(KEY).unwrap().to_vec(),
        Balances::free_balance(1),
        Balances::free_balance(2),
        Balances::free_balance(99),
        Balances::total_issuance(),
    )
}
fn deposit_exact(account: u64, amount: u128) -> Result<(), DispatchError> {
    let deposit = Balances::deposit_creating(&account, amount);
    let credited = deposit.peek();
    drop(deposit);
    if credited != amount {
        return Err(DispatchError::Other("incomplete recipient credit"));
    }
    Ok(())
}
fn apply(genesis: u128, fault: u8) -> Result<Split, DispatchError> {
    with_transaction(|| {
        let result = (|| {
            let height = System::block_number();
            let previous = height
                .checked_sub(1)
                .ok_or(DispatchError::Other("genesis block"))?;
            let cp = load()?;
            if Balances::total_issuance() > cp.cumulative_issued {
                return Err(DispatchError::Other("untracked issuance"));
            }
            let mut ledger = PlanningLedger::from_checkpoint(cp, previous, genesis)
                .map_err(|_| DispatchError::Other("invalid planning checkpoint"))?;
            let split = ledger
                .apply_next(height)
                .map_err(|_| DispatchError::Other("planning transition"))?;
            store(ledger.checkpoint());
            if fault == 1 {
                return Err(DispatchError::Other("injected after counter"));
            }
            deposit_exact(2, split.base)?;
            if fault == 2 {
                return Err(DispatchError::Other("injected after base"));
            }
            deposit_exact(99, split.ai)?;
            if fault == 3 {
                return Err(DispatchError::Other("injected after both shares"));
            }
            Ok(split)
        })();
        if result.is_ok() {
            TransactionOutcome::Commit(result)
        } else {
            TransactionOutcome::Rollback(result)
        }
    })
}

fn sensitive_calls() -> Vec<RuntimeCall> {
    let key = pallet_balances::TotalIssuance::<Test>::hashed_key().to_vec();
    vec![
        RuntimeCall::Balances(pallet_balances::Call::force_set_balance {
            who: 1,
            new_free: CAP,
        }),
        RuntimeCall::Balances(pallet_balances::Call::force_adjust_total_issuance {
            direction: pallet_balances::AdjustmentDirection::Increase,
            delta: 500,
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
        RuntimeCall::System(frame_system::Call::set_code {
            code: vec![0, 97, 115, 109, 1, 0, 0, 0],
        }),
        RuntimeCall::System(frame_system::Call::set_code_without_checks {
            code: vec![0, 97, 115, 109, 1, 0, 0, 0],
        }),
        RuntimeCall::System(frame_system::Call::authorize_upgrade {
            code_hash: Default::default(),
        }),
        RuntimeCall::System(frame_system::Call::authorize_upgrade_without_checks {
            code_hash: Default::default(),
        }),
        RuntimeCall::System(frame_system::Call::apply_authorized_upgrade {
            code: vec![0, 97, 115, 109, 1, 0, 0, 0],
        }),
    ]
}
#[test]
fn encoded_signed_and_unsigned_monetary_storage_upgrade_routes_are_closed() {
    ext(3000).execute_with(|| {
        for call in sensitive_calls() {
            let bytes = call.encode();
            assert!(!ClosedCalls::contains(&call));
            for signed in [false, true] {
                let decoded = RuntimeCall::decode_all(&mut &bytes[..]).unwrap();
                let before = snapshot();
                let events = System::events();
                let code = sp_io::storage::get(b":code");
                let auth = System::authorized_upgrade();
                let origin = if signed {
                    RuntimeOrigin::signed(1)
                } else {
                    RuntimeOrigin::none()
                };
                assert_eq!(
                    decoded.dispatch(origin).unwrap_err().error,
                    frame_system::Error::<Test>::CallFiltered.into()
                );
                assert_eq!(snapshot(), before);
                assert_eq!(System::events(), events);
                assert_eq!(sp_io::storage::get(b":code"), code);
                assert_eq!(System::authorized_upgrade().encode(), auth.encode());
            }
        }
    });
}
#[test]
fn prior_authorized_upgrade_is_still_blocked_for_external_origins() {
    use sp_runtime::traits::Hash;
    ext(3000).execute_with(|| {
        let code = vec![0, 97, 115, 109, 1, 0, 0, 0];
        let hash = <Test as frame_system::Config>::Hashing::hash(&code);
        assert!(System::authorize_upgrade_without_checks(RuntimeOrigin::root(), hash).is_ok());
        let before = System::authorized_upgrade();
        let code_before = sp_io::storage::get(b":code");
        for origin in [RuntimeOrigin::signed(1), RuntimeOrigin::none()] {
            let call = RuntimeCall::System(frame_system::Call::apply_authorized_upgrade {
                code: code.clone(),
            });
            assert!(call.dispatch(origin).is_err());
            assert_eq!(System::authorized_upgrade().encode(), before.encode());
            assert_eq!(sp_io::storage::get(b":code"), code_before);
        }
    });
}
#[test]
fn no_third_pallet_dispatch_variant_or_sudo_wrapper_exists() {
    // This candidate has only System index0 and Balances index1. A future added
    // pallet must trigger review of both this composition and the whitelist.
    for index in 2..=255u8 {
        assert!(RuntimeCall::decode_all(&mut &[index, 0][..]).is_err());
    }
}
#[test]
fn ordinary_transfer_preserves_stock_and_scheduled_emission_remains_once_per_height() {
    ext(3000).execute_with(|| {
        let call = RuntimeCall::Balances(pallet_balances::Call::transfer_keep_alive {
            dest: 2,
            value: 100,
        });
        assert!(ClosedCalls::contains(&call));
        assert!(call.dispatch(RuntimeOrigin::signed(1)).is_ok());
        assert_eq!(Balances::total_issuance(), 3000);
        let split = apply(3000, 0).unwrap();
        assert_eq!(split.ai, split.issued * 30 / 100);
        assert_eq!(split.base + split.ai, split.issued);
        let after = snapshot();
        assert!(apply(3000, 0).is_err());
        assert_eq!(snapshot(), after);
    });
}
#[test]
fn near_cap_burn_and_failed_credit_preserve_gross_cap_and_atomicity() {
    for fault in [1, 2, 3] {
        ext(CAP - 5).execute_with(|| {
            let before = snapshot();
            assert!(apply(CAP - 5, fault).is_err());
            assert_eq!(snapshot(), before);
            assert_eq!(apply(CAP - 5, 0).unwrap().issued, 5);
            let (burn, remaining) = Balances::slash(&1, 50);
            assert_eq!(remaining, 0);
            drop(burn);
            System::set_block_number(2);
            assert_eq!(apply(CAP - 5, 0).unwrap().issued, 0);
            assert_eq!(load().unwrap().cumulative_issued, CAP);
            assert_eq!(Balances::total_issuance(), CAP - 50);
        });
    }
}
#[test]
fn root_and_internal_currency_are_explicitly_outside_external_profile_guarantee() {
    // Root is not a signed/unsigned extrinsic origin. It remains powerful if a
    // later governance/wrapper/Runtime path introduces it; filtering is no cure.
    ext(3000).execute_with(|| {
        let call = RuntimeCall::Balances(pallet_balances::Call::force_set_balance {
            who: 1,
            new_free: CAP,
        });
        assert!(call.dispatch(RuntimeOrigin::root()).is_ok());
        assert_eq!(Balances::total_issuance(), CAP + 20);
    });
    ext(CAP - 100).execute_with(|| {
        assert_eq!(apply(CAP - 100, 0).unwrap().issued, 100);
        let (burn, _) = Balances::slash(&1, 50);
        drop(burn);
        deposit_exact(1, 50).unwrap();
        assert_eq!(Balances::total_issuance(), CAP);
        assert_eq!(load().unwrap().cumulative_issued, CAP);
    });
}
