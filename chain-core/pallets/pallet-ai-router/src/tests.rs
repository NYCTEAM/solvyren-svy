use crate::{self as pallet_ai_router, *};
use frame_support::{assert_noop, assert_ok, derive_impl, parameter_types, weights::Weight};
use sp_runtime::BuildStorage;

type Block = frame_system::mocking::MockBlock<Test>;
frame_support::construct_runtime!(
    pub enum Test { System: frame_system, Balances: pallet_balances, Router: pallet_ai_router }
);
#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
    type Block = Block;
    type AccountData = pallet_balances::AccountData<u64>;
}
#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Test {
    type AccountStore = System;
}
parameter_types! { pub const MaxNodes: u32 = 3; pub const Ttl: u64 = 3; pub static DevFundingEnabled: bool = true; }
parameter_types! { pub static ApprovalRequired: bool = false; }
pub struct TestWeights;
impl WeightInfo for TestWeights {
    fn report() -> Weight {
        Weight::zero()
    }
    fn route(_: u32) -> Weight {
        Weight::zero()
    }
    fn settle() -> Weight {
        Weight::zero()
    }
}
impl Config for Test {
    type Funding = Prefunded;
    type Currency = Balances;
    type DevFundingEnabled = DevFundingEnabled;
    type RequireDeviceApproval = ApprovalRequired;
    type RewardAccount = frame_support::traits::ConstU64<99>;
    type TaskReward = frame_support::traits::ConstU64<10>;
    type TaskOrigin = frame_system::EnsureRoot<u64>;
    type MaxNodes = MaxNodes;
    type HeartbeatTtl = Ttl;
    type WeightInfo = TestWeights;
}
fn ext() -> sp_io::TestExternalities {
    let mut storage = frame_system::GenesisConfig::<Test>::default()
        .build_storage()
        .unwrap();
    pallet_balances::GenesisConfig::<Test> {
        balances: vec![(99, 10000), (1, 100), (2, 100), (3, 100)],
        ..Default::default()
    }
    .assimilate_storage(&mut storage)
    .unwrap();
    storage.into()
}

#[test]
fn disabled_dev_funding_rejects_configuration_and_existing_plan() {
    use frame_support::traits::Hooks;
    DevFundingEnabled::set(false);
    ext().execute_with(|| {
        let plan = FundingPlan {
            source: 1,
            block_budget: 10,
        };
        assert_noop!(
            Router::configure_dev_funding(RuntimeOrigin::root(), Some(plan.clone())),
            Error::<Test>::InvalidReward
        );
        Router::on_initialize(1);
        assert_eq!(Balances::free_balance(1), 100);
        DevFundingPlan::<Test>::put(plan);
        assert!(std::panic::catch_unwind(|| Router::on_initialize(2)).is_err());
        assert_eq!(Balances::free_balance(1), 100);
        assert!(DevFundingPlan::<Test>::get().is_some());
    });
    DevFundingEnabled::set(true);
}

#[test]
fn live_approval_gates_registration_and_both_assignment_paths() {
    ApprovalRequired::set(true);
    ext().execute_with(|| {
        System::set_block_number(1);
        let device = frame_support::BoundedVec::try_from(b"verified-device".to_vec()).unwrap();
        let other = frame_support::BoundedVec::try_from(b"other-device".to_vec()).unwrap();
        AiOperator::<Test>::put(3);
        assert_noop!(Router::set_device_approval(RuntimeOrigin::signed(1), 1, Some(device.clone())),
            sp_runtime::DispatchError::BadOrigin);
        assert_noop!(Router::register_node(RuntimeOrigin::signed(1), 16, device.clone()),
            Error::<Test>::DeviceNotApproved);
        assert_noop!(Router::report_hardware(RuntimeOrigin::signed(1), 16, 16, true),
            Error::<Test>::DeviceNotApproved);
        assert_ok!(Router::set_device_approval(RuntimeOrigin::signed(3), 1, Some(device.clone())));
        assert_noop!(Router::register_node(RuntimeOrigin::signed(1), 16, other),
            Error::<Test>::DeviceNotApproved);
        assert_ok!(Router::register_node(RuntimeOrigin::signed(1), 16, device));
        assert_ok!(Router::heartbeat(RuntimeOrigin::signed(1), 16, true));
        assert_ok!(Router::submit_ai_task(RuntimeOrigin::signed(3), 1, 8));
        assert_ok!(Router::settle_task(RuntimeOrigin::signed(3), 0));
        assert_ok!(Router::heartbeat(RuntimeOrigin::signed(1), 16, true));
        assert_ok!(Router::set_device_approval(RuntimeOrigin::signed(3), 1, None));
        assert_noop!(Router::submit_ai_task(RuntimeOrigin::signed(3), 1, 8),
            Error::<Test>::NoEligibleMiner);
        assert_noop!(Router::submit_bound_ai_task(RuntimeOrigin::signed(3), [1; 32], 1, 16, 8, 1),
            Error::<Test>::DeviceNotApproved);
    });
    ApprovalRequired::set(false);
}

