#![cfg_attr(not(feature = "std"), no_std)]
pub use pallet::*;

#[cfg(test)]
mod tests;

#[frame_support::pallet]
pub mod pallet {
    use frame_support::sp_runtime::traits::Zero;
    use frame_support::{
        pallet_prelude::*,
        traits::{BalanceStatus, Currency, EnsureOrigin, ExistenceRequirement, ReservableCurrency},
        transactional,
    };
    use frame_system::pallet_prelude::*;
    pub type BalanceOf<T> =
        <<T as Config>::Currency as Currency<<T as frame_system::Config>::AccountId>>::Balance;

    #[derive(
        Clone,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
    )]
    pub struct GpuNode<BlockNumber> {
        pub total_vram: u64,
        pub free_vram: u64,
        pub last_seen: BlockNumber,
        pub busy: bool,
        pub idle: bool,
    }

    #[derive(
        Clone,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
    )]
    pub struct Assignment<AccountId, BlockNumber, Balance> {
        pub miner: AccountId,
        pub task_type: u8,
        pub required_vram: u64,
        pub assigned_at: BlockNumber,
        pub reward: Balance,
    }

    /// Devnet funding transfers existing tokens; it does not mint block rewards.
    #[derive(
        Clone,
        Encode,
        Decode,
        DecodeWithMemTracking,
        Eq,
        PartialEq,
        RuntimeDebug,
        TypeInfo,
        MaxEncodedLen,
    )]
    pub struct FundingPlan<AccountId, Balance> {
        pub source: AccountId,
        pub block_budget: Balance,
    }

    pub trait TaskFunding<T: Config> {
        fn reserve(maximum: BalanceOf<T>) -> DispatchResult;
        fn finish(miner: &T::AccountId, maximum: BalanceOf<T>, actual: BalanceOf<T>) -> DispatchResult;
    }

    pub struct Prefunded;
    impl<T: Config> TaskFunding<T> for Prefunded {
        fn reserve(maximum: BalanceOf<T>) -> DispatchResult {
            T::Currency::reserve(&T::RewardAccount::get(), maximum)
        }
        fn finish(miner: &T::AccountId, maximum: BalanceOf<T>, actual: BalanceOf<T>) -> DispatchResult {
            let pool = T::RewardAccount::get();
            if !actual.is_zero() {
                let unpaid = T::Currency::repatriate_reserved(&pool, miner, actual, BalanceStatus::Free)?;
                ensure!(unpaid.is_zero(), Error::<T>::IncompletePayment);
            }
            ensure!(T::Currency::unreserve(&pool, maximum - actual).is_zero(), Error::<T>::IncompletePayment);
            Ok(())
        }
    }

    #[pallet::config]
    pub trait Config: frame_system::Config<RuntimeEvent: From<Event<Self>>> {
        /// Governance or authenticated application gateway; not arbitrary users.
        type Funding: TaskFunding<Self>;
        type TaskOrigin: EnsureOrigin<Self::RuntimeOrigin>;
        type Currency: ReservableCurrency<Self::AccountId>;
        /// Whether the legacy prefunded development transfer is permitted.
        type DevFundingEnabled: Get<bool>;
        type RequireDeviceApproval: Get<bool>;
        #[pallet::constant]
        type RewardAccount: Get<Self::AccountId>;
        #[pallet::constant]
        type TaskReward: Get<BalanceOf<Self>>;
        #[pallet::constant]
        type MaxNodes: Get<u32>;
        #[pallet::constant]
        type HeartbeatTtl: Get<BlockNumberFor<Self>>;
        type WeightInfo: WeightInfo;
    }

    /// Supply benchmark-generated weights in the integrating runtime.
    pub trait WeightInfo {
        fn report() -> Weight;
        fn route(max_nodes: u32) -> Weight;
        fn settle() -> Weight;
    }

    #[pallet::pallet]
    pub struct Pallet<T>(_);

    /// One aggregate worker per signed account. Bounded admission caps scan cost.
    #[pallet::storage]
    pub type GpuNodeRegistry<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, GpuNode<BlockNumberFor<T>>, OptionQuery>;
    #[pallet::storage]
    pub type NodeCount<T> = StorageValue<_, u32, ValueQuery>;
    #[pallet::storage]
    pub type NextTaskId<T> = StorageValue<_, u64, ValueQuery>;
    #[pallet::storage]
    pub type ActiveTasks<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        u64,
        Assignment<T::AccountId, BlockNumberFor<T>, BalanceOf<T>>,
        OptionQuery,
    >;
    #[pallet::storage]
    pub type NodeDevices<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, BoundedVec<u8, ConstU32<96>>, OptionQuery>;
    #[pallet::storage]
    pub type PendingResults<T> = StorageMap<_, Blake2_128Concat, u64, [u8; 32], OptionQuery>;
    #[pallet::storage]
    pub type CompletedResults<T> = StorageMap<_, Blake2_128Concat, u64, [u8; 32], OptionQuery>;

    /// Scoped verifier and scheduler. Only its public signing account is stored.
    #[pallet::storage]
    pub type AiOperator<T: Config> = StorageValue<_, T::AccountId, OptionQuery>;

    #[pallet::genesis_config]
    #[derive(frame_support::DefaultNoBound)]
    pub struct GenesisConfig<T: Config> {
        pub operator: Option<T::AccountId>,
    }

    #[pallet::genesis_build]
    impl<T: Config> BuildGenesisConfig for GenesisConfig<T> {
        fn build(&self) {
            if let Some(operator) = &self.operator {
                assert!(T::RequireDeviceApproval::get());
                AiOperator::<T>::put(operator);
            }
        }
    }

    /// Verified inventory identifier bound to a dedicated device signing account.
    #[pallet::storage]
    pub type ApprovedDevices<T: Config> =
        StorageMap<_, Blake2_128Concat, T::AccountId, BoundedVec<u8, ConstU32<96>>, OptionQuery>;

    /// Permanent replay protection, retained after payment and cancellation.
    #[pallet::storage]
    pub type RequestTasks<T> = StorageMap<_, Blake2_128Concat, [u8; 32], u64, OptionQuery>;
    #[pallet::storage]
    pub type TaskRequests<T> = StorageMap<_, Blake2_128Concat, u64, [u8; 32], OptionQuery>;
    /// Immutable original terms for externally bound tasks; Assignment encoding is unchanged.
    #[pallet::storage]
    pub type BoundTaskTerms<T: Config> = StorageMap<
        _,
        Blake2_128Concat,
        u64,
        Assignment<T::AccountId, BlockNumberFor<T>, BalanceOf<T>>,
        OptionQuery,
    >;
    /// Persistent actual payment, including zero-value settlements.
    #[pallet::storage]
    pub type PaidRewards<T: Config> =
        StorageMap<_, Blake2_128Concat, u64, BalanceOf<T>, OptionQuery>;

    /// Disabled by default. This is a prefunded devnet simulation, not mining issuance.
    #[pallet::storage]
    pub type DevFundingPlan<T: Config> =
        StorageValue<_, FundingPlan<T::AccountId, BalanceOf<T>>, OptionQuery>;

    #[pallet::hooks]
    impl<T: Config> Hooks<BlockNumberFor<T>> for Pallet<T> {
        fn on_initialize(_n: BlockNumberFor<T>) -> Weight {
            if !T::DevFundingEnabled::get() {
                assert!(
                    DevFundingPlan::<T>::get().is_none(),
                    "development funding disabled with existing plan"
                );
                return T::DbWeight::get().reads(1);
            }
            if let Some(plan) = DevFundingPlan::<T>::get() {
                // Funds already exist in source. A failed transfer never mints or overdrafts.
                let amount =
                    frame_support::sp_runtime::Perbill::from_percent(30) * plan.block_budget;
                if !amount.is_zero()
                    && T::Currency::transfer(
                        &plan.source,
                        &T::RewardAccount::get(),
                        amount,
                        ExistenceRequirement::KeepAlive,
                    )
                    .is_ok()
                {
                    Self::deposit_event(Event::DevPoolFunded { amount });
                }
                T::DbWeight::get().reads_writes(3, 3)
            } else {
                T::DbWeight::get().reads(1)
            }
        }
    }

    #[pallet::event]
    #[pallet::generate_deposit(pub(super) fn deposit_event)]
    pub enum Event<T: Config> {
        Heartbeat {
            miner: T::AccountId,
            free_vram: u64,
            idle: bool,
        },
        TaskAssigned {
            task_id: u64,
            miner: T::AccountId,
            task_type: u8,
            required_vram: u64,
        },
        TaskSettled {
            task_id: u64,
        },
        NodeRemoved {
            miner: T::AccountId,
        },
        NodeRegistered {
            miner: T::AccountId,
        },
        ResultSubmitted {
            task_id: u64,
            result_hash: [u8; 32],
        },
        TaskPaid {
            task_id: u64,
            miner: T::AccountId,
            reward: BalanceOf<T>,
        },
        DevPoolFunded {
            amount: BalanceOf<T>,
        },
        DevFundingConfigured {
            enabled: bool,
        },
        DeviceApprovalChanged { miner: T::AccountId, approved: bool },
        TaskRequestBound {
            task_id: u64,
            request_hash: [u8; 32],
        },
    }

    #[pallet::error]
    pub enum Error<T> {
        InvalidMemory,
        RegistryFull,
        InvalidTaskType,
        NoEligibleMiner,
        TaskIdOverflow,
        UnknownTask,
        NodeBusy,
        UnknownNode,
        NotAssignedMiner,
        ResultAlreadySubmitted,
        NoResult,
        IncompletePayment,
        InvalidReward,
        RequestAlreadyBound,
        InvalidRequestHash,
        DeviceNotApproved,
    }

    #[pallet::call]
    impl<T: Config> Pallet<T> {
        #[pallet::call_index(0)]
        #[pallet::weight(T::WeightInfo::report())]
        pub fn report_hardware(
            origin: OriginFor<T>,
            total_vram: u64,
            free_vram: u64,
            idle: bool,
        ) -> DispatchResult {
            let miner = ensure_signed(origin)?;
            ensure!(
                total_vram > 0 && free_vram <= total_vram,
                Error::<T>::InvalidMemory
            );
            ensure!(Self::can_receive_tasks(&miner), Error::<T>::DeviceNotApproved);
            let old = GpuNodeRegistry::<T>::get(&miner);
            if old.is_none() {
                ensure!(
                    NodeCount::<T>::get() < T::MaxNodes::get(),
                    Error::<T>::RegistryFull
                );
                NodeCount::<T>::mutate(|n| *n += 1);
            }
            // A heartbeat must never clear an outstanding reservation.
            let busy = old.map(|n| n.busy).unwrap_or(false);
            GpuNodeRegistry::<T>::insert(
                &miner,
                GpuNode {
                    total_vram,
                    free_vram,
                    last_seen: frame_system::Pallet::<T>::block_number(),
                    busy,
                    idle,
                },
            );
            Self::deposit_event(Event::Heartbeat {
                miner,
                free_vram,
                idle,
            });
            Ok(())
        }

        #[pallet::call_index(1)]
        #[pallet::weight(T::WeightInfo::route(T::MaxNodes::get()))]
        pub fn submit_ai_task(
            origin: OriginFor<T>,
            task_type: u8,
            required_vram: u64,
        ) -> DispatchResult {
            Self::ensure_operator(origin)?;
            Self::route_ai_task(task_type, required_vram)?;
            Ok(())
        }

        /// Trusted scheduler cancels an uncommitted assignment.
        /// Committed work requires authorized verification or explicit dispute resolution;
        /// timeout alone must not erase the miner's result and refund its reservation.
        /// Miners cannot release themselves and receive overlapping assignments.
        #[pallet::call_index(2)]
        #[pallet::weight(T::WeightInfo::settle().saturating_add(T::DbWeight::get().reads(1)))]
        #[transactional]
        pub fn settle_task(origin: OriginFor<T>, task_id: u64) -> DispatchResult {
            Self::ensure_operator(origin)?;
            let assignment = ActiveTasks::<T>::take(task_id).ok_or(Error::<T>::UnknownTask)?;
            ensure!(!PendingResults::<T>::contains_key(task_id) || !Self::can_receive_tasks(&assignment.miner),
                Error::<T>::ResultAlreadySubmitted);
            T::Funding::finish(&assignment.miner, assignment.reward, Zero::zero())?;
            PendingResults::<T>::remove(task_id);
            GpuNodeRegistry::<T>::mutate(&assignment.miner, |node| {
                if let Some(node) = node {
                    node.busy = false;
                    // Require a fresh heartbeat before reuse; memory may still be occupied.
                    node.free_vram = 0;
                }
            });
            Self::deposit_event(Event::TaskSettled { task_id });
            Ok(())
        }

        #[pallet::call_index(3)]
        #[pallet::weight(T::WeightInfo::settle())]
        pub fn remove_node(origin: OriginFor<T>, miner: T::AccountId) -> DispatchResult {
            Self::ensure_operator(origin)?;
            let node = GpuNodeRegistry::<T>::get(&miner).ok_or(Error::<T>::UnknownNode)?;
            ensure!(!node.busy, Error::<T>::NodeBusy);
            GpuNodeRegistry::<T>::remove(&miner);
            NodeDevices::<T>::remove(&miner);
            NodeCount::<T>::mutate(|n| *n = n.saturating_sub(1));
            Self::deposit_event(Event::NodeRemoved { miner });
            Ok(())
        }

        #[pallet::call_index(4)]
        #[pallet::weight(T::WeightInfo::report())]
        #[transactional]
        pub fn register_node(
            origin: OriginFor<T>,
            total_vram: u64,
            device: BoundedVec<u8, ConstU32<96>>,
        ) -> DispatchResult {
            let miner = ensure_signed(origin.clone())?;
            if T::RequireDeviceApproval::get() {
                ensure!(ApprovedDevices::<T>::get(&miner).as_ref() == Some(&device), Error::<T>::DeviceNotApproved);
            }
            NodeDevices::<T>::insert(&miner, device);
            Self::report_hardware(origin, total_vram, 0, true)?;
            Self::deposit_event(Event::NodeRegistered { miner });
            Ok(())
        }

        #[pallet::call_index(5)]
        #[pallet::weight(T::WeightInfo::report())]
        pub fn heartbeat(origin: OriginFor<T>, free_vram: u64, idle: bool) -> DispatchResult {
            let miner = ensure_signed(origin.clone())?;
            let total = GpuNodeRegistry::<T>::get(&miner)
                .ok_or(Error::<T>::UnknownNode)?
                .total_vram;
            Self::report_hardware(origin, total, free_vram, idle)
        }

        /// Hash commitment only. This is NOT a ZK proof or on-chain verification.
        #[pallet::call_index(6)]
        #[pallet::weight(T::WeightInfo::settle())]
        pub fn submit_result(
            origin: OriginFor<T>,
            task_id: u64,
            result_hash: [u8; 32],
        ) -> DispatchResult {
            let miner = ensure_signed(origin)?;
            let task = ActiveTasks::<T>::get(task_id).ok_or(Error::<T>::UnknownTask)?;
            ensure!(task.miner == miner, Error::<T>::NotAssignedMiner);
            ensure!(Self::can_receive_tasks(&miner), Error::<T>::DeviceNotApproved);
            ensure!(
                !PendingResults::<T>::contains_key(task_id),
                Error::<T>::ResultAlreadySubmitted
            );
            PendingResults::<T>::insert(task_id, result_hash);
            Self::deposit_event(Event::ResultSubmitted {
                task_id,
                result_hash,
            });
            Ok(())
        }

        /// Development trusted-verifier attestation; verifier must independently
        /// recompute the workload before submitting this call.
        #[pallet::call_index(7)]
        #[pallet::weight(T::WeightInfo::settle())]
        #[transactional]
        pub fn verify_result(
            origin: OriginFor<T>,
            task_id: u64,
            expected_hash: [u8; 32],
        ) -> DispatchResult {
            Self::ensure_operator(origin)?;
            ensure!(
                PendingResults::<T>::contains_key(task_id),
                Error::<T>::NoResult
            );
            let task = ActiveTasks::<T>::get(task_id).ok_or(Error::<T>::UnknownTask)?;
            Self::pay_verified_result(task_id, expected_hash, task.reward)
        }

        /// Trusted scheduler sets the already-agreed maximum for this assignment.
        #[pallet::call_index(8)]
        #[pallet::weight(T::WeightInfo::route(T::MaxNodes::get()))]
        pub fn submit_ai_task_with_reward(
            origin: OriginFor<T>,
            task_type: u8,
            required_vram: u64,
            reward: BalanceOf<T>,
        ) -> DispatchResult {
            Self::ensure_operator(origin)?;
            Self::route_ai_task_with_reward(task_type, required_vram, reward)?;
            Ok(())
        }

        /// Explicitly enable/disable the prefunded devnet per-block transfer.
        #[pallet::call_index(9)]
        #[pallet::weight(T::WeightInfo::settle())]
        pub fn configure_dev_funding(
            origin: OriginFor<T>,
            plan: Option<FundingPlan<T::AccountId, BalanceOf<T>>>,
        ) -> DispatchResult {
            T::TaskOrigin::ensure_origin(origin)?;
            ensure!(
                T::DevFundingEnabled::get() || plan.is_none(),
                Error::<T>::InvalidReward
            );
            if let Some(ref p) = plan {
                ensure!(
                    p.source != T::RewardAccount::get() && !p.block_budget.is_zero(),
                    Error::<T>::InvalidReward
                );
            }
            let enabled = plan.is_some();
            DevFundingPlan::<T>::set(plan);
            Self::deposit_event(Event::DevFundingConfigured { enabled });
            Ok(())
        }

        /// Settle actual usage at or below the pre-reserved maximum; release the rest.
        #[pallet::call_index(10)]
        #[pallet::weight(T::WeightInfo::settle())]
        #[transactional]
        pub fn verify_result_with_reward(
            origin: OriginFor<T>,
            task_id: u64,
            expected_hash: [u8; 32],
            actual_reward: BalanceOf<T>,
        ) -> DispatchResult {
            Self::ensure_operator(origin)?;
            Self::pay_verified_result(task_id, expected_hash, actual_reward)
        }

        /// Trusted Studio scheduler binds a request digest and its exact chosen worker.
        /// 16=text, 17=image, 18=video. Legacy matrix workers must not execute these tasks.
        /// The digest must commit to the canonical request, model/version and pricing terms.
        #[pallet::call_index(11)]
        #[pallet::weight(T::WeightInfo::route(T::MaxNodes::get()))]
        #[transactional]
        pub fn submit_bound_ai_task(
            origin: OriginFor<T>,
            request_hash: [u8; 32],
            miner: T::AccountId,
            task_type: u8,
            required_vram: u64,
            reward: BalanceOf<T>,
        ) -> DispatchResult {
            Self::ensure_operator(origin)?;
            ensure!((16..=18).contains(&task_type), Error::<T>::InvalidTaskType);
            ensure!(request_hash != [0; 32], Error::<T>::InvalidRequestHash);
            ensure!(
                !RequestTasks::<T>::contains_key(request_hash),
                Error::<T>::RequestAlreadyBound
            );
            ensure!(required_vram > 0, Error::<T>::InvalidMemory);
            ensure!(!reward.is_zero(), Error::<T>::InvalidReward);
            let task_id = NextTaskId::<T>::get();
            let next_id = task_id.checked_add(1).ok_or(Error::<T>::TaskIdOverflow)?;
            let now = frame_system::Pallet::<T>::block_number();
            ensure!(Self::can_receive_tasks(&miner), Error::<T>::DeviceNotApproved);
            let mut node = GpuNodeRegistry::<T>::get(&miner).ok_or(Error::<T>::UnknownNode)?;
            ensure!(
                // idle is a scheduling hint: Studio may already hold a local lease.
                // The authoritative chain reservation is busy, which heartbeats cannot clear.
                !node.busy
                    && node.free_vram >= required_vram
                    && node.last_seen <= now
                    && now - node.last_seen <= T::HeartbeatTtl::get(),
                Error::<T>::NoEligibleMiner
            );
            T::Funding::reserve(reward)?;
            node.busy = true;
            node.free_vram -= required_vram;
            GpuNodeRegistry::<T>::insert(&miner, node);
            let assignment = Assignment {
                miner: miner.clone(),
                task_type,
                required_vram,
                assigned_at: now,
                reward,
            };
            ActiveTasks::<T>::insert(task_id, &assignment);
            BoundTaskTerms::<T>::insert(task_id, &assignment);
            RequestTasks::<T>::insert(request_hash, task_id);
            TaskRequests::<T>::insert(task_id, request_hash);
            NextTaskId::<T>::put(next_id);
            Self::deposit_event(Event::TaskAssigned {
                task_id,
                miner,
                task_type,
                required_vram,
            });
            Self::deposit_event(Event::TaskRequestBound {
                task_id,
                request_hash,
            });
            Ok(())
        }
        /// The verifier checks the machine and its device signing key off-chain.
        #[pallet::call_index(12)]
        #[pallet::weight(T::WeightInfo::report().saturating_add(T::DbWeight::get().writes(2)))]
        pub fn set_device_approval(
            origin: OriginFor<T>, miner: T::AccountId, device: Option<BoundedVec<u8, ConstU32<96>>>,
        ) -> DispatchResult {
            Self::ensure_operator(origin)?;
            ensure!(T::RequireDeviceApproval::get(), Error::<T>::DeviceNotApproved);
            let approved = device.is_some();
            if let Some(device) = device {
                ensure!(!device.is_empty(), Error::<T>::DeviceNotApproved);
                ApprovedDevices::<T>::insert(&miner, device);
            } else {
                ApprovedDevices::<T>::remove(&miner);
            }
            NodeDevices::<T>::remove(&miner);
            Self::deposit_event(Event::DeviceApprovalChanged { miner, approved });
            Ok(())
        }
    }

    impl<T: Config> Pallet<T> {
        fn ensure_operator(origin: OriginFor<T>) -> DispatchResult {
            if T::RequireDeviceApproval::get() {
                if let Ok(signer) = ensure_signed(origin.clone()) {
                    ensure!(AiOperator::<T>::get().as_ref() == Some(&signer), DispatchError::BadOrigin);
                    return Ok(());
                }
            }
            T::TaskOrigin::ensure_origin(origin)?;
            Ok(())
        }

        fn can_receive_tasks(miner: &T::AccountId) -> bool {
            if !T::RequireDeviceApproval::get() { return true; }
            match ApprovedDevices::<T>::get(miner) {
                Some(device) => NodeDevices::<T>::get(miner).as_ref() == Some(&device),
                None => false,
            }
        }

        fn pay_verified_result(
            task_id: u64,
            expected_hash: [u8; 32],
            actual_reward: BalanceOf<T>,
        ) -> DispatchResult {
            let hash = PendingResults::<T>::get(task_id).ok_or(Error::<T>::NoResult)?;
            ensure!(hash == expected_hash, Error::<T>::NoResult);
            let task = ActiveTasks::<T>::get(task_id).ok_or(Error::<T>::UnknownTask)?;
            ensure!(actual_reward <= task.reward, Error::<T>::InvalidReward);
            ensure!(Self::can_receive_tasks(&task.miner), Error::<T>::DeviceNotApproved);
            T::Funding::finish(&task.miner, task.reward, actual_reward)?;
            ActiveTasks::<T>::remove(task_id);
            PendingResults::<T>::remove(task_id);
            CompletedResults::<T>::insert(task_id, hash);
            PaidRewards::<T>::insert(task_id, actual_reward);
            GpuNodeRegistry::<T>::mutate(&task.miner, |node| {
                if let Some(node) = node {
                    node.busy = false;
                    node.free_vram = 0;
                }
            });
            Self::deposit_event(Event::TaskPaid {
                task_id,
                miner: task.miner,
                reward: actual_reward,
            });
            Ok(())
        }
        /// Runtime-internal API; external callers use the origin-checked extrinsic.
        /// Best-fit selection is deterministic in storage iteration order.
        #[transactional]
        pub fn route_ai_task(
            task_type: u8,
            required_vram: u64,
        ) -> Result<T::AccountId, DispatchError> {
            Self::route_ai_task_with_reward(task_type, required_vram, T::TaskReward::get())
        }

        #[transactional]
        pub fn route_ai_task_with_reward(
            task_type: u8,
            required_vram: u64,
            reward: BalanceOf<T>,
        ) -> Result<T::AccountId, DispatchError> {
            ensure!(task_type <= 2, Error::<T>::InvalidTaskType);
            ensure!(required_vram > 0, Error::<T>::InvalidMemory);
            ensure!(!reward.is_zero(), Error::<T>::InvalidReward);
            let task_id = NextTaskId::<T>::get();
            let next_id = task_id.checked_add(1).ok_or(Error::<T>::TaskIdOverflow)?;
            let now = frame_system::Pallet::<T>::block_number();
            let mut best: Option<(T::AccountId, GpuNode<BlockNumberFor<T>>)> = None;
            for (account, node) in GpuNodeRegistry::<T>::iter() {
                if !Self::can_receive_tasks(&account) || node.busy
                    || !node.idle
                    || node.free_vram < required_vram
                    || node.last_seen > now
                    || now - node.last_seen > T::HeartbeatTtl::get()
                {
                    continue;
                }
                if best
                    .as_ref()
                    .map(|(_, n)| node.free_vram < n.free_vram)
                    .unwrap_or(true)
                {
                    best = Some((account, node));
                }
            }
            let (miner, mut node) = best.ok_or(Error::<T>::NoEligibleMiner)?;
            T::Funding::reserve(reward)?;
            node.busy = true;
            node.free_vram -= required_vram;
            GpuNodeRegistry::<T>::insert(&miner, node);
            ActiveTasks::<T>::insert(
                task_id,
                Assignment {
                    miner: miner.clone(),
                    task_type,
                    required_vram,
                    assigned_at: now,
                    reward,
                },
            );
            NextTaskId::<T>::put(next_id);
            Self::deposit_event(Event::TaskAssigned {
                task_id,
                miner: miner.clone(),
                task_type,
                required_vram,
            });
            Ok(miner)
        }
    }
}
