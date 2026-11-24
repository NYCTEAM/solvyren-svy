// This is free and unencumbered software released into the public domain.
//
// Anyone is free to copy, modify, publish, use, compile, sell, or
// distribute this software, either in source code form or as a compiled
// binary, for any purpose, commercial or non-commercial, and by any
// means.
//
// In jurisdictions that recognize copyright laws, the author or authors
// of this software dedicate any and all copyright interest in the
// software to the public domain. We make this dedication for the benefit
// of the public at large and to the detriment of our heirs and
// successors. We intend this dedication to be an overt act of
// relinquishment in perpetuity of all present and future rights to this
// software under copyright law.
//
// THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND,
// EXPRESS OR IMPLIED, INCLUDING BUT NOT LIMITED TO THE WARRANTIES OF
// MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT.
// IN NO EVENT SHALL THE AUTHORS BE LIABLE FOR ANY CLAIM, DAMAGES OR
// OTHER LIABILITY, WHETHER IN AN ACTION OF CONTRACT, TORT OR OTHERWISE,
// ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE USE OR
// OTHER DEALINGS IN THE SOFTWARE.
//
// For more information, please refer to <http://unlicense.org>

// Substrate and Polkadot dependencies
use frame_support::{
    derive_impl, parameter_types,
    traits::{
        ConstBool, ConstU128, ConstU32, ConstU64, ConstU8, Contains, Imbalance, OnUnbalanced,
        VariantCountOf,
    },
    weights::{
        constants::{RocksDbWeight, WEIGHT_REF_TIME_PER_SECOND},
        IdentityFee, Weight,
    },
};
use frame_system::limits::{BlockLength, BlockWeights};
use pallet_transaction_payment::{ConstFeeMultiplier, FungibleAdapter, Multiplier};
use sp_consensus_aura::sr25519::AuthorityId as AuraId;
use sp_runtime::{traits::One, Perbill};
use sp_version::RuntimeVersion;

parameter_types! {
    pub const MaxGpuNodes: u32 = 128;
    pub const GpuHeartbeatTtl: u32 = 3;
}

// Conservative development weights, not benchmark results. Public deployment
// requires measured weights and a separate admission/security review.
pub struct DevelopmentRouterWeights;
impl pallet_ai_router::WeightInfo for DevelopmentRouterWeights {
    fn report() -> Weight {
        Weight::from_parts(100_000_000, 8192)
    }
    fn route(max_nodes: u32) -> Weight {
        Weight::from_parts(100_000_000, 8192).saturating_add(
            Weight::from_parts(1_000_000_000, 1024).saturating_mul(max_nodes.into()),
        )
    }
    fn settle() -> Weight {
        Weight::from_parts(100_000_000, 8192)
    }
}

#[cfg(feature = "svy-matrix-live")]
pub struct AcceptedTaskIssuance;
#[cfg(feature = "svy-matrix-live")]
impl pallet_ai_router::TaskFunding<Runtime> for AcceptedTaskIssuance {
    fn reserve(maximum: Balance) -> frame_support::dispatch::DispatchResult {
        crate::PowRewards::reserve_ai_budget(maximum)
    }
    fn finish(miner: &AccountId, maximum: Balance, actual: Balance) -> frame_support::dispatch::DispatchResult {
        crate::PowRewards::finish_ai_budget(miner, maximum, actual)
    }
}

impl pallet_ai_router::Config for Runtime {
    #[cfg(feature = "svy-matrix-live")]
    type Funding = AcceptedTaskIssuance;
    #[cfg(not(feature = "svy-matrix-live"))]
    type Funding = pallet_ai_router::Prefunded;
    type Currency = Balances;
    type RewardAccount = AiRewardAccount;
    type TaskReward = AiTaskReward;
    type RequireDeviceApproval = ConstBool<{ cfg!(feature = "svy-matrix-live") }>;
    type TaskOrigin = frame_system::EnsureRoot<AccountId>;
    #[cfg(feature = "gpu-pow-rewards-dev")]
    type DevFundingEnabled = ConstBool<false>;
    #[cfg(not(feature = "gpu-pow-rewards-dev"))]
    type DevFundingEnabled = ConstBool<true>;
    type MaxNodes = MaxGpuNodes;
    type HeartbeatTtl = GpuHeartbeatTtl;
    type WeightInfo = DevelopmentRouterWeights;
}