#[test]
fn escrow_result_attestation_and_payment_are_atomic() {
    ext().execute_with(|| {
        System::set_block_number(1);
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            22 * GIB,
            true
        ));
        assert_eq!(Router::route_ai_task(1, GIB).unwrap(), 1);
        assert_eq!(Balances::reserved_balance(99), 10);
        assert_noop!(
            Router::submit_result(RuntimeOrigin::signed(2), 0, [7; 32]),
            Error::<Test>::NotAssignedMiner
        );
        assert_ok!(Router::submit_result(RuntimeOrigin::signed(1), 0, [7; 32]));
        assert_noop!(
            Router::verify_result(RuntimeOrigin::root(), 0, [8; 32]),
            Error::<Test>::NoResult
        );
        assert_eq!(Balances::reserved_balance(99), 10);
        assert!(GpuNodeRegistry::<Test>::get(1).unwrap().busy);
        assert_ok!(Router::verify_result(RuntimeOrigin::root(), 0, [7; 32]));
        assert_eq!(Balances::reserved_balance(99), 0);
        assert_eq!(Balances::free_balance(1), 110);
        assert_eq!(CompletedResults::<Test>::get(0), Some([7; 32]));
        assert_noop!(
            Router::verify_result(RuntimeOrigin::root(), 0, [7; 32]),
            Error::<Test>::NoResult
        );
    });
}

#[test]
fn cancellation_refunds_escrow_and_insufficient_funding_cannot_reserve_worker() {
    use frame_support::traits::Currency;
    ext().execute_with(|| {
        System::set_block_number(1);
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            22 * GIB,
            true
        ));
        assert_ok!(Router::submit_ai_task(RuntimeOrigin::root(), 1, GIB));
        assert_ok!(Router::settle_task(RuntimeOrigin::root(), 0));
        assert_eq!(Balances::free_balance(99), 10000);
        assert_eq!(Balances::reserved_balance(99), 0);
        assert_ok!(Router::heartbeat(RuntimeOrigin::signed(1), 22 * GIB, true));
        let _ = Balances::make_free_balance_be(&99, 1);
        assert!(Router::route_ai_task(1, GIB).is_err());
        assert!(!GpuNodeRegistry::<Test>::get(1).unwrap().busy);
        assert_eq!(NextTaskId::<Test>::get(), 1);
    });
}
const GIB: u64 = 1024 * 1024 * 1024;

