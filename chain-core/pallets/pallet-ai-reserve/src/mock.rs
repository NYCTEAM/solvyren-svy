//! Mock runtime for S4 tests only. Not a Runtime composition.
use crate as pallet_ai_reserve;
use frame_support::{derive_impl, parameter_types, traits::ConstU128};
use frame_system::EnsureSignedBy;
use sp_runtime::BuildStorage;

type Block = frame_system::mocking::MockBlock<Test>;

frame_support::construct_runtime!(
    pub enum Test {
        System: frame_system,
        Balances: pallet_balances,
        AiReserve: pallet_ai_reserve,
    }
);

#[derive_impl(frame_system::config_preludes::TestDefaultConfig)]
impl frame_system::Config for Test {
    type Block = Block;
    type AccountId = u64;
    type AccountData = pallet_balances::AccountData<u128>;
}

pub const ED: u128 = 10;

#[derive_impl(pallet_balances::config_preludes::TestDefaultConfig)]
impl pallet_balances::Config for Test {
    type AccountStore = System;
    type Balance = u128;
    type ExistentialDeposit = ConstU128<ED>;
}

pub const RESERVE: u64 = 999;
pub const SCHED: u64 = 1;
pub const ATT: u64 = 2;
pub const WORKER: u64 = 3;
pub const BENEF: u64 = 4;
pub const ANYONE: u64 = 5;
pub const WORKER2: u64 = 6;
pub const RESERVE_START: u128 = 10_000;
/// Beneficiary must already exist: pallet-balances `repatriate_reserved` to a dead account
/// fails with `DeadAccount` (see test `payment_to_nonexistent_beneficiary_fails_atomically`).
pub const BENEF_START: u128 = 100;
/// TEST VALUE ONLY. Not a mainnet parameter.
pub const SETTLE_WINDOW: u64 = 20;
pub const MAX_LEASE: u64 = 50;

parameter_types! {
    pub const RewardAccount: u64 = RESERVE;
    pub const MaxLeaseBlocks: u64 = MAX_LEASE;
    pub const SettleWindow: u64 = SETTLE_WINDOW;
}

impl pallet_ai_reserve::Config for Test {
    type Currency = Balances;
    type SchedulerOrigin = EnsureSignedBy<pallet_ai_reserve::SchedulerMembers<Test>, u64>;
    type AttestorOrigin = EnsureSignedBy<pallet_ai_reserve::AttestorMembers<Test>, u64>;
    type RewardAccount = RewardAccount;
    type MaxLeaseBlocks = MaxLeaseBlocks;
    type SettleWindow = SettleWindow;
}

/// V1 evidence rate: in 1_000_000, cached 0, out 2_000_000, caps 1024/192.
pub fn v1_rate() -> pallet_ai_reserve::TokenRate {
    pallet_ai_reserve::TokenRate {
        model_digest: [0xb2; 32],
        tokenizer_digest: [0xb2; 32],
        runtime_digest: [0x70; 32],
        input_per_million: 1_000_000,
        cached_per_million: 0,
        output_per_million: 2_000_000,
        max_input_tokens: 1024,
        max_output_tokens: 192,
    }
}

pub struct ExtBuilder {
    pub reserve_balance: u128,
    pub schedulers: Vec<u64>,
    pub attestors: Vec<u64>,
    pub rates: Vec<pallet_ai_reserve::RateEntry>,
}

impl Default for ExtBuilder {
    fn default() -> Self {
        Self {
            reserve_balance: RESERVE_START,
            schedulers: vec![SCHED],
            attestors: vec![ATT],
            rates: vec![pallet_ai_reserve::RateEntry { rate: v1_rate(), version: 1 }],
        }
    }
}

impl ExtBuilder {
    pub fn storage(self) -> sp_runtime::Storage {
        RuntimeGenesisConfig {
            system: Default::default(),
            balances: pallet_balances::GenesisConfig::<Test> {
                balances: vec![(RESERVE, self.reserve_balance), (BENEF, BENEF_START)],
                ..Default::default()
            },
            ai_reserve: pallet_ai_reserve::GenesisConfig::<Test> {
                schedulers: self.schedulers,
                attestors: self.attestors,
                rates: self.rates,
            },
        }
        .build_storage()
        .unwrap()
    }

    pub fn build(self) -> sp_io::TestExternalities {
        let mut ext: sp_io::TestExternalities = self.storage().into();
        ext.execute_with(|| System::set_block_number(100));
        ext
    }
}