parameter_types! {
    pub AiRewardAccount: AccountId = super::reward_account();
    pub const AiTaskReward: Balance = super::UNIT;
}

// Local module imports
#[cfg(not(feature = "gpu-pow-v2"))]
use super::Aura;
use super::{
    AccountId, Balance, Balances, Block, BlockNumber, Hash, Nonce, PalletInfo, Runtime,
    RuntimeCall, RuntimeEvent, RuntimeFreezeReason, RuntimeHoldReason, RuntimeOrigin, RuntimeTask,
    System, EXISTENTIAL_DEPOSIT, SLOT_DURATION, VERSION,
};

const NORMAL_DISPATCH_RATIO: Perbill = Perbill::from_percent(75);

parameter_types! {
    pub const BlockHashCount: BlockNumber = 2400;
    pub const Version: RuntimeVersion = VERSION;

    /// We allow for 2 seconds of compute with a 6 second average block time.
    pub RuntimeBlockWeights: BlockWeights = BlockWeights::with_sensible_defaults(
        Weight::from_parts(2u64 * WEIGHT_REF_TIME_PER_SECOND, u64::MAX),
        NORMAL_DISPATCH_RATIO,
    );
    pub RuntimeBlockLength: BlockLength = BlockLength::max_with_normal_ratio(5 * 1024 * 1024, NORMAL_DISPATCH_RATIO);
    pub const SS58Prefix: u8 = 42;
}

/// The default types are being injected by [`derive_impl`](`frame_support::derive_impl`) from
/// [`SoloChainDefaultConfig`](`struct@frame_system::config_preludes::SolochainDefaultConfig`),
/// but overridden as needed.
#[derive_impl(frame_system::config_preludes::SolochainDefaultConfig)]
impl frame_system::Config for Runtime {
    /// The block type for the runtime.
    type Block = Block;
    /// Block & extrinsics weights: base values and limits.
    type BlockWeights = RuntimeBlockWeights;
    /// The maximum length of a block (in bytes).
    type BlockLength = RuntimeBlockLength;
    /// The identifier used to distinguish between accounts.
    type AccountId = AccountId;
    /// The type for storing how many extrinsics an account has signed.
    type Nonce = Nonce;
    /// The type for hashing blocks and tries.
    type Hash = Hash;
    /// Maximum number of block number to block hash mappings to keep (oldest pruned first).
    type BlockHashCount = BlockHashCount;
    /// The weight of database operations that the runtime can invoke.
    type DbWeight = RocksDbWeight;
    /// Version of the runtime.
    type Version = Version;
    /// The data to be stored in an account.
    type AccountData = pallet_balances::AccountData<Balance>;
    /// This is used as an identifier of the chain. 42 is the generic substrate prefix.
    type SS58Prefix = SS58Prefix;
    type MaxConsumers = frame_support::traits::ConstU32<16>;
    type BaseCallFilter = RewardRuntimeCallFilter;
}

pub struct RewardRuntimeCallFilter;