#[test]
fn concurrent_task_rewards_and_cancellations_preserve_other_escrows() {
    ext().execute_with(|| {
        System::set_block_number(1);
        let issuance = Balances::total_issuance();
        for miner in 1..=3 {
            assert_ok!(Router::report_hardware(
                RuntimeOrigin::signed(miner),
                24 * GIB,
                22 * GIB,
                true
            ));
        }
        for (miner, request, reward) in [(1, [11; 32], 25), (2, [12; 32], 40), (3, [13; 32], 60)] {
            assert_ok!(Router::submit_bound_ai_task(
                RuntimeOrigin::root(),
                request,
                miner,
                16,
                GIB,
                reward
            ));
        }
        assert_eq!(Balances::reserved_balance(99), 125);
        assert_eq!(Balances::free_balance(99), 9875);
        assert_ok!(Router::submit_result(RuntimeOrigin::signed(1), 0, [21; 32]));
        assert_ok!(Router::verify_result_with_reward(
            RuntimeOrigin::root(),
            0,
            [21; 32],
            15
        ));
        assert_eq!(Balances::free_balance(1), 115);
        assert_eq!(Balances::reserved_balance(99), 100);
        assert_eq!(Balances::free_balance(99), 9885);
        assert!(GpuNodeRegistry::<Test>::get(2).unwrap().busy);
        assert!(GpuNodeRegistry::<Test>::get(3).unwrap().busy);

        assert_ok!(Router::settle_task(RuntimeOrigin::root(), 2));
        assert_eq!(Balances::reserved_balance(99), 40);
        assert_eq!(Balances::free_balance(99), 9945);
        assert_eq!(Balances::free_balance(3), 100);
        assert_eq!(PaidRewards::<Test>::get(2), None);
        assert!(ActiveTasks::<Test>::contains_key(1));
        assert_ok!(Router::submit_result(RuntimeOrigin::signed(2), 1, [22; 32]));
        assert_noop!(
            Router::settle_task(RuntimeOrigin::root(), 1),
            Error::<Test>::ResultAlreadySubmitted
        );
        assert_noop!(
            Router::verify_result_with_reward(RuntimeOrigin::root(), 1, [22; 32], 41),
            Error::<Test>::InvalidReward
        );
        assert_ok!(Router::verify_result_with_reward(
            RuntimeOrigin::root(),
            1,
            [22; 32],
            30
        ));

        assert_eq!(Balances::reserved_balance(99), 0);
        assert_eq!(Balances::free_balance(99), 9955);
        assert_eq!(Balances::free_balance(2), 130);
        assert_eq!(Balances::total_issuance(), issuance);
        assert_eq!(PaidRewards::<Test>::get(0), Some(15));
        assert_eq!(PaidRewards::<Test>::get(1), Some(30));
        assert_eq!(ActiveTasks::<Test>::iter().count(), 0);
        for (task, request) in [(0, [11; 32]), (1, [12; 32]), (2, [13; 32])] {
            assert_eq!(RequestTasks::<Test>::get(request), Some(task));
            assert!(BoundTaskTerms::<Test>::contains_key(task));
        }
        assert_noop!(
            Router::verify_result_with_reward(RuntimeOrigin::root(), 0, [21; 32], 15),
            Error::<Test>::NoResult
        );
        assert_eq!(Balances::free_balance(1), 115);
    });
}

#[test]
fn depleted_aggregate_escrow_cannot_partially_pay_or_cancel_a_task() {
    use frame_support::traits::ReservableCurrency;
    ext().execute_with(|| {
        System::set_block_number(1);
        for (miner, request, reward) in [(1, [31; 32], 25), (2, [32; 32], 40)] {
            assert_ok!(Router::report_hardware(
                RuntimeOrigin::signed(miner),
                24 * GIB,
                22 * GIB,
                true
            ));
            assert_ok!(Router::submit_bound_ai_task(
                RuntimeOrigin::root(),
                request,
                miner,
                16,
                GIB,
                reward
            ));
        }
        assert_ok!(Router::submit_result(RuntimeOrigin::signed(1), 0, [41; 32]));
        // Fault injection only: simulate an unrelated privileged reserve slash.
        // This is not an extrinsic sent to the running development chain.
        let (burned, remainder) = Balances::slash_reserved(&99, 50);
        assert_eq!(remainder, 0);
        drop(burned);
        assert_eq!(Balances::reserved_balance(99), 15);
        let pool_free = Balances::free_balance(99);
        let issuance = Balances::total_issuance();
        assert_noop!(
            Router::verify_result(RuntimeOrigin::root(), 0, [41; 32]),
            Error::<Test>::IncompletePayment
        );
        assert_noop!(
            Router::settle_task(RuntimeOrigin::root(), 1),
            Error::<Test>::IncompletePayment
        );
        assert_eq!(Balances::reserved_balance(99), 15);
        assert_eq!(Balances::free_balance(99), pool_free);
        assert_eq!(Balances::free_balance(1), 100);
        assert_eq!(Balances::free_balance(2), 100);
        assert_eq!(Balances::total_issuance(), issuance);
        assert_eq!(PendingResults::<Test>::get(0), Some([41; 32]));
        assert_eq!(PaidRewards::<Test>::get(0), None);
        assert_eq!(ActiveTasks::<Test>::iter().count(), 2);
        assert!(GpuNodeRegistry::<Test>::get(1).unwrap().busy);
        assert!(GpuNodeRegistry::<Test>::get(2).unwrap().busy);
    });
}

