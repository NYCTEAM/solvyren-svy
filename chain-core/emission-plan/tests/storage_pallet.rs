//! Real FRAME storage candidate, without a node, author proof or live issuance.
#[path = "../research/storage_pallet.rs"]
mod emission;
use codec::{DecodeAll, Encode};
use eagle_emission_plan::{
    accounting::PlanningLedger, advance_stored_reward, period_reward, BLOCKS_PER_PERIOD, CAP,
    INITIAL_REWARD,
};
use frame_support::{
    derive_impl,
    storage::{with_transaction, TransactionOutcome},
    traits::{ConstU128, ConstU64, Currency},
};
use sp_runtime::{BuildStorage, DispatchError};
type Block = frame_system::mocking::MockBlock<Test>;
frame_support::construct_runtime!(pub enum Test{System:frame_system,Balances:pallet_balances,Emission:emission});
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
impl emission::Config for Test {
    type Currency = Balances;
    type AiPool = ConstU64<99>;
}
fn ext(genesis: u128) -> sp_io::TestExternalities {
    let mut s = frame_system::GenesisConfig::<Test>::default()
        .build_storage()
        .unwrap();
    pallet_balances::GenesisConfig::<Test> {
        balances: vec![(1, genesis - 20), (2, 10), (99, 10)],
        ..Default::default()
    }
    .assimilate_storage(&mut s)
    .unwrap();
    emission::GenesisConfig::<Test> {
        genesis_issued: genesis,
        ..Default::default()
    }
    .assimilate_storage(&mut s)
    .unwrap();
    let mut e: sp_io::TestExternalities = s.into();
    e.execute_with(|| System::set_block_number(1));
    e
}
fn snapshot() -> Vec<u8> {
    (
        sp_io::storage::get(&emission::Ledger::<Test>::hashed_key()).map(|b| b.to_vec()),
        sp_io::storage::get(&emission::Acceptance::<Test>::hashed_key()).map(|b| b.to_vec()),
        Emission::state(),
        Balances::total_issuance(),
        System::account(1),
        System::account(2),
        System::account(99),
        System::events(),
    )
        .encode()
}
#[test]
fn storage_matches_planning_reference_and_refuses_duplicate_skipped_and_missing_state() {
    ext(3000).execute_with(|| {
        let mut reference = PlanningLedger::new(3000).unwrap();
        for height in 1..=100 {
            System::set_block_number(height);
            let split = reference.apply_next(height).unwrap();
            assert!(Emission::issue_for_author(&2).is_ok());
            let s = Emission::state().unwrap();
            assert_eq!(
                (s.last, s.cumulative, s.base, s.ai),
                (
                    height,
                    reference.cumulative_issued(),
                    reference.base_issued(),
                    reference.ai_issued()
                )
            );
            assert_eq!(s.cumulative, 3000 + s.base + s.ai);
            assert_eq!(Balances::total_issuance(), s.cumulative);
            assert_eq!(split.base + split.ai, split.issued);
        }
        let before = snapshot();
        assert!(Emission::issue_for_author(&2).is_err());
        assert_eq!(snapshot(), before);
        System::set_block_number(102);
        let before = snapshot();
        assert!(Emission::issue_for_author(&2).is_err());
        assert_eq!(snapshot(), before);
        emission::Ledger::<Test>::kill();
        let before = snapshot();
        assert!(Emission::issue_for_author(&2).is_err());
        assert_eq!(snapshot(), before);
    });
}
#[test]
fn second_recipient_failure_rolls_back_first_credit_ledger_and_events() {
    ext(CAP - 5).execute_with(|| {
        assert!(Balances::force_set_balance(RuntimeOrigin::root(), 99, 0).is_ok());
        let before = snapshot();
        assert!(Emission::issue_for_author(&2).is_err());
        assert_eq!(snapshot(), before);
    });
}
#[test]
fn cap_burn_and_outer_rollback_preserve_once_only_gross_issuance() {
    ext(CAP - 5).execute_with(|| {
        let before = snapshot();
        let r: Result<(), DispatchError> = with_transaction(|| {
            assert!(Emission::issue_for_author(&2).is_ok());
            TransactionOutcome::Rollback(Ok(()))
        });
        assert!(r.is_ok());
        assert_eq!(snapshot(), before);
        assert!(Emission::issue_for_author(&2).is_ok());
        assert_eq!(Emission::state().unwrap().cumulative, CAP);
        let (burn, _) = Balances::slash(&1, 50);
        drop(burn);
        System::set_block_number(2);
        assert!(Emission::issue_for_author(&2).is_ok());
        assert_eq!(Balances::total_issuance(), CAP - 50);
        assert_eq!(Emission::state().unwrap().cumulative, CAP);
    });
}
#[test]
fn corrupted_counters_and_untracked_stock_fail_without_more_mint() {
    for corrupt in 0..3 {
        ext(3000).execute_with(|| {
            if corrupt == 2 {
                let c = Balances::deposit_creating(&1, 100);
                drop(c);
            } else {
                emission::Ledger::<Test>::mutate(|s| {
                    let s = s.as_mut().unwrap();
                    if corrupt == 0 {
                        s.cumulative += 1
                    } else {
                        s.period = 1
                    }
                });
            }
            let before = snapshot();
            assert!(Emission::issue_for_author(&2).is_err());
            assert_eq!(snapshot(), before);
        });
    }
}
#[test]
fn stored_reward_step_matches_all_periods_and_rejects_unbounded_cache() {
    let mut reward = INITIAL_REWARD;
    for p in 0..9999 {
        reward = advance_stored_reward(reward, p).unwrap();
        assert_eq!(reward, period_reward(p + 1).unwrap());
    }
    assert!(advance_stored_reward(INITIAL_REWARD + 1, 0).is_err());
    assert!(advance_stored_reward(0, 9999).is_err());
}
#[test]
fn boundary_cache_transition_matches_canonical_reward() {
    // Construct this boundary from the independently recomputed canonical planning
    // prefix; no on-chain checkpoint import or skipped-height acceptance exists.
    ext(3000).execute_with(|| {
        let reward = INITIAL_REWARD;
        let blocks = BLOCKS_PER_PERIOD as u64;
        let ai = reward * 30 / 100;
        let base = reward - ai;
        let cumulative = 3000 + reward * u128::from(blocks);
        drop(Balances::deposit_creating(&2, base * u128::from(blocks)));
        drop(Balances::deposit_creating(&99, ai * u128::from(blocks)));
        assert_eq!(Balances::total_issuance(), cumulative);
        emission::Ledger::<Test>::put(emission::StateData {
            version: 1,
            last: blocks,
            genesis: 3000,
            cumulative,
            base: base * u128::from(blocks),
            ai: ai * u128::from(blocks),
            period: 0,
            reward,
        });
        System::set_block_number(blocks + 1);
        assert!(Emission::issue_for_author(&2).is_ok());
        let s = Emission::state().unwrap();
        assert_eq!(s.period, 1);
        assert_eq!(s.reward, period_reward(1).unwrap());
        assert_eq!(s.cumulative, cumulative + s.reward);
        assert_eq!(Balances::total_issuance(), s.cumulative);
    });
}
#[test]
fn malformed_storage_is_retained_and_no_dispatchable_emission_call_exists() {
    ext(3000).execute_with(|| {
        sp_io::storage::set(&emission::Ledger::<Test>::hashed_key(), &[255, 1, 2]);
        let before = snapshot();
        assert!(Emission::issue_for_author(&2).is_err());
        assert_eq!(snapshot(), before);
        assert!(RuntimeCall::decode_all(&mut &[2, 0][..]).is_err());
    });
}
#[test]
fn genesis_must_match_existing_stock_and_cannot_exceed_cap() {
    for genesis in [2999, CAP + 1] {
        let result = std::panic::catch_unwind(|| {
            let mut s = frame_system::GenesisConfig::<Test>::default()
                .build_storage()
                .unwrap();
            pallet_balances::GenesisConfig::<Test> {
                balances: vec![(1, 3000)],
                ..Default::default()
            }
            .assimilate_storage(&mut s)
            .unwrap();
            emission::GenesisConfig::<Test> {
                genesis_issued: genesis,
                ..Default::default()
            }
            .assimilate_storage(&mut s)
            .unwrap();
        });
        assert!(result.is_err());
    }
}
#[test]
fn nonexistent_author_below_minimum_cannot_commit_unpaid_emission() {
    ext(CAP - 5).execute_with(|| {
        let before = snapshot();
        let account = System::account(3);
        assert!(Emission::issue_for_author(&3).is_err());
        assert_eq!(snapshot(), before);
        assert_eq!(System::account(3), account);
    });
}
#[test]
fn trailing_storage_bytes_must_not_be_silently_accepted() {
    ext(3000).execute_with(|| {
        let mut bytes = Emission::state().unwrap().encode();
        bytes.push(0);
        sp_io::storage::set(&emission::Ledger::<Test>::hashed_key(), &bytes);
        let before = snapshot();
        assert!(Emission::issue_for_author(&2).is_err());
        assert_eq!(snapshot(), before);
    });
}
#[test]
fn acceptance_record_rejects_stale_or_invalid_successors_before_issuance() {
    ext(3000).execute_with(|| {
        let mut work = [0; 32];
        work[0] = 2;
        let anchor = emission::AcceptanceData {
            version: 1,
            height: 0,
            header: [0; 108],
            work,
            policy: [7; 32],
        };
        Emission::initialize_acceptance(anchor.clone()).unwrap();
        assert!(Emission::initialize_acceptance(anchor.clone()).is_err());
        let mut child = anchor.clone();
        child.height = 1;
        child.work[0] = 4;
        let id = sp_io::hashing::sha2_256(&sp_io::hashing::sha2_256(&anchor.header));
        child.header[4..36].copy_from_slice(&id);
        for case in 0..6 {
            let mut expected = anchor.clone();
            let mut next = child.clone();
            match case {
                0 => expected.work[0] ^= 1,
                1 => next.height = 2,
                2 => next.policy = [8; 32],
                3 => next.work = anchor.work,
                4 => next.header[4] ^= 1,
                _ => next.version = 2,
            }
            let before = snapshot();
            assert!(Emission::issue_accepted(&expected, next, &2).is_err());
            assert_eq!(snapshot(), before);
        }
    });
}