impl Contains<RuntimeCall> for RewardRuntimeCallFilter {
    fn contains(call: &RuntimeCall) -> bool {
        #[cfg(not(feature = "gpu-pow-rewards-dev"))]
        {
            let _ = call;
            true
        }
        #[cfg(feature = "gpu-pow-rewards-dev")]
        {
            // frame-system call index 8 is experimental `do_task`. Its Rust
            // variant is absent when the dependency feature is disabled,
            // including some Wasm builds. Deny the encoded index in either
            // feature configuration, and review this if frame-system changes.
            if let RuntimeCall::System(system_call) = call {
                if codec::Encode::encode(system_call).first() == Some(&8) {
                    return false;
                }
            }
            #[cfg(feature = "svy-matrix-live")]
            if let RuntimeCall::AiRouter(router_call) = call {
                use pallet_ai_router::Call::*;
                return matches!(router_call,
                    set_device_approval { .. } | register_node { .. } | heartbeat { .. } |
                    report_hardware { .. } | remove_node { .. } | submit_ai_task { .. } |
                    submit_ai_task_with_reward { .. } | submit_bound_ai_task { .. } |
                    submit_result { .. } | settle_task { .. } | verify_result { .. } |
                    verify_result_with_reward { .. });
            }
            // The issuance experiment must not expose AiRouter's internal
            // reserve/repatriate/unreserve and development-funding paths until
            // those paths participate in authenticated gross accounting.
            // Keep only non-destructive public balance operations available.
            !matches!(
                call,
                RuntimeCall::System(
                    frame_system::Call::set_heap_pages { .. }
                        | frame_system::Call::set_code { .. }
                        | frame_system::Call::set_code_without_checks { .. }
                        | frame_system::Call::set_storage { .. }
                        | frame_system::Call::kill_storage { .. }
                        | frame_system::Call::kill_prefix { .. }
                        | frame_system::Call::authorize_upgrade { .. }
                        | frame_system::Call::authorize_upgrade_without_checks { .. }
                        | frame_system::Call::apply_authorized_upgrade { .. }
                ) | RuntimeCall::AiRouter(_)
                    | RuntimeCall::Balances(
                        pallet_balances::Call::transfer_allow_death { .. }
                            | pallet_balances::Call::force_transfer { .. }
                            | pallet_balances::Call::transfer_all {
                                keep_alive: false,
                                ..
                            }
                            | pallet_balances::Call::upgrade_accounts { .. }
                            | pallet_balances::Call::force_set_balance { .. }
                            | pallet_balances::Call::force_adjust_total_issuance { .. }
                            | pallet_balances::Call::burn { .. }
                    )
            )
        }
    }
}

#[cfg(all(test, feature = "gpu-pow-rewards-dev"))]
mod reward_call_filter_tests {
    use super::*;
    use codec::DecodeAll;

    #[test]
    fn reward_runtime_filters_ai_router_calls() {
        let report = RuntimeCall::AiRouter(pallet_ai_router::Call::report_hardware {
            total_vram: 24,
            free_vram: 12,
            idle: true,
        });
        assert_eq!(RewardRuntimeCallFilter::contains(&report), cfg!(feature = "svy-matrix-live"));
    }

    #[test]
    fn reward_runtime_filters_system_mutation_task_and_upgrade_calls() {
        let denied = [
            RuntimeCall::System(frame_system::Call::set_heap_pages { pages: 0 }),
            RuntimeCall::System(frame_system::Call::set_code { code: Vec::new() }),
            RuntimeCall::System(frame_system::Call::set_code_without_checks { code: Vec::new() }),
            RuntimeCall::System(frame_system::Call::set_storage { items: Vec::new() }),
            RuntimeCall::System(frame_system::Call::kill_storage { keys: Vec::new() }),
            RuntimeCall::System(frame_system::Call::kill_prefix {
                prefix: Vec::new(),
                subkeys: 0,
            }),
            RuntimeCall::System(frame_system::Call::authorize_upgrade {
                code_hash: Default::default(),
            }),
            RuntimeCall::System(frame_system::Call::authorize_upgrade_without_checks {
                code_hash: Default::default(),
            }),
            RuntimeCall::System(frame_system::Call::apply_authorized_upgrade { code: Vec::new() }),
        ];
        let remark = RuntimeCall::System(frame_system::Call::remark {
            remark: b"allowed".to_vec(),
        });

        for call in denied {
            assert!(!RewardRuntimeCallFilter::contains(&call));
        }
        assert!(RewardRuntimeCallFilter::contains(&remark));

        // No composed pallet currently contributes a decodable RuntimeTask.
        // System call index 8 is nevertheless denied so a future task cannot turn
        // this outer System call into an unreviewed nested execution route.
        assert!(RuntimeTask::decode_all(&mut &[0u8][..]).is_err());
    }