#[test]
fn committed_results_cannot_be_cancelled_or_lose_reserved_rewards() {
    for bound in [false, true] {
        ext().execute_with(|| {
            System::set_block_number(1);
            assert_ok!(Router::report_hardware(
                RuntimeOrigin::signed(1),
                24 * GIB,
                22 * GIB,
                true
            ));
            if bound {
                assert_ok!(Router::submit_bound_ai_task(
                    RuntimeOrigin::root(),
                    [6; 32],
                    1,
                    16,
                    GIB,
                    25
                ));
            } else {
                assert_ok!(Router::submit_ai_task_with_reward(
                    RuntimeOrigin::root(),
                    1,
                    GIB,
                    25
                ));
            }
            assert_ok!(Router::submit_result(RuntimeOrigin::signed(1), 0, [9; 32]));
            let assignment = ActiveTasks::<Test>::get(0).unwrap();
            let node = GpuNodeRegistry::<Test>::get(1).unwrap();
            let issuance = Balances::total_issuance();
            assert_noop!(
                Router::settle_task(RuntimeOrigin::root(), 0),
                Error::<Test>::ResultAlreadySubmitted
            );
            assert_eq!(ActiveTasks::<Test>::get(0), Some(assignment));
            assert_eq!(GpuNodeRegistry::<Test>::get(1), Some(node));
            assert_eq!(PendingResults::<Test>::get(0), Some([9; 32]));
            assert_eq!(Balances::reserved_balance(99), 25);
            assert_eq!(Balances::free_balance(99), 9975);
            assert_eq!(Balances::free_balance(1), 100);
            assert_eq!(Balances::total_issuance(), issuance);
            if bound {
                assert_eq!(RequestTasks::<Test>::get([6; 32]), Some(0));
                assert!(BoundTaskTerms::<Test>::contains_key(0));
            }
            // Failed cancellation cannot prevent a later authorized one-time payment.
            assert_ok!(Router::verify_result_with_reward(
                RuntimeOrigin::root(),
                0,
                [9; 32],
                15
            ));
            assert_eq!(Balances::free_balance(1), 115);
            assert_eq!(Balances::reserved_balance(99), 0);
            assert_eq!(PaidRewards::<Test>::get(0), Some(15));
            assert_noop!(
                Router::settle_task(RuntimeOrigin::root(), 0),
                Error::<Test>::UnknownTask
            );
            assert_eq!(Balances::free_balance(1), 115);
        });
    }
}

