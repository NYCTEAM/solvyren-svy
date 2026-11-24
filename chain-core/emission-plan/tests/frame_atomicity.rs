//! Test-only adapter: actual FRAME storage/Balance APIs in TestExternalities.
//! No production pallet, runtime hooks, signer, node or live currency is involved.
use codec::{DecodeAll, Encode};
use eagle_emission_plan::{
    accounting::{PlanningCheckpoint, PlanningLedger},
    Split, CAP,
};
use frame_support::{
    derive_impl,
    storage::{with_transaction, TransactionOutcome},
    traits::{ConstU128, Currency, Imbalance},
};
use sp_runtime::{BuildStorage, DispatchError};
type Block = frame_system::mocking::MockBlock<Test>;
frame_support::construct_runtime!(pub enum Test {System:frame_system,Balances:pallet_balances});
#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
    type Block = Block;
    type AccountData = pallet_balances::AccountData<u128>;
}
#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Test {
    type AccountStore = System;
    type Balance = u128;
    type ExistentialDeposit = ConstU128<10>;
}
const KEY: &[u8] = b":eagle:test-only:emission-checkpoint:v1";
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
#[test]
fn balances_and_counter_commit_once_and_reject_duplicate_or_skipped_blocks() {
    ext(3000).execute_with(|| {
        let before = snapshot();
        let split = apply(3000, 0).unwrap();
        assert_eq!(Balances::free_balance(2), before.2 + split.base);
        assert_eq!(Balances::free_balance(99), before.3 + split.ai);
        assert_eq!(Balances::total_issuance(), 3000 + split.issued);
        assert_eq!(load().unwrap().cumulative_issued, 3000 + split.issued);
        let committed = snapshot();
        assert!(apply(3000, 0).is_err());
        assert_eq!(snapshot(), committed);
        System::set_block_number(3);
        assert!(apply(3000, 0).is_err());
        assert_eq!(snapshot(), committed);
    });
}
#[test]
fn faults_after_counter_one_share_or_both_shares_roll_back_all_storage() {
    for fault in [1, 2, 3] {
        ext(3000).execute_with(|| {
            let before = snapshot();
            let events = System::events();
            assert!(apply(3000, fault).is_err());
            assert_eq!(snapshot(), before);
            assert_eq!(System::events(), events);
            assert!(apply(3000, 0).is_ok());
        });
    }
}
#[test]
fn outer_storage_rollback_reverts_a_successful_nested_application() {
    ext(3000).execute_with(|| {
        let before = snapshot();
        let result: Result<(), DispatchError> = with_transaction(|| {
            assert!(apply(3000, 0).is_ok());
            assert_ne!(snapshot(), before);
            TransactionOutcome::Rollback(Ok(()))
        });
        assert!(result.is_ok());
        assert_eq!(snapshot(), before);
        assert!(apply(3000, 0).is_ok());
    });
}
#[test]
fn burning_balance_after_cap_does_not_reopen_cumulative_issuance() {
    ext(CAP - 5).execute_with(|| {
        assert_eq!(apply(CAP - 5, 0).unwrap().issued, 5);
        assert_eq!(Balances::total_issuance(), CAP);
        let (burn, unslashed) = Balances::slash(&1, 10);
        assert_eq!(unslashed, 0);
        drop(burn);
        assert_eq!(Balances::total_issuance(), CAP - 10);
        assert_eq!(load().unwrap().cumulative_issued, CAP);
        System::set_block_number(2);
        assert_eq!(apply(CAP - 5, 0).unwrap().issued, 0);
        assert_eq!(Balances::total_issuance(), CAP - 10);
        assert_eq!(load().unwrap().cumulative_issued, CAP);
    });
}
#[test]
fn untracked_mint_and_balanced_checkpoint_forgery_fail_before_more_issuance() {
    ext(3000).execute_with(|| {
        drop(Balances::deposit_creating(&1, 1));
        let before = snapshot();
        assert!(apply(3000, 0).is_err());
        assert_eq!(snapshot(), before);
    });
    ext(3000).execute_with(|| {
        let mut cp = load().unwrap();
        cp.cumulative_issued += 1;
        cp.base_issued += 1;
        store(cp);
        let before = snapshot();
        assert!(apply(3000, 0).is_err());
        assert_eq!(snapshot(), before);
    });
}
#[test]
fn trailing_checkpoint_bytes_are_not_silently_accepted() {
    ext(3000).execute_with(|| {
        let mut raw = sp_io::storage::get(KEY).unwrap().to_vec();
        raw.push(0);
        sp_io::storage::set(KEY, &raw);
        let before = snapshot();
        assert!(apply(3000, 0).is_err());
        assert_eq!(snapshot(), before);
    });
}