    #[test]
    fn reward_runtime_filters_balance_authority_and_destruction_calls() {
        let account = AccountId::new([7; 32]);
        let denied = [
            RuntimeCall::Balances(pallet_balances::Call::force_set_balance {
                who: account.into(),
                new_free: 1,
            }),
            RuntimeCall::Balances(pallet_balances::Call::force_adjust_total_issuance {
                direction: pallet_balances::AdjustmentDirection::Increase,
                delta: 1,
            }),
            RuntimeCall::Balances(pallet_balances::Call::burn {
                value: 1,
                keep_alive: true,
            }),
        ];

        for call in denied {
            assert!(!RewardRuntimeCallFilter::contains(&call));
        }
    }
}

impl pallet_aura::Config for Runtime {
    type AuthorityId = AuraId;
    type DisabledValidators = ();
    type MaxAuthorities = ConstU32<32>;
    type AllowMultipleBlocksPerSlot = ConstBool<false>;
    type SlotDuration = pallet_aura::MinimumPeriodTimesTwo<Runtime>;
}

impl pallet_grandpa::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;

    type WeightInfo = ();
    type MaxAuthorities = ConstU32<32>;
    type MaxNominators = ConstU32<0>;
    type MaxSetIdSessionEntries = ConstU64<0>;

    type KeyOwnerProof = sp_core::Void;
    type EquivocationReportSystem = ();
}

impl pallet_timestamp::Config for Runtime {
    /// A timestamp: milliseconds since the unix epoch.
    type Moment = u64;
    #[cfg(not(feature = "gpu-pow-v2"))]
    type OnTimestampSet = Aura;
    // The isolated PoW runtime does not have an Aura pre-runtime slot digest.
    // Keep Timestamp's once-per-block and minimum-period checks, but do not
    // compare its moment to pallet-aura's CurrentSlot.
    #[cfg(feature = "gpu-pow-v2")]
    type OnTimestampSet = ();
    type MinimumPeriod = ConstU64<{ SLOT_DURATION / 2 }>;
    type WeightInfo = ();
}

impl pallet_balances::Config for Runtime {
    type MaxLocks = ConstU32<50>;
    type MaxReserves = ();
    type ReserveIdentifier = [u8; 8];
    /// The type for recording an account's balance.
    type Balance = Balance;
    /// The ubiquitous event type.
    type RuntimeEvent = RuntimeEvent;
    #[cfg(not(feature = "gpu-pow-rewards-dev"))]
    type DustRemoval = ();
    #[cfg(feature = "gpu-pow-rewards-dev")]
    type DustRemoval = PowRewardFeeHandler;
    type ExistentialDeposit = ConstU128<EXISTENTIAL_DEPOSIT>;
    type AccountStore = System;
    type WeightInfo = pallet_balances::weights::SubstrateWeight<Runtime>;
    type FreezeIdentifier = RuntimeFreezeReason;
    type MaxFreezes = VariantCountOf<RuntimeFreezeReason>;
    type RuntimeHoldReason = RuntimeHoldReason;
    type RuntimeFreezeReason = RuntimeFreezeReason;
    type DoneSlashHandler = ();
}

parameter_types! {
    pub FeeMultiplier: Multiplier = Multiplier::one();
}

impl pallet_transaction_payment::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    #[cfg(not(feature = "gpu-pow-rewards-dev"))]
    type OnChargeTransaction = FungibleAdapter<Balances, ()>;
    #[cfg(feature = "gpu-pow-rewards-dev")]
    type OnChargeTransaction = FungibleAdapter<Balances, PowRewardFeeHandler>;
    type OperationalFeeMultiplier = ConstU8<5>;
    type WeightToFee = IdentityFee<Balance>;
    type LengthToFee = IdentityFee<Balance>;
    type FeeMultiplierUpdate = ConstFeeMultiplier<FeeMultiplier>;
    #[cfg(not(feature = "gpu-pow-rewards-dev"))]
    type WeightInfo = pallet_transaction_payment::weights::SubstrateWeight<Runtime>;
    #[cfg(feature = "gpu-pow-rewards-dev")]
    type WeightInfo = RewardTransactionPaymentWeights;
}