#[test]
fn prefunded_block_contributions_and_variable_reservations() {
    use frame_support::traits::Hooks;
    ext().execute_with(|| {
        System::set_block_number(1);
        let before = Balances::total_issuance();
        assert_eq!(DevFundingPlan::<Test>::get(), None);
        Router::on_initialize(1);
        assert_eq!(Balances::free_balance(99), 10000);
        assert_noop!(
            Router::configure_dev_funding(
                RuntimeOrigin::signed(1),
                Some(FundingPlan {
                    source: 1,
                    block_budget: 100
                })
            ),
            sp_runtime::DispatchError::BadOrigin
        );
        assert_ok!(Router::configure_dev_funding(
            RuntimeOrigin::root(),
            Some(FundingPlan {
                source: 1,
                block_budget: 100
            })
        ));
        Router::on_initialize(2);
        Router::on_initialize(3);
        assert_eq!(Balances::free_balance(99), 10060);
        assert_eq!(Balances::free_balance(1), 40);
        assert_eq!(Balances::total_issuance(), before);
        Router::on_initialize(4); // KeepAlive prevents draining the source.
        Router::on_initialize(5);
        assert_eq!(Balances::free_balance(99), 10090);
        assert_eq!(Balances::total_issuance(), before);
        assert_ok!(Router::configure_dev_funding(RuntimeOrigin::root(), None));
        Router::on_initialize(6);
        assert_eq!(Balances::free_balance(99), 10090);

        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(2),
            24 * GIB,
            22 * GIB,
            true
        ));
        assert_noop!(
            Router::submit_ai_task_with_reward(RuntimeOrigin::root(), 1, GIB, 0),
            Error::<Test>::InvalidReward
        );
        assert_ok!(Router::submit_ai_task_with_reward(
            RuntimeOrigin::root(),
            1,
            GIB,
            25
        ));
        assert_eq!(Balances::reserved_balance(99), 25);
        assert_ok!(Router::submit_result(RuntimeOrigin::signed(2), 0, [9; 32]));
        assert_noop!(
            Router::verify_result_with_reward(RuntimeOrigin::root(), 0, [9; 32], 26),
            Error::<Test>::InvalidReward
        );
        assert_ok!(Router::verify_result_with_reward(
            RuntimeOrigin::root(),
            0,
            [9; 32],
            15
        ));
        assert_eq!(Balances::free_balance(2), 115);
        assert_eq!(Balances::reserved_balance(99), 0);
        assert_eq!(Balances::free_balance(99), 10075);
        assert_noop!(
            Router::verify_result(RuntimeOrigin::root(), 0, [9; 32]),
            Error::<Test>::NoResult
        );
    });
}

#[test]
fn excludes_non_idle_and_selects_smallest_sufficient_card() {
    ext().execute_with(|| {
        System::set_block_number(1);
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            22 * GIB,
            false
        ));
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(2),
            48 * GIB,
            40 * GIB,
            true
        ));
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(3),
            24 * GIB,
            23 * GIB,
            true
        ));
        assert_eq!(Router::route_ai_task(1, 22 * GIB).unwrap(), 3);
        assert_noop!(
            Router::remove_node(RuntimeOrigin::root(), 3),
            Error::<Test>::NodeBusy
        );
        assert_eq!(Router::route_ai_task(1, 22 * GIB).unwrap(), 2);
    });
}

#[test]
fn routes_22_gib_and_reserves_until_trusted_settlement() {
    ext().execute_with(|| {
        System::set_block_number(1);
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            23 * GIB,
            true
        ));
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(2),
            16 * GIB,
            16 * GIB,
            true
        ));
        assert_eq!(Router::route_ai_task(1, 22 * GIB).unwrap(), 1);
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            24 * GIB,
            true
        ));
        assert_noop!(
            Router::route_ai_task(1, 22 * GIB),
            Error::<Test>::NoEligibleMiner
        );
        assert_noop!(
            Router::settle_task(RuntimeOrigin::signed(1), 0),
            sp_runtime::DispatchError::BadOrigin
        );
        assert_ok!(Router::settle_task(RuntimeOrigin::root(), 0));
        assert_noop!(
            Router::route_ai_task(1, 22 * GIB),
            Error::<Test>::NoEligibleMiner
        );
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            24 * GIB,
            true
        ));
        assert_eq!(Router::route_ai_task(1, 22 * GIB).unwrap(), 1);
    });
}

#[test]
fn stale_nodes_and_unauthorized_submissions_are_rejected() {
    ext().execute_with(|| {
        System::set_block_number(1);
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            24 * GIB,
            true
        ));
        System::set_block_number(5);
        assert_noop!(
            Router::route_ai_task(1, 22 * GIB),
            Error::<Test>::NoEligibleMiner
        );
        assert_noop!(
            Router::submit_ai_task(RuntimeOrigin::signed(9), 1, GIB),
            sp_runtime::DispatchError::BadOrigin
        );
        assert_eq!(NextTaskId::<Test>::get(), 0);
    });
}