#[test]
fn initialized_or_malformed_acceptance_blocks_unverified_legacy_issuance() {
    for malformed in [false, true] {
        ext(3000).execute_with(|| {
            let mut work = [0; 32];
            work[0] = 2;
            let anchor = emission::AcceptanceData {
                version: 1,
                height: 0,
                header: [0; 108],
                work,
                policy: [7; 32],
            };
            Emission::initialize_acceptance(anchor.clone()).unwrap();
            if malformed {
                sp_io::storage::set(&emission::Acceptance::<Test>::hashed_key(), &[255]);
            }
            let before = snapshot();
            assert!(
                Emission::issue_for_author(&2).is_err(),
                "legacy issuance must refuse once any acceptance record exists"
            );
            assert_eq!(snapshot(), before);
            if !malformed {
                let mut next = anchor.clone();
                next.height = 1;
                next.work[0] = 4;
                let id = sp_io::hashing::sha2_256(&sp_io::hashing::sha2_256(&anchor.header));
                next.header[4..36].copy_from_slice(&id);
                Emission::issue_accepted(&anchor, next, &2).unwrap();
                assert_eq!(Emission::state().unwrap().last, 1);
                assert_eq!(Emission::read_acceptance().unwrap().height, 1);
                System::set_block_number(2);
                let before = snapshot();
                assert!(Emission::issue_for_author(&2).is_err());
                assert_eq!(snapshot(), before);
            }
        });
    }
}