#[cfg(feature = "gpu-pow-rewards-dev")]
pub struct PowRewardFeeHandler;

#[cfg(feature = "gpu-pow-rewards-dev")]
impl OnUnbalanced<pallet_balances::CreditOf<Runtime, ()>> for PowRewardFeeHandler {
    fn on_nonzero_unbalanced(amount: pallet_balances::CreditOf<Runtime, ()>) {
        pallet_pow_rewards::Pallet::<Runtime>::record_destroyed(amount.peek());
        drop(amount);
    }
}

#[cfg(feature = "gpu-pow-rewards-dev")]
pub struct RewardTransactionPaymentWeights;

#[cfg(feature = "gpu-pow-rewards-dev")]
impl pallet_transaction_payment::WeightInfo for RewardTransactionPaymentWeights {
    fn charge_transaction_payment() -> Weight {
        // Benchmark CLI 51.0.0, compiled Wasm, 50 steps requested and 20
        // repeats executed for this zero-component benchmark, on the
        // exact reward runtime measured 80_385_000 ps (minimum 77_491_000)
        // for the real tip/refund path through `PowRewardFeeHandler`.
        // The CLI reported no attributed DB accesses. The explicit allowances
        // below are provisional development reservations, not a proven bound.
        Weight::from_parts(80_385_000, 8_192)
            .saturating_add(<Runtime as frame_system::Config>::DbWeight::get().reads(2))
            .saturating_add(<Runtime as frame_system::Config>::DbWeight::get().writes(2))
    }
}

#[cfg(not(feature = "gpu-pow-rewards-dev"))]
impl pallet_sudo::Config for Runtime {
    type RuntimeEvent = RuntimeEvent;
    type RuntimeCall = RuntimeCall;
    type WeightInfo = pallet_sudo::weights::SubstrateWeight<Runtime>;
}

#[cfg(feature = "gpu-pow-rewards-dev")]
pub struct PowRewardSource;

#[cfg(feature = "gpu-pow-rewards-dev")]
impl pallet_pow_rewards::RewardSource<Runtime> for PowRewardSource {
    fn context() -> Result<pallet_pow_rewards::RewardContext<AccountId>, sp_runtime::DispatchError>
    {
        let claim = super::pow_beneficiary_from_digest(&System::digest())
            .map_err(sp_runtime::DispatchError::Other)?;
        let timestamp_seconds = super::Timestamp::get()
            .checked_div(1_000)
            .ok_or(sp_runtime::DispatchError::Other("invalid timestamp unit"))?;
        Ok(pallet_pow_rewards::RewardContext {
            beneficiary: claim.beneficiary,
            timestamp_seconds,
        })
    }
}

#[cfg(feature = "gpu-pow-rewards-dev")]
pub struct GregorianFirst24Months;

#[cfg(feature = "gpu-pow-rewards-dev")]
impl pallet_pow_rewards::EmissionSchedule for GregorianFirst24Months {
    fn quote(
        _height: u64,
        timestamp_seconds: u64,
        last_timestamp_seconds: u64,
        scheduled_issued: u128,
        genesis_issued: u128,
    ) -> Result<u128, sp_runtime::DispatchError> {
        use pallet_pow_rewards::gregorian::{RateEmission, UtcDate};
        // The approved development schedule ends at the first instant of
        // 2028-11-01 UTC. RateEmission naturally quotes zero after its final
        // month, but accepting such blocks would silently turn an undefined
        // tail policy into a permanent zero-emission policy. Refuse it here so
        // every block remains fail-closed until the tail is explicitly set.
        const END_EXCLUSIVE_SECONDS: u64 = 1_856_649_600;
        if timestamp_seconds >= END_EXCLUSIVE_SECONDS
            || last_timestamp_seconds >= END_EXCLUSIVE_SECONDS
        {
            return Err(sp_runtime::DispatchError::Other(
                "reward schedule ends after approved month 24",
            ));
        }
        let schedule = RateEmission::new(
            UtcDate {
                year: 2026,
                month: 11,
                day: 1,
            },
            genesis_issued,
        )
        .map_err(|_| sp_runtime::DispatchError::Other("invalid reward schedule"))?;
        schedule
            .quote_delta(timestamp_seconds, last_timestamp_seconds, scheduled_issued)
            .map_err(|_| sp_runtime::DispatchError::Other("reward schedule refused block"))
    }
}