#[test]
fn heartbeat_ttl_boundary_is_identical_for_legacy_and_bound_routing() {
    for bound in [false, true] {
        ext().execute_with(|| {
            System::set_block_number(1);
            assert_ok!(Router::report_hardware(
                RuntimeOrigin::signed(1),
                24 * GIB,
                22 * GIB,
                true
            ));
            // Age exactly TTL is eligible in both entry points.
            System::set_block_number(4);
            if bound {
                assert_ok!(Router::submit_bound_ai_task(
                    RuntimeOrigin::root(),
                    [61; 32],
                    1,
                    16,
                    GIB,
                    10
                ));
            } else {
                assert_eq!(Router::route_ai_task(1, GIB).unwrap(), 1);
            }
            assert_eq!(NextTaskId::<Test>::get(), 1);
            assert_eq!(Balances::reserved_balance(99), 10);
            assert!(GpuNodeRegistry::<Test>::get(1).unwrap().busy);
        });
        for future in [false, true] {
            ext().execute_with(|| {
                System::set_block_number(1);
                assert_ok!(Router::report_hardware(
                    RuntimeOrigin::signed(1),
                    24 * GIB,
                    22 * GIB,
                    true
                ));
                System::set_block_number(5);
                if future {
                    // Fault injection: a future-dated storage entry must not underflow age.
                    GpuNodeRegistry::<Test>::mutate(1, |node| node.as_mut().unwrap().last_seen = 6);
                }
                if bound {
                    assert_noop!(
                        Router::submit_bound_ai_task(
                            RuntimeOrigin::root(),
                            [62; 32],
                            1,
                            16,
                            GIB,
                            10
                        ),
                        Error::<Test>::NoEligibleMiner
                    );
                    assert!(!RequestTasks::<Test>::contains_key([62; 32]));
                } else {
                    assert_noop!(
                        Router::route_ai_task(1, GIB),
                        Error::<Test>::NoEligibleMiner
                    );
                }
                assert_eq!(NextTaskId::<Test>::get(), 0);
                assert_eq!(Balances::reserved_balance(99), 0);
                assert_eq!(Balances::free_balance(99), 10000);
                assert_eq!(ActiveTasks::<Test>::iter().count(), 0);
                assert!(!GpuNodeRegistry::<Test>::get(1).unwrap().busy);
            });
        }
    }
}

#[test]
fn expired_busy_worker_keeps_escrow_and_can_finish_without_reassignment() {
    ext().execute_with(|| {
        System::set_block_number(1);
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            22 * GIB,
            true
        ));
        assert_ok!(Router::submit_bound_ai_task(
            RuntimeOrigin::root(),
            [63; 32],
            1,
            16,
            GIB,
            25
        ));
        let issuance = Balances::total_issuance();
        System::set_block_number(100);
        assert_eq!(Balances::reserved_balance(99), 25);
        assert!(GpuNodeRegistry::<Test>::get(1).unwrap().busy);
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [64; 32], 1, 16, GIB, 25),
            Error::<Test>::NoEligibleMiner
        );
        // A fresh heartbeat can refresh memory but cannot remove the original busy lock.
        assert_ok!(Router::heartbeat(RuntimeOrigin::signed(1), 22 * GIB, true));
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [64; 32], 1, 16, GIB, 25),
            Error::<Test>::NoEligibleMiner
        );
        // Completion is still valid if that refreshed heartbeat expires again.
        System::set_block_number(200);
        assert_ok!(Router::submit_result(RuntimeOrigin::signed(1), 0, [65; 32]));
        assert_ok!(Router::verify_result_with_reward(
            RuntimeOrigin::root(),
            0,
            [65; 32],
            15
        ));
        assert_eq!(PaidRewards::<Test>::get(0), Some(15));
        assert_eq!(Balances::reserved_balance(99), 0);
        assert_eq!(Balances::free_balance(1), 115);
        assert_eq!(Balances::free_balance(99), 9985);
        assert_eq!(Balances::total_issuance(), issuance);
        assert!(!GpuNodeRegistry::<Test>::get(1).unwrap().busy);
        assert_eq!(GpuNodeRegistry::<Test>::get(1).unwrap().free_vram, 0);
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [64; 32], 1, 16, GIB, 25),
            Error::<Test>::NoEligibleMiner
        );
        assert_ok!(Router::heartbeat(RuntimeOrigin::signed(1), 22 * GIB, true));
        assert_ok!(Router::submit_bound_ai_task(
            RuntimeOrigin::root(),
            [64; 32],
            1,
            16,
            GIB,
            25
        ));
        assert_eq!(RequestTasks::<Test>::get([63; 32]), Some(0));
        assert_eq!(RequestTasks::<Test>::get([64; 32]), Some(1));
        assert_noop!(
            Router::verify_result_with_reward(RuntimeOrigin::root(), 0, [65; 32], 15),
            Error::<Test>::NoResult
        );
    });
}