#[test]
fn reaped_recipient_below_minimum_must_not_commit_an_unpaid_emission() {
    for recipient in [2, 99] {
        ext(CAP - 5).execute_with(|| {
            assert!(Balances::force_set_balance(RuntimeOrigin::root(), recipient, 0).is_ok());
            assert_eq!(Balances::free_balance(recipient), 0);
            let before = snapshot();
            let events = System::events();
            assert_eq!(
                apply(CAP - 5, 0),
                Err(DispatchError::Other("incomplete recipient credit")),
                "recipient {recipient}"
            );
            assert_eq!(snapshot(), before);
            assert_eq!(System::events(), events);
        });
    }
}

// Characterization of development authority, not production-cap acceptance.
// These calls execute only in TestExternalities, never against the running node.
#[test]
fn privileged_balance_setting_can_exceed_cap_before_adapter_can_reject() {
    ext(3000).execute_with(|| {
        let before = snapshot();
        assert_eq!(
            Balances::force_set_balance(RuntimeOrigin::signed(1), 1, CAP),
            Err(DispatchError::BadOrigin)
        );
        assert_eq!(snapshot(), before);

        assert!(Balances::force_set_balance(RuntimeOrigin::root(), 1, CAP).is_ok());
        assert_eq!(Balances::free_balance(1), CAP);
        assert_eq!(Balances::total_issuance(), CAP + 20);
        assert_eq!(load().unwrap().cumulative_issued, 3000);
        let already_modified = snapshot();
        assert_eq!(
            apply(3000, 0),
            Err(DispatchError::Other("untracked issuance"))
        );
        // Rejecting a later emission does not undo a previous privileged dispatch.
        assert_eq!(snapshot(), already_modified);
    });
}

#[test]
fn privileged_total_only_adjustment_breaks_account_sum_without_crediting_accounts() {
    ext(3000).execute_with(|| {
        let before = snapshot();
        assert_eq!(
            Balances::force_adjust_total_issuance(
                RuntimeOrigin::signed(1),
                pallet_balances::AdjustmentDirection::Increase,
                500,
            ),
            Err(DispatchError::BadOrigin)
        );
        assert_eq!(snapshot(), before);
        assert!(Balances::force_adjust_total_issuance(
            RuntimeOrigin::root(),
            pallet_balances::AdjustmentDirection::Increase,
            500,
        )
        .is_ok());
        assert_eq!(
            Balances::free_balance(1) + Balances::free_balance(2) + Balances::free_balance(99),
            3000
        );
        assert_eq!(Balances::total_issuance(), 3500);
        assert_eq!(load().unwrap().cumulative_issued, 3000);
        let already_modified = snapshot();
        assert!(apply(3000, 0).is_err());
        assert_eq!(snapshot(), already_modified);
    });
}

#[test]
fn stock_only_guard_cannot_detect_untracked_remint_hidden_by_a_prior_burn() {
    ext(CAP - 100).execute_with(|| {
        assert_eq!(apply(CAP - 100, 0).unwrap().issued, 100);
        assert_eq!(Balances::total_issuance(), CAP);
        let checkpoint = load().unwrap();
        let (burn, unslashed) = Balances::slash(&1, 50);
        assert_eq!(unslashed, 0);
        drop(burn);
        assert_eq!(Balances::total_issuance(), CAP - 50);
        deposit_exact(1, 50).unwrap(); // Deliberately bypass the planning adapter.
        assert_eq!(Balances::total_issuance(), CAP);
        assert_eq!(load().unwrap(), checkpoint);
        System::set_block_number(2);
        // Current stock looks valid even though another 50 atoms were ever issued.
        assert_eq!(apply(CAP - 100, 0).unwrap().issued, 0);
        assert_eq!(load().unwrap().cumulative_issued, CAP);
        assert_eq!(Balances::total_issuance(), CAP);
    });
}