#[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
pub struct BlockHeightSchedule;

#[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
impl pallet_pow_rewards::EmissionSchedule for BlockHeightSchedule {
    fn quote(
        height: u64,
        _timestamp_seconds: u64,
        _last_timestamp_seconds: u64,
        scheduled_issued: u128,
        genesis_issued: u128,
    ) -> Result<u128, sp_runtime::DispatchError> {
        pallet_pow_rewards::block_height_schedule::quote(height, genesis_issued, scheduled_issued)
            .map_err(|_| sp_runtime::DispatchError::Other("block reward schedule refused block"))
    }
}

#[cfg(all(feature = "gpu-pow-rewards-dev", feature = "runtime-benchmarks"))]
pub struct PowRewardBenchmarkHelper;

#[cfg(all(feature = "gpu-pow-rewards-dev", feature = "runtime-benchmarks"))]
impl pallet_pow_rewards::BenchmarkHelper<Runtime> for PowRewardBenchmarkHelper {
    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    fn setup_high_height(beneficiary: AccountId, height: u32) -> sp_runtime::DispatchResult {
        use codec::Encode;
        use sp_runtime::{generic::Digest, DigestItem};
        if height == 0 {
            return Err(sp_runtime::DispatchError::Other(
                "benchmark height must be nonzero",
            ));
        }
        let digest = Digest {
            logs: alloc::vec![DigestItem::PreRuntime(
                super::POW_ENGINE_ID,
                super::PowBeneficiaryV1::new(beneficiary).encode(),
            )],
        };
        pallet_pow_rewards::Ledger::<Runtime>::try_mutate(|value| -> sp_runtime::DispatchResult {
            let ledger = value
                .as_mut()
                .ok_or(sp_runtime::DispatchError::Other("benchmark ledger missing"))?;
            if ledger.genesis_issued != pallet_pow_rewards::block_height_schedule::TEAM_GENESIS
                || ledger.scheduled_issued != 0
                || ledger.lifetime_issued != ledger.genesis_issued
            {
                return Err(sp_runtime::DispatchError::Other(
                    "benchmark requires funded clean test genesis",
                ));
            }
            ledger.last_height = u64::from(height - 1);
            Ok(())
        })?;
        System::set_block_number(height - 1);
        System::initialize(&height, &Hash::zero(), &digest);
        crate::Timestamp::set_timestamp(1_856_649_610_000);
        Ok(())
    }

    fn setup(beneficiary: AccountId) -> sp_runtime::DispatchResult {
        use codec::Encode;
        use sp_runtime::{generic::Digest, DigestItem};

        let digest = Digest {
            logs: alloc::vec![DigestItem::PreRuntime(
                super::POW_ENGINE_ID,
                super::PowBeneficiaryV1::new(beneficiary).encode(),
            )],
        };
        System::initialize(&1, &Hash::zero(), &digest);
        // Last second covered by the approved first-24-month schedule. This
        // exercises every Gregorian month while remaining strictly in policy.
        crate::Timestamp::set_timestamp(1_856_649_599_000);
        Ok(())
    }