#[test]
fn admission_memory_and_task_id_are_bounded() {
    ext().execute_with(|| {
        System::set_block_number(1);
        assert_noop!(
            Router::report_hardware(RuntimeOrigin::signed(1), GIB, 2 * GIB, true),
            Error::<Test>::InvalidMemory
        );
        for id in 1..=3 {
            assert_ok!(Router::report_hardware(
                RuntimeOrigin::signed(id),
                24 * GIB,
                24 * GIB,
                true
            ));
        }
        assert_noop!(
            Router::report_hardware(RuntimeOrigin::signed(4), GIB, GIB, true),
            Error::<Test>::RegistryFull
        );
        NextTaskId::<Test>::put(u64::MAX);
        assert_noop!(Router::route_ai_task(1, GIB), Error::<Test>::TaskIdOverflow);
        assert!(!GpuNodeRegistry::<Test>::get(1).unwrap().busy);
        assert_ok!(Router::remove_node(RuntimeOrigin::root(), 1));
        assert_eq!(NodeCount::<Test>::get(), 2);
    });
}

#[test]
fn bound_task_requires_scheduler_and_exact_selected_worker() {
    ext().execute_with(|| {
        System::set_block_number(1);
        for id in 1..=2 {
            assert_ok!(Router::report_hardware(
                RuntimeOrigin::signed(id),
                24 * GIB,
                22 * GIB,
                true
            ));
        }
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::signed(1), [1; 32], 2, 16, GIB, 25),
            sp_runtime::DispatchError::BadOrigin
        );
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [1; 32], 2, 0, GIB, 25),
            Error::<Test>::InvalidTaskType
        );
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [0; 32], 2, 16, GIB, 25),
            Error::<Test>::InvalidRequestHash
        );
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [1; 32], 9, 16, GIB, 25),
            Error::<Test>::UnknownNode
        );
        assert_ok!(Router::submit_bound_ai_task(
            RuntimeOrigin::root(),
            [1; 32],
            2,
            16,
            GIB,
            25
        ));
        let terms = BoundTaskTerms::<Test>::get(0).unwrap();
        assert_eq!(terms, ActiveTasks::<Test>::get(0).unwrap());
        assert_eq!(
            (
                terms.miner,
                terms.task_type,
                terms.required_vram,
                terms.reward
            ),
            (2, 16, GIB, 25)
        );
        assert_eq!(RequestTasks::<Test>::get([1; 32]), Some(0));
        assert_eq!(TaskRequests::<Test>::get(0), Some([1; 32]));
        assert!(!GpuNodeRegistry::<Test>::get(1).unwrap().busy);
        assert_noop!(
            Router::submit_result(RuntimeOrigin::signed(1), 0, [9; 32]),
            Error::<Test>::NotAssignedMiner
        );
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [2; 32], 2, 16, GIB, 25),
            Error::<Test>::NoEligibleMiner
        );
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [1; 32], 1, 16, GIB, 25),
            Error::<Test>::RequestAlreadyBound
        );
        assert_ok!(Router::submit_result(RuntimeOrigin::signed(2), 0, [9; 32]));
        assert_noop!(
            Router::verify_result_with_reward(RuntimeOrigin::signed(2), 0, [9; 32], 15),
            sp_runtime::DispatchError::BadOrigin
        );
        assert_noop!(
            Router::verify_result_with_reward(RuntimeOrigin::root(), 0, [9; 32], 26),
            Error::<Test>::InvalidReward
        );
        assert_ok!(Router::verify_result_with_reward(
            RuntimeOrigin::root(),
            0,
            [9; 32],
            15
        ));
        assert_eq!(PaidRewards::<Test>::get(0), Some(15));
        assert_eq!(Balances::free_balance(2), 115);
        assert_eq!(Balances::reserved_balance(99), 0);
        assert_eq!(Balances::free_balance(99), 9985);
        assert!(ActiveTasks::<Test>::get(0).is_none());
        assert_eq!(BoundTaskTerms::<Test>::get(0).unwrap().reward, 25);
        assert_noop!(
            Router::verify_result_with_reward(RuntimeOrigin::root(), 0, [9; 32], 15),
            Error::<Test>::NoResult
        );
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [1; 32], 1, 16, GIB, 25),
            Error::<Test>::RequestAlreadyBound
        );
    });
}

#[test]
fn bound_task_cancellation_preserves_replay_protection() {
    ext().execute_with(|| {
        System::set_block_number(1);
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            22 * GIB,
            true
        ));
        assert_ok!(Router::submit_bound_ai_task(
            RuntimeOrigin::root(),
            [3; 32],
            1,
            16,
            GIB,
            25
        ));
        assert_noop!(
            Router::settle_task(RuntimeOrigin::signed(1), 0),
            sp_runtime::DispatchError::BadOrigin
        );
        assert_ok!(Router::settle_task(RuntimeOrigin::root(), 0));
        assert_eq!(Balances::reserved_balance(99), 0);
        assert_eq!(Balances::free_balance(99), 10000);
        assert_eq!(PaidRewards::<Test>::get(0), None);
        assert_eq!(RequestTasks::<Test>::get([3; 32]), Some(0));
        assert_eq!(TaskRequests::<Test>::get(0), Some([3; 32]));
        assert!(BoundTaskTerms::<Test>::get(0).is_some());
        assert_noop!(
            Router::submit_result(RuntimeOrigin::signed(1), 0, [9; 32]),
            Error::<Test>::UnknownTask
        );
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [3; 32], 1, 16, GIB, 25),
            Error::<Test>::RequestAlreadyBound
        );
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [4; 32], 1, 16, GIB, 25),
            Error::<Test>::NoEligibleMiner
        );
    });
}

#[test]
fn bound_task_failure_rolls_back_all_reservations_and_bindings() {
    ext().execute_with(|| {
        System::set_block_number(1);
        assert_ok!(Router::report_hardware(
            RuntimeOrigin::signed(1),
            24 * GIB,
            22 * GIB,
            true
        ));
        assert!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [4; 32], 1, 16, GIB, 10001)
                .is_err()
        );
        assert_eq!(NextTaskId::<Test>::get(), 0);
        assert_eq!(Balances::reserved_balance(99), 0);
        assert_eq!(RequestTasks::<Test>::get([4; 32]), None);
        assert_eq!(TaskRequests::<Test>::get(0), None);
        assert_eq!(BoundTaskTerms::<Test>::get(0), None);
        assert!(!GpuNodeRegistry::<Test>::get(1).unwrap().busy);
        System::set_block_number(5);
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [4; 32], 1, 16, GIB, 25),
            Error::<Test>::NoEligibleMiner
        );
        assert_ok!(Router::heartbeat(RuntimeOrigin::signed(1), 22 * GIB, false));
        assert_ok!(Router::submit_bound_ai_task(
            RuntimeOrigin::root(),
            [4; 32],
            1,
            16,
            GIB,
            25
        ));
        assert_ok!(Router::settle_task(RuntimeOrigin::root(), 0));
        assert_ok!(Router::heartbeat(RuntimeOrigin::signed(1), GIB / 2, true));
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [5; 32], 1, 16, GIB, 25),
            Error::<Test>::NoEligibleMiner
        );
        assert_ok!(Router::heartbeat(RuntimeOrigin::signed(1), 22 * GIB, true));
        NextTaskId::<Test>::put(u64::MAX);
        assert_noop!(
            Router::submit_bound_ai_task(RuntimeOrigin::root(), [5; 32], 1, 16, GIB, 25),
            Error::<Test>::TaskIdOverflow
        );
        assert_eq!(Balances::reserved_balance(99), 0);
        assert!(!RequestTasks::<Test>::contains_key([5; 32]));
    });
}