    fn setup_existing_accounts(beneficiary: AccountId) -> sp_runtime::DispatchResult {
        use codec::Encode;
        use sp_runtime::{generic::Digest, DigestItem};

        let digest = |account: AccountId| Digest {
            logs: alloc::vec![DigestItem::PreRuntime(
                super::POW_ENGINE_ID,
                super::PowBeneficiaryV1::new(account).encode(),
            )],
        };
        // First create both accounts through the real issuance path so total
        // issuance and the reward ledger remain mutually consistent.
        System::initialize(&1, &Hash::zero(), &digest(beneficiary.clone()));
        crate::Timestamp::set_timestamp(1_793_491_210_000);
        crate::PowRewards::issue_current_block()?;

        // Advance to the next valid height and timestamp. Only this second
        // issuance is inside the benchmark's measured block.
        System::initialize(&2, &Hash::zero(), &digest(beneficiary));
        crate::Timestamp::set_timestamp(1_793_491_220_000);
        Ok(())
    }

    fn setup_legacy_beneficiary(beneficiary: AccountId) -> sp_runtime::DispatchResult {
        use codec::Encode;
        use sp_runtime::{generic::Digest, DigestItem};

        let digest = |account: AccountId| Digest {
            logs: alloc::vec![DigestItem::PreRuntime(
                super::POW_ENGINE_ID,
                super::PowBeneficiaryV1::new(account).encode(),
            )],
        };
        // Create the beneficiary and AI pool through real issuance first. This
        // keeps total issuance and the reward ledger exact; benchmark setup
        // must not mint an untracked balance merely to reach the old-account
        // branch.
        System::initialize(&1, &Hash::zero(), &digest(beneficiary.clone()));
        crate::Timestamp::set_timestamp(1_793_491_210_000);
        crate::PowRewards::issue_current_block()?;

        frame_system::Account::<Runtime>::mutate(&beneficiary, |info| {
            info.data.flags = pallet_balances::ExtraFlags::old_logic();
        });
        debug_assert!(!frame_system::Account::<Runtime>::get(&beneficiary)
            .data
            .flags
            .is_new_logic());

        // Only the second issuance is measured. `deposit_creating` must first
        // run Balances::ensure_upgraded for this beneficiary.
        System::initialize(&2, &Hash::zero(), &digest(beneficiary));
        crate::Timestamp::set_timestamp(1_793_491_220_000);
        Ok(())
    }
}

#[cfg(feature = "gpu-pow-rewards-dev")]
parameter_types! {
    pub const PowRewardLifetimeCap: u128 = if cfg!(feature = "svy-matrix-live") { pallet_pow_rewards::block_height_schedule::CAP } else { 1_000_000_000u128 * super::UNIT };
}

/// Temporary reservation for the block-height development runtime only.
/// Diagnostic compiled-Wasm runs on the local debug benchmark host measured
/// a maximum 2_548_696 ns over 20 samples across the tested issuance paths.
/// Reserve 6 ms (> 2x that observed maximum); keep the earlier larger DB and
/// proof allowances. This is not a release-host or proven worst-case bound.
#[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
pub struct BlockRewardDevelopmentWeights;

#[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
impl pallet_pow_rewards::WeightInfo for BlockRewardDevelopmentWeights {
    fn issue_current_block() -> Weight {
        Weight::from_parts(6_000_000_000, 65_536)
            .saturating_add(<Runtime as frame_system::Config>::DbWeight::get().reads(10))
            .saturating_add(<Runtime as frame_system::Config>::DbWeight::get().writes(7))
    }
}

#[cfg(feature = "gpu-pow-rewards-dev")]
impl pallet_pow_rewards::Config for Runtime {
    type Currency = Balances;
    type AiPool = AiRewardAccount;
    type LifetimeCap = PowRewardLifetimeCap;
    type Source = PowRewardSource;
    #[cfg(not(feature = "gpu-pow-v3-block-rewards-dev"))]
    type Schedule = GregorianFirst24Months;
    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    type Schedule = BlockHeightSchedule;
    #[cfg(not(feature = "gpu-pow-v3-block-rewards-dev"))]
    type WeightInfo = pallet_pow_rewards::weights::WeightInfo<Runtime>;
    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    type WeightInfo = BlockRewardDevelopmentWeights;
    #[cfg(feature = "runtime-benchmarks")]
    type BenchmarkHelper = PowRewardBenchmarkHelper;
    #[cfg(not(feature = "runtime-benchmarks"))]
    type BenchmarkHelper = ();
}
