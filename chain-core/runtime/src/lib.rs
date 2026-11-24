#![cfg_attr(not(feature = "std"), no_std)]

#[cfg(feature = "std")]
include!(concat!(env!("OUT_DIR"), "/wasm_binary.rs"));

pub mod apis;
#[cfg(feature = "svy-transfer-fee-candidate")]
pub mod transfer_payment;
#[cfg(feature = "svy-transfer-fee-candidate")]
pub type PaymentExtension = transfer_payment::TransferPayment;
#[cfg(not(feature = "svy-transfer-fee-candidate"))]
pub type PaymentExtension = pallet_transaction_payment::ChargeTransactionPayment<Runtime>;
#[cfg(feature = "runtime-benchmarks")]
mod benchmarks;
pub mod configs;
#[cfg(any(feature = "incinerator-rehearsal", feature = "native-dead-mainnet-drill"))]
mod incinerator_rehearsal;
#[cfg(any(feature = "incinerator-rehearsal", feature = "native-dead-mainnet-drill"))]
mod native_incinerator_rehearsal;
#[cfg(all(feature = "incinerator-rehearsal", feature = "gpu-pow-rewards-dev"))]
compile_error!("Incinerator rehearsal only supports the separate development runtime");
#[cfg(feature = "incinerator-rehearsal")]
impl incinerator_rehearsal::Config for Runtime {}

extern crate alloc;
use alloc::vec::Vec;
use codec::{Decode, DecodeAll, Encode, MaxEncodedLen};
use scale_info::TypeInfo;
use sp_runtime::{
    generic, impl_opaque_keys,
    traits::{BlakeTwo256, IdentifyAccount, Verify},
    DigestItem, MultiAddress, MultiSignature,
};
#[cfg(feature = "std")]
use sp_version::NativeVersion;
use sp_version::RuntimeVersion;

pub use frame_system::Call as SystemCall;
pub use pallet_balances::Call as BalancesCall;
pub use pallet_timestamp::Call as TimestampCall;

pub fn reward_account() -> AccountId {
    use sp_runtime::traits::AccountIdConversion;
    frame_support::PalletId(*b"eag/ai00").into_account_truncating()
}
#[cfg(any(feature = "std", test))]
pub use sp_runtime::BuildStorage;

pub mod genesis_config_presets;

/// Opaque types. These are used by the CLI to instantiate machinery that don't need to know
/// the specifics of the runtime. They can then be made to be agnostic over specific formats
/// of data like extrinsics, allowing for them to continue syncing the network through upgrades
/// to even the core data structures.
pub mod opaque {
    use super::*;
    use sp_runtime::{
        generic,
        traits::{BlakeTwo256, Hash as HashT},
    };

    pub use sp_runtime::OpaqueExtrinsic as UncheckedExtrinsic;

    /// Opaque block header type.
    pub type Header = generic::Header<BlockNumber, BlakeTwo256>;
    /// Opaque block type.
    pub type Block = generic::Block<Header, UncheckedExtrinsic>;
    /// Opaque block identifier type.
    pub type BlockId = generic::BlockId<Block>;
    /// Opaque block hash type.
    pub type Hash = <BlakeTwo256 as HashT>::Output;
}

impl_opaque_keys! {
    pub struct SessionKeys {
        pub aura: Aura,
        pub grandpa: Grandpa,
    }
}

// To learn more about runtime versioning, see:
// https://docs.substrate.io/main-docs/build/upgrade#runtime-versioning
#[cfg(not(feature = "gpu-pow-v2"))]
#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
    spec_name: alloc::borrow::Cow::Borrowed("eagle-runtime"),
    impl_name: alloc::borrow::Cow::Borrowed("eagle-runtime"),
    authoring_version: 1,
    // The version of the runtime specification. A full node will not attempt to use its native
    //   runtime in substitute for the on-chain Wasm runtime unless all of `spec_name`,
    //   `spec_version`, and `authoring_version` are the same between Wasm and native.
    // This value is set to 100 to notify Polkadot-JS App (https://polkadot.js.org/apps) to use
    //   the compatible custom types.
    spec_version: 104,
    impl_version: 1,
    apis: apis::RUNTIME_API_VERSIONS,
    transaction_version: 1,
    system_version: 1,
};

#[cfg(all(feature = "gpu-pow-v2", not(feature = "gpu-pow-rewards-dev")))]
#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
    spec_name: alloc::borrow::Cow::Borrowed("eagle-pow-v2-runtime"),
    impl_name: alloc::borrow::Cow::Borrowed("eagle-pow-v2-runtime"),
    authoring_version: 1,
    // This is a separate experimental runtime and genesis, not a Runtime104
    // upgrade or a database-compatible replacement for the Aura chain.
    spec_version: 1,
    impl_version: 1,
    apis: apis::RUNTIME_API_VERSIONS,
    transaction_version: 1,
    system_version: 1,
};

#[cfg(all(
    feature = "gpu-pow-rewards-dev",
    not(feature = "gpu-pow-v3-rewards-dev")
))]
#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
    spec_name: alloc::borrow::Cow::Borrowed("eagle-pow-rewards-dev-runtime"),
    impl_name: alloc::borrow::Cow::Borrowed("eagle-pow-rewards-dev-runtime"),
    authoring_version: 1,
    spec_version: 1,
    impl_version: 1,
    apis: apis::RUNTIME_API_VERSIONS,
    transaction_version: 1,
    system_version: 1,
};

#[cfg(all(
    feature = "gpu-pow-v3-rewards-dev",
    not(feature = "gpu-pow-v3-block-rewards-dev")
))]
#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
    spec_name: alloc::borrow::Cow::Borrowed("eagle-pow-v3-rewards-dev-runtime"),
    impl_name: alloc::borrow::Cow::Borrowed("eagle-pow-v3-rewards-dev-runtime"),
    authoring_version: 1,
    spec_version: 1,
    impl_version: 1,
    apis: apis::RUNTIME_API_VERSIONS,
    transaction_version: 1,
    system_version: 1,
};

#[cfg(all(feature = "gpu-pow-v3-block-rewards-dev", not(feature = "gpu-pow-matrix-block-rewards-dev")))]
#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
    spec_name: alloc::borrow::Cow::Borrowed("eagle-pow-v3-block-rewards-dev-runtime"),
    impl_name: alloc::borrow::Cow::Borrowed("eagle-pow-v3-block-rewards-dev-runtime"),
    authoring_version: 1,
    spec_version: 1,
    impl_version: 1,
    apis: apis::RUNTIME_API_VERSIONS,
    transaction_version: 1,
    system_version: 1,
};

/// Separate research genesis, never an in-place replacement of an existing chain.
#[cfg(all(feature = "gpu-pow-matrix-block-rewards-dev", not(feature = "svy-matrix-live")))]
#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
    spec_name: alloc::borrow::Cow::Borrowed("eagle-pow-matrix-block-rewards-dev-runtime"),
    impl_name: alloc::borrow::Cow::Borrowed("eagle-pow-matrix-block-rewards-dev-runtime"),
    authoring_version: 1,
    spec_version: 1,
    impl_version: 1,
    apis: apis::RUNTIME_API_VERSIONS,
    transaction_version: 1,
    system_version: 1,
};

#[cfg(all(feature = "svy-matrix-live", not(feature = "svy-transfer-fee-candidate")))]
#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
    spec_name: alloc::borrow::Cow::Borrowed("solvyren-matrix-runtime"),
    impl_name: alloc::borrow::Cow::Borrowed("solvyren-matrix-runtime"),
    authoring_version: 1,
    spec_version: 5,
    impl_version: 1,
    apis: apis::RUNTIME_API_VERSIONS,
    transaction_version: 1,
    system_version: 1,
};

#[cfg(feature = "svy-transfer-fee-candidate")]
#[sp_version::runtime_version]
pub const VERSION: RuntimeVersion = RuntimeVersion {
    spec_name: alloc::borrow::Cow::Borrowed("solvyren-matrix-runtime"),
    impl_name: alloc::borrow::Cow::Borrowed("solvyren-matrix-runtime"),
    authoring_version: 1,
    spec_version: 6,
    impl_version: 1,
    apis: apis::RUNTIME_API_VERSIONS,
    transaction_version: 1,
    system_version: 1,
};

mod block_times {
    /// This determines the average expected block time that we are targeting. Blocks will be
    /// produced at a minimum duration defined by `SLOT_DURATION`. `SLOT_DURATION` is picked up by
    /// `pallet_timestamp` which is in turn picked up by `pallet_aura` to implement `fn
    /// slot_duration()`.
    ///
    /// Change this to adjust the block time.
    pub const MILLI_SECS_PER_BLOCK: u64 = 10000;

    // NOTE: Currently it is not possible to change the slot duration after the chain has started.
    // Attempting to do so will brick block production.
    pub const SLOT_DURATION: u64 = MILLI_SECS_PER_BLOCK;
}
pub use block_times::*;

// Time is measured by number of blocks.
pub const MINUTES: BlockNumber = 60_000 / (MILLI_SECS_PER_BLOCK as BlockNumber);
pub const HOURS: BlockNumber = MINUTES * 60;
pub const DAYS: BlockNumber = HOURS * 24;

pub const BLOCK_HASH_COUNT: BlockNumber = 2400;

// Unit = the base number of indivisible units for balances
pub const UNIT: Balance = 1_000_000_000_000;
pub const MILLI_UNIT: Balance = 1_000_000_000;
pub const MICRO_UNIT: Balance = 1_000_000;

/// Existential deposit.
// Experimental block-schedule chain only: every non-zero atomic reward can
// create an account. Production requires a separate state-growth/fee review.
#[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
pub const EXISTENTIAL_DEPOSIT: Balance = 1;
#[cfg(not(feature = "gpu-pow-v3-block-rewards-dev"))]
pub const EXISTENTIAL_DEPOSIT: Balance = MILLI_UNIT;

/// The version information used to identify this runtime when compiled natively.
#[cfg(feature = "std")]
pub fn native_version() -> NativeVersion {
    NativeVersion {
        runtime_version: VERSION,
        can_author_with: Default::default(),
    }
}

/// Alias to 512-bit hash when used in the context of a transaction signature on the chain.
pub type Signature = MultiSignature;

/// Some way of identifying an account on the chain. We intentionally make it equivalent
/// to the public key of our transaction signing scheme.
pub type AccountId = <<Signature as Verify>::Signer as IdentifyAccount>::AccountId;

/// Consensus payload that binds an experimental PoW candidate to the account
/// that may receive its future block reward. The payload is included as the
/// single `pow_` pre-runtime digest, so it is covered by the unsealed header
/// hash and therefore by the GPU work challenge.
#[derive(Clone, Debug, Decode, Encode, Eq, MaxEncodedLen, PartialEq, TypeInfo)]
pub struct PowBeneficiaryV1 {
    pub version: u8,
    pub beneficiary: AccountId,
}

impl PowBeneficiaryV1 {
    pub const VERSION: u8 = 1;

    pub fn new(beneficiary: AccountId) -> Self {
        Self {
            version: Self::VERSION,
            beneficiary,
        }
    }

    pub fn encode_payload(&self) -> Vec<u8> {
        self.encode()
    }

    pub fn decode_payload(bytes: &[u8]) -> Result<Self, &'static str> {
        let claim = Self::decode_all(&mut &bytes[..])
            .map_err(|_| "PoW beneficiary payload must decode exactly")?;
        if claim.version != Self::VERSION {
            return Err("unsupported PoW beneficiary payload version");
        }
        Ok(claim)
    }
}

pub const POW_ENGINE_ID: [u8; 4] = *b"pow_";
/// The isolated reward runtime accepts exactly one unsealed digest item: the
/// versioned PoW beneficiary claim. Keeping this shape fixed gives the reward
/// hook a finite digest parsing bound; sealed headers add one final PoW seal at
/// the node boundary.
pub const POW_UNSEALED_DIGEST_ITEMS: usize = 1;

/// Extract exactly one versioned beneficiary claim from an unsealed PoW
/// header. This parser is shared by node verification and future runtime
/// reward logic so they cannot disagree on payload framing.
pub fn pow_beneficiary_from_digest(
    digest: &generic::Digest,
) -> Result<PowBeneficiaryV1, &'static str> {
    if digest.logs.len() != POW_UNSEALED_DIGEST_ITEMS {
        return Err("unsealed PoW header must contain exactly one digest item");
    }
    match &digest.logs[0] {
        DigestItem::PreRuntime(id, bytes) if *id == POW_ENGINE_ID => {
            PowBeneficiaryV1::decode_payload(bytes)
        }
        _ => Err("unsealed PoW header must contain only the beneficiary digest"),
    }
}

#[cfg(test)]
mod pow_beneficiary_tests {
    use super::*;

    #[test]
    fn beneficiary_digest_is_exact_unique_and_versioned() {
        let account = AccountId::new([37; 32]);
        let payload = PowBeneficiaryV1::new(account.clone()).encode_payload();
        let valid = generic::Digest {
            logs: alloc::vec![DigestItem::PreRuntime(POW_ENGINE_ID, payload.clone())],
        };
        assert_eq!(
            pow_beneficiary_from_digest(&valid).unwrap().beneficiary,
            account
        );

        assert!(pow_beneficiary_from_digest(&generic::Digest::default()).is_err());
        let duplicate = generic::Digest {
            logs: alloc::vec![
                DigestItem::PreRuntime(POW_ENGINE_ID, payload.clone()),
                DigestItem::PreRuntime(POW_ENGINE_ID, payload.clone()),
            ],
        };
        assert!(pow_beneficiary_from_digest(&duplicate).is_err());

        let extra_other = generic::Digest {
            logs: alloc::vec![
                DigestItem::PreRuntime(POW_ENGINE_ID, payload.clone()),
                DigestItem::Other(alloc::vec![0]),
            ],
        };
        assert!(pow_beneficiary_from_digest(&extra_other).is_err());
        let extra_consensus = generic::Digest {
            logs: alloc::vec![
                DigestItem::PreRuntime(POW_ENGINE_ID, payload.clone()),
                DigestItem::Consensus(*b"test", alloc::vec![0]),
            ],
        };
        assert!(pow_beneficiary_from_digest(&extra_consensus).is_err());

        let mut trailing = payload.clone();
        trailing.push(0);
        assert!(PowBeneficiaryV1::decode_payload(&trailing).is_err());
        let mut wrong_version = payload;
        wrong_version[0] = PowBeneficiaryV1::VERSION + 1;
        assert!(PowBeneficiaryV1::decode_payload(&wrong_version).is_err());
    }
}

/// Balance of an account.
pub type Balance = u128;

/// Index of a transaction in the chain.
pub type Nonce = u32;

/// A hash of some data used by the chain.
pub type Hash = sp_core::H256;

/// An index to a block.
pub type BlockNumber = u32;

/// The address format for describing accounts.
pub type Address = MultiAddress<AccountId, ()>;

/// Block header type as expected by this runtime.
pub type Header = generic::Header<BlockNumber, BlakeTwo256>;

/// Block type as expected by this runtime.
pub type Block = generic::Block<Header, UncheckedExtrinsic>;

/// A Block signed with a Justification
pub type SignedBlock = generic::SignedBlock<Block>;

/// BlockId type as expected by this runtime.
pub type BlockId = generic::BlockId<Block>;

macro_rules! transaction_extensions {
    ($($extra:ty),*) => { (
    frame_system::AuthorizeCall<Runtime>,
    frame_system::CheckNonZeroSender<Runtime>,
    frame_system::CheckSpecVersion<Runtime>,
    frame_system::CheckTxVersion<Runtime>,
    frame_system::CheckGenesis<Runtime>,
    frame_system::CheckEra<Runtime>,
    frame_system::CheckNonce<Runtime>,
    frame_system::CheckWeight<Runtime>,
    PaymentExtension,
    frame_metadata_hash_extension::CheckMetadataHash<Runtime>,
    frame_system::WeightReclaim<Runtime>,
    $($extra,)*
    ) };
}

/// The `TransactionExtension` to the basic transaction logic.
#[cfg(not(any(feature = "incinerator-rehearsal", feature = "native-dead-mainnet-drill")))]
pub type TxExtension = transaction_extensions!();
#[cfg(any(feature = "incinerator-rehearsal", feature = "native-dead-mainnet-drill"))]
pub type TxExtension = transaction_extensions!(native_incinerator_rehearsal::NativeIncinerator);

/// Unchecked extrinsic type as expected by this runtime.
pub type UncheckedExtrinsic =
    generic::UncheckedExtrinsic<Address, RuntimeCall, Signature, TxExtension>;

/// The payload being signed in transactions.
pub type SignedPayload = generic::SignedPayload<RuntimeCall, TxExtension>;

/// All migrations of the runtime, aside from the ones declared in the pallets.
///
/// This can be a tuple of types, each implementing `OnRuntimeUpgrade`.
#[allow(unused_parens)]
type Migrations = ();

/// Executive: handles dispatch to the various modules.
pub type Executive = frame_executive::Executive<
    Runtime,
    Block,
    frame_system::ChainContext<Runtime>,
    Runtime,
    AllPalletsWithSystem,
    Migrations,
>;

#[cfg(all(test, feature = "gpu-pow-rewards-dev"))]
mod reward_wasm_execution_tests {
    use super::*;
    use codec::{DecodeAll, Encode};
    use sc_executor::{
        HeapAllocStrategy, WasmExecutionMethod, WasmExecutor, WasmtimeInstantiationStrategy,
    };
    use sp_core::traits::{CallContext, CodeExecutor, RuntimeCode, WrappedRuntimeCode};
    use sp_core::Pair as _;
    use sp_runtime::{
        generic::Digest,
        traits::{BlakeTwo256, Hash as HashT, Header as HeaderT},
        DigestItem, StateVersion,
    };

    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    #[test]
    fn one_atom_transfer_fee_lower_bound_without_signing() {
        use frame_support::{dispatch::GetDispatchInfo, traits::Contains};
        let call = RuntimeCall::Balances(pallet_balances::Call::transfer_keep_alive {
            dest: AccountId::new([8; 32]).into(),
            value: 1,
        });
        assert!(configs::RewardRuntimeCallFilter::contains(&call));
        sp_io::TestExternalities::default().execute_with(|| {
            let fee = TransactionPayment::compute_fee(0, &call.get_dispatch_info(), 0);
            println!("zero-length, zero-tip transfer fee lower bound: {fee} atoms");
            assert!(fee > 1);
        });
    }

    #[cfg(feature = "runtime-benchmarks")]
    type TestHostFunctions = (
        sp_io::SubstrateHostFunctions,
        frame_benchmarking::benchmarking::HostFunctions,
    );
    #[cfg(not(feature = "runtime-benchmarks"))]
    type TestHostFunctions = sp_io::SubstrateHostFunctions;

    fn executor() -> WasmExecutor<TestHostFunctions> {
        WasmExecutor::<TestHostFunctions>::builder()
            .with_execution_method(WasmExecutionMethod::Compiled {
                instantiation_strategy: WasmtimeInstantiationStrategy::RecreateInstance,
            })
            .with_max_runtime_instances(1)
            .with_runtime_cache_size(2)
            .with_onchain_heap_alloc_strategy(HeapAllocStrategy::Static { extra_pages: 1024 })
            .build()
    }

    fn call_wasm(
        executor: &WasmExecutor<TestHostFunctions>,
        method: &str,
        input: &[u8],
    ) -> Result<Vec<u8>, String> {
        let code = WASM_BINARY.expect("reward runtime Wasm must be embedded");
        let fetch = WrappedRuntimeCode(alloc::borrow::Cow::Borrowed(code));
        let runtime_code = RuntimeCode {
            code_fetcher: &fetch,
            heap_pages: None,
            hash: sp_core::blake2_256(code).to_vec(),
        };
        let (result, native) = sp_externalities::with_externalities(|externalities| {
            executor.call(
                externalities,
                &runtime_code,
                method,
                input,
                CallContext::Onchain,
            )
        })
        .expect("externalities are installed");
        assert!(!native, "test must execute embedded Wasm, not native code");
        result.map_err(|error| format!("{error:?}"))
    }

    fn valid_item(account: u8) -> DigestItem {
        valid_account_item(AccountId::new([account; 32]))
    }

    fn valid_account_item(account: AccountId) -> DigestItem {
        DigestItem::PreRuntime(POW_ENGINE_ID, PowBeneficiaryV1::new(account).encode())
    }

    fn run_builder(
        number: BlockNumber,
        parent_hash: Hash,
        items: Vec<DigestItem>,
        timestamp_millis: u64,
    ) -> Result<Header, String> {
        run_builder_with_extrinsics(number, parent_hash, items, timestamp_millis, vec![])
    }

    fn run_builder_with_extrinsics(
        number: BlockNumber,
        parent_hash: Hash,
        items: Vec<DigestItem>,
        timestamp_millis: u64,
        additional_extrinsics: Vec<UncheckedExtrinsic>,
    ) -> Result<Header, String> {
        let executor = executor();
        let timestamp = UncheckedExtrinsic::new_bare(RuntimeCall::Timestamp(TimestampCall::set {
            now: timestamp_millis,
        }));
        let mut extrinsics = vec![timestamp];
        extrinsics.extend(additional_extrinsics);
        let extrinsics_root = BlakeTwo256::ordered_trie_root(
            extrinsics.iter().map(Encode::encode).collect(),
            StateVersion::V1,
        );
        let header = Header::new(
            number,
            Default::default(),
            extrinsics_root,
            parent_hash,
            Digest { logs: items },
        );

        let initialized = call_wasm(&executor, "Core_initialize_block", &header.encode())?;
        let _: sp_runtime::ExtrinsicInclusionMode =
            DecodeAll::decode_all(&mut &initialized[..]).map_err(|error| format!("{error:?}"))?;
        for (index, extrinsic) in extrinsics.iter().enumerate() {
            let applied = call_wasm(
                &executor,
                "BlockBuilder_apply_extrinsic",
                &extrinsic.encode(),
            )?;
            let outcome: sp_runtime::ApplyExtrinsicResult =
                DecodeAll::decode_all(&mut &applied[..]).map_err(|error| format!("{error:?}"))?;
            if !matches!(outcome, Ok(Ok(()))) {
                return Err(format!("extrinsic {index} failed: {outcome:?}"));
            }
        }
        let finalized = call_wasm(&executor, "BlockBuilder_finalize_block", &[])?;
        Header::decode_all(&mut &finalized[..]).map_err(|error| format!("{error:?}"))
    }

    fn fresh_ext() -> sp_io::TestExternalities {
        let mut genesis = RuntimeGenesisConfig::default();
        #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
        let genesis_issued = {
            let amount = pallet_pow_rewards::block_height_schedule::TEAM_GENESIS;
            genesis.balances.balances = vec![(AccountId::new([9; 32]), amount)];
            amount
        };
        #[cfg(not(feature = "gpu-pow-v3-block-rewards-dev"))]
        let genesis_issued = 0;
        genesis.pow_rewards = pallet_pow_rewards::GenesisConfig {
            genesis_issued,
            initial_timestamp_seconds: 1_793_491_200,
            _marker: Default::default(),
        };
        genesis
            .build_storage()
            .expect("reward genesis storage")
            .into()
    }

    type RawSnapshot = (Vec<(Vec<u8>, (Vec<u8>, i32))>, Hash);

    fn snapshot(ext: sp_io::TestExternalities) -> RawSnapshot {
        ext.into_raw_snapshot()
    }

    fn from_snapshot(raw: &RawSnapshot) -> sp_io::TestExternalities {
        sp_io::TestExternalities::from_raw_snapshot(raw.0.clone(), raw.1, StateVersion::V1)
    }

    fn signed_call(sender: sp_core::sr25519::Pair, call: RuntimeCall) -> UncheckedExtrinsic {
        let genesis_hash = Hash::zero();
        let tx_ext: TxExtension = (
            frame_system::AuthorizeCall::<Runtime>::new(),
            frame_system::CheckNonZeroSender::<Runtime>::new(),
            frame_system::CheckSpecVersion::<Runtime>::new(),
            frame_system::CheckTxVersion::<Runtime>::new(),
            frame_system::CheckGenesis::<Runtime>::new(),
            frame_system::CheckEra::<Runtime>::from(sp_runtime::generic::Era::Immortal),
            frame_system::CheckNonce::<Runtime>::from(0),
            frame_system::CheckWeight::<Runtime>::new(),
            PaymentExtension::from(0),
            frame_metadata_hash_extension::CheckMetadataHash::<Runtime>::new(false),
            frame_system::WeightReclaim::<Runtime>::new(),
        );
        let payload = SignedPayload::from_raw(
            call.clone(),
            tx_ext.clone(),
            (
                (),
                (),
                VERSION.spec_version,
                VERSION.transaction_version,
                genesis_hash,
                genesis_hash,
                (),
                (),
                (),
                None,
                (),
            ),
        );
        let signature = payload.using_encoded(|encoded| sender.sign(encoded));
        UncheckedExtrinsic::new_signed(
            call,
            AccountId::from(sender.public()).into(),
            Signature::Sr25519(signature),
            tx_ext,
        )
    }

    fn signed_burn(sender: sp_core::sr25519::Pair, value: Balance) -> UncheckedExtrinsic {
        signed_call(
            sender,
            RuntimeCall::Balances(pallet_balances::Call::burn {
                value,
                keep_alive: true,
            }),
        )
    }

    fn signed_remark(sender: sp_core::sr25519::Pair) -> UncheckedExtrinsic {
        signed_call(
            sender,
            RuntimeCall::System(frame_system::Call::remark {
                remark: b"eagw-gross-fee-accounting-v1".to_vec(),
            }),
        )
    }

    #[test]
    fn reward_dust_handler_records_credit_before_drop() {
        use frame_support::traits::{fungible::Balanced, OnUnbalanced};

        let mut ext = fresh_ext();
        ext.execute_with(|| {
            let before = Balances::total_issuance();
            let amount = EXISTENTIAL_DEPOSIT - 1;
            let credit = <Balances as Balanced<AccountId>>::issue(amount);
            assert_eq!(Balances::total_issuance(), before + amount);

            configs::PowRewardFeeHandler::on_unbalanced(credit);

            assert_eq!(Balances::total_issuance(), before);
            assert_eq!(
                PowRewards::ledger()
                    .expect("reward ledger")
                    .destroyed_issued,
                amount
            );
        });
    }

    #[test]
    fn embedded_reward_wasm_reserves_mandatory_hook_weight() {
        use frame_support::dispatch::DispatchClass;

        let mut ext = fresh_ext();
        ext.execute_with(|| {
            let executor = executor();
            let header = Header::new(
                1,
                Default::default(),
                BlakeTwo256::ordered_trie_root(Vec::<Vec<u8>>::new(), StateVersion::V1),
                Hash::zero(),
                Digest {
                    logs: vec![valid_item(7)],
                },
            );
            let initialized = call_wasm(&executor, "Core_initialize_block", &header.encode())
                .expect("reward block initialization must execute in Wasm");
            let _: sp_runtime::ExtrinsicInclusionMode =
                DecodeAll::decode_all(&mut &initialized[..]).expect("initialize result decodes");

            let expected =
                <<Runtime as pallet_pow_rewards::Config>::WeightInfo as pallet_pow_rewards::WeightInfo>::issue_current_block();
            let consumed = System::block_weight();
            let mandatory = consumed.get(DispatchClass::Mandatory);
            assert!(
                mandatory.ref_time() >= expected.ref_time(),
                "mandatory ref_time did not reserve the reward hook"
            );
            assert!(
                mandatory.proof_size() >= expected.proof_size(),
                "mandatory proof_size did not reserve the reward hook"
            );
        });
    }

    #[test]
    fn embedded_reward_wasm_accepts_one_exact_beneficiary() {
        let mut ext = fresh_ext();
        ext.execute_with(|| {
            let finalized = run_builder(1, Hash::zero(), vec![valid_item(7)], 1_793_491_210_000)
                .expect("valid reward block must finalize in Wasm");
            assert_ne!(*finalized.state_root(), Default::default());
        });
    }

    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    fn expected_ai_after_empty_block(previously_issued: u128, scheduled: u128) -> u128 {
        if cfg!(feature = "svy-matrix-live") { previously_issued } else { scheduled * pallet_pow_rewards::AI_PERCENT / 100 }
    }

    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    #[test]
    fn block_height_candidate_wasm_execute_full_boundary_blocks() {
        use frame_support::traits::Currency;
        use pallet_pow_rewards::block_height_schedule::TEAM_GENESIS;
        // Independently calculated prior scheduled totals at period boundaries.
        #[cfg(not(feature = "svy-matrix-live"))]
        let parents = [
            (219_001u32, 14_203_385_801_731_213_258u128),
            (5_256_001u32, 237_500_000_000_000_000_000u128),
        ];
        #[cfg(feature = "svy-matrix-live")]
        let parents = [(1_576_801u32, 23_652_000_000_000_000_000u128), (3_153_601, 44_506_914_480_000_000_000)];
        for (height, before) in parents {
            let team = AccountId::new([9; 32]);
            let miner = AccountId::new([7; 32]);
            let pool = <Runtime as pallet_pow_rewards::Config>::AiPool::get();
            let mut genesis = RuntimeGenesisConfig::default();
            genesis.balances.balances = vec![(team.clone(), TEAM_GENESIS)];
            genesis.pow_rewards = pallet_pow_rewards::GenesisConfig {
                genesis_issued: TEAM_GENESIS,
                initial_timestamp_seconds: 0,
                _marker: Default::default(),
            };
            let mut parent: sp_io::TestExternalities = genesis.build_storage().unwrap().into();
            parent.execute_with(|| {
                let ai = before * pallet_pow_rewards::AI_PERCENT / 100;
                drop(<Balances as Currency<AccountId>>::deposit_creating(
                    &miner,
                    before - ai,
                ));
                drop(<Balances as Currency<AccountId>>::deposit_creating(
                    &pool, ai,
                ));
                pallet_pow_rewards::Ledger::<Runtime>::mutate(|value| {
                    let ledger = value.as_mut().unwrap();
                    ledger.last_height = u64::from(height - 1);
                    ledger.scheduled_issued = before;
                    ledger.lifetime_issued = TEAM_GENESIS + before;
                    ledger.base_issued = before - ai;
                    ledger.ai_issued = ai;
                });
                System::set_block_number(height - 1);
            });
            parent.commit_all().unwrap();
            let parent_snapshot = snapshot(parent);
            let mut author = from_snapshot(&parent_snapshot);
            let timestamp_millis = 1_856_649_610_000;
            let header = author.execute_with(|| {
                run_builder(
                    height,
                    Hash::zero(),
                    vec![valid_account_item(miner.clone())],
                    timestamp_millis,
                )
                .expect("boundary authoring succeeds")
            });
            author.commit_all().unwrap();
            let author_root = snapshot(author).1;
            let body = vec![UncheckedExtrinsic::new_bare(RuntimeCall::Timestamp(
                TimestampCall::set {
                    now: timestamp_millis,
                },
            ))];
            let block = Block {
                header,
                extrinsics: body,
            };
            let mut importer = from_snapshot(&parent_snapshot);
            importer.execute_with(|| {
                let output = call_wasm(&executor(), "Core_execute_block", &block.encode())
                    .expect("full boundary block must execute in Wasm");
                <()>::decode_all(&mut &output[..]).expect("execute result decodes");
                let ledger = PowRewards::ledger().unwrap();
                assert_eq!(ledger.last_height, u64::from(height));
                let budget = ledger.scheduled_issued * pallet_pow_rewards::AI_PERCENT / 100;
                assert_eq!(ledger.base_issued, ledger.scheduled_issued - budget);
                assert_eq!(ledger.ai_issued, expected_ai_after_empty_block(before * pallet_pow_rewards::AI_PERCENT / 100, ledger.scheduled_issued));
                assert_eq!(Balances::total_issuance(), ledger.lifetime_issued);
            });
            importer.commit_all().unwrap();
            assert_eq!(
                snapshot(importer).1,
                author_root,
                "author and full execution roots match at {height}"
            );
        }
    }

    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    #[test]
    fn block_height_candidate_wasm_actual_period_boundaries() {
        use frame_support::traits::{Currency, Get};
        use pallet_pow_rewards::block_height_schedule::{CAP, TEAM_GENESIS};
        // Independent Python arithmetic oracle: height, prior scheduled total,
        // scheduled total after the tested block. Each parent is synthetic.
        #[cfg(not(feature = "svy-matrix-live"))]
        let cases: [(u32, u128, u128); 6] = [
            (
                219_000,
                14_203_320_946_088_282_978,
                14_203_385_801_731_213_258,
            ),
            (
                219_001,
                14_203_385_801_731_213_258,
                14_203_448_502_591_177_557,
            ),
            (
                2_628_000,
                142_499_955_277_015_102_500,
                142_500_000_000_000_000_000,
            ),
            (
                2_628_001,
                142_500_000_000_000_000_000,
                142_500_043_237_095_286_925,
            ),
            (
                5_256_000,
                237_499_970_184_676_735_086,
                237_500_000_000_000_000_000,
            ),
            (
                5_256_001,
                237_500_000_000_000_000_000,
                237_500_029_544_568_307_445,
            ),
        ];
        #[cfg(feature = "svy-matrix-live")]
        let cases: [(u32, u128, u128); 6] = [
            (1, 0, 15_000_000_000_000),
            (1_576_800, 23_651_985_000_000_000_000, 23_652_000_000_000_000_000),
            (1_576_801, 23_652_000_000_000_000_000, 23_652_013_226_100_000_000),
            (3_153_600, 44_506_901_253_900_000_000, 44_506_914_480_000_000_000),
            (3_153_601, 44_506_914_480_000_000_000, 44_506_926_141_981_414_000),
            (4_730_400, 62_895_515_111_613_786_000, 62_895_526_773_595_200_000),
        ];
        let team = AccountId::new([9; 32]);
        let miner = AccountId::new([7; 32]);
        let ai_pool = <Runtime as pallet_pow_rewards::Config>::AiPool::get();
        for (height, before, after) in cases {
            let mut genesis = RuntimeGenesisConfig::default();
            genesis.balances.balances = vec![(team.clone(), TEAM_GENESIS)];
            genesis.pow_rewards = pallet_pow_rewards::GenesisConfig {
                genesis_issued: TEAM_GENESIS,
                initial_timestamp_seconds: 0,
                _marker: Default::default(),
            };
            let mut ext: sp_io::TestExternalities = genesis.build_storage().unwrap().into();
            ext.execute_with(|| {
                let old_ai = before * pallet_pow_rewards::AI_PERCENT / 100;
                let old_base = before - old_ai;
                drop(<Balances as Currency<AccountId>>::deposit_creating(
                    &miner, old_base,
                ));
                drop(<Balances as Currency<AccountId>>::deposit_creating(
                    &ai_pool, old_ai,
                ));
                pallet_pow_rewards::Ledger::<Runtime>::mutate(|value| {
                    let ledger = value.as_mut().unwrap();
                    ledger.last_height = u64::from(height - 1);
                    ledger.scheduled_issued = before;
                    ledger.lifetime_issued = TEAM_GENESIS + before;
                    ledger.base_issued = old_base;
                    ledger.ai_issued = old_ai;
                });
                System::set_block_number(height - 1);
                run_builder(
                    height,
                    Hash::zero(),
                    vec![valid_account_item(miner.clone())],
                    1_856_649_610_000,
                )
                .expect("boundary block must execute in Wasm");
                let ledger = PowRewards::ledger().unwrap();
                assert_eq!(ledger.scheduled_issued, after, "height {height}");
                assert_eq!(ledger.ai_issued, expected_ai_after_empty_block(old_ai, after));
                assert_eq!(ledger.base_issued, after - after * pallet_pow_rewards::AI_PERCENT / 100);
                assert_eq!(ledger.lifetime_issued, TEAM_GENESIS + ledger.base_issued + ledger.ai_issued);
                assert_eq!(Balances::total_issuance(), ledger.lifetime_issued);
                assert_eq!(Balances::free_balance(&team), TEAM_GENESIS);
                assert_eq!(Balances::free_balance(&miner), ledger.base_issued);
                assert_eq!(Balances::free_balance(&ai_pool), ledger.ai_issued);
                assert!(ledger.lifetime_issued <= CAP);
            });
        }
    }

    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    #[test]
    fn block_height_candidate_wasm_small_and_zero_rewards_do_not_stall() {
        use pallet_pow_rewards::block_height_schedule::{quote, PERIOD_BLOCKS, TEAM_GENESIS};
        assert_eq!(EXISTENTIAL_DEPOSIT, 1);
        let team = AccountId::new([9; 32]);
        let miner = AccountId::new([7; 32]);
        let periods = if cfg!(feature = "svy-matrix-live") { [400u64, 401] } else { [2_000, 10_000] };
        for period in periods {
            let height = period * PERIOD_BLOCKS + 1;
            let reward = quote(height, TEAM_GENESIS, 0).unwrap();
            if period == 2_000 {
                assert!(
                    reward > 0 && reward < MILLI_UNIT,
                    "fixture must exercise old sub-ED failure"
                );
            } else {
                assert_eq!(reward, 0, "fixture must exercise zero-reward tail");
            }
            let mut genesis = RuntimeGenesisConfig::default();
            genesis.balances.balances = vec![(team.clone(), TEAM_GENESIS)];
            genesis.pow_rewards = pallet_pow_rewards::GenesisConfig {
                genesis_issued: TEAM_GENESIS,
                initial_timestamp_seconds: 0,
                _marker: Default::default(),
            };
            let mut ext: sp_io::TestExternalities = genesis.build_storage().unwrap().into();
            // Synthetic parent state: isolates a far-future reward without
            // claiming that preceding hundreds of millions of blocks ran.
            ext.execute_with(|| {
                pallet_pow_rewards::Ledger::<Runtime>::mutate(|ledger| {
                    ledger.as_mut().unwrap().last_height = height - 1;
                });
                System::set_block_number((height - 1).try_into().unwrap());
                assert_eq!(Balances::free_balance(&miner), 0);
                run_builder(
                    height.try_into().unwrap(),
                    Hash::zero(),
                    vec![valid_account_item(miner.clone())],
                    1_856_649_610_000,
                )
                .expect("small or zero reward must not block Wasm finalization");
                let ledger = PowRewards::ledger().unwrap();
                let ai = reward * pallet_pow_rewards::AI_PERCENT / 100;
                assert_eq!(ledger.last_height, height);
                assert_eq!(ledger.scheduled_issued, reward);
                assert_eq!(ledger.ai_issued, expected_ai_after_empty_block(0, reward));
                assert_eq!(Balances::free_balance(&miner), reward - ai);
                assert_eq!(Balances::total_issuance(), TEAM_GENESIS + reward - ai + ledger.ai_issued);
            });
        }
    }

    #[cfg(feature = "gpu-pow-v3-block-rewards-dev")]
    #[test]
    fn block_height_candidate_wasm_funded_two_blocks_after_old_time_boundary() {
        use pallet_pow_rewards::block_height_schedule::{quote, CAP, TEAM_GENESIS};
        let team = AccountId::new([9; 32]);
        let miner = AccountId::new([7; 32]);
        let mut genesis = RuntimeGenesisConfig::default();
        genesis.balances.balances = vec![(team.clone(), TEAM_GENESIS)];
        genesis.pow_rewards = pallet_pow_rewards::GenesisConfig {
            genesis_issued: TEAM_GENESIS,
            initial_timestamp_seconds: 0,
            _marker: Default::default(),
        };
        let mut ext: sp_io::TestExternalities = genesis.build_storage().unwrap().into();
        let mut parent = Hash::zero();
        let mut scheduled = 0u128;
        for height in 1..=2u32 {
            let reward = quote(u64::from(height), TEAM_GENESIS, scheduled).unwrap();
            scheduled += reward;
            let header = ext.execute_with(|| {
                let header = run_builder(
                    height,
                    parent,
                    vec![valid_account_item(miner.clone())],
                    (1_856_649_610u64 + u64::from(height) * 10) * 1_000,
                )
                .expect("block-height candidate must finalize after former calendar stop");
                let ledger = PowRewards::ledger().expect("ledger exists");
                assert_eq!(ledger.genesis_issued, TEAM_GENESIS);
                assert_eq!(ledger.scheduled_issued, scheduled);
                assert_eq!(ledger.lifetime_issued, TEAM_GENESIS + ledger.base_issued + ledger.ai_issued);
                assert!(ledger.lifetime_issued <= CAP);
                assert_eq!(Balances::total_issuance(), ledger.lifetime_issued);
                assert_eq!(Balances::free_balance(&team), TEAM_GENESIS);
                assert_eq!(Balances::free_balance(&miner), ledger.base_issued);
                assert_eq!(ledger.ai_issued, expected_ai_after_empty_block(0, scheduled));
                assert_eq!(ledger.base_issued, scheduled - scheduled * pallet_pow_rewards::AI_PERCENT / 100);
                header
            });
            ext.commit_all().expect("commit valid block");
            parent = header.hash();
        }
    }

    #[cfg(not(feature = "gpu-pow-v3-block-rewards-dev"))]
    #[test]
    fn embedded_reward_wasm_preserves_five_percent_genesis_and_split() {
        use pallet_pow_rewards::gregorian::{RateEmission, UtcDate, CAP};
        let genesis_amount = CAP * 5 / 100;
        let treasury = AccountId::new([9; 32]);
        let beneficiary = AccountId::new([7; 32]);
        let mut genesis = RuntimeGenesisConfig::default();
        genesis.balances.balances = vec![(treasury.clone(), genesis_amount)];
        genesis.pow_rewards = pallet_pow_rewards::GenesisConfig {
            genesis_issued: genesis_amount,
            initial_timestamp_seconds: 1_793_491_200,
            _marker: Default::default(),
        };
        let mut ext: sp_io::TestExternalities = genesis.build_storage().unwrap().into();
        let plan = RateEmission::new(
            UtcDate {
                year: 2026,
                month: 11,
                day: 1,
            },
            genesis_amount,
        )
        .unwrap();
        let timestamp = plan.month_bounds(11).unwrap().1;
        ext.execute_with(|| {
            run_builder(
                1,
                Hash::zero(),
                vec![valid_account_item(beneficiary.clone())],
                timestamp * 1000,
            )
            .expect("funded genesis block must finalize in Wasm");
            let ledger = PowRewards::ledger().unwrap();
            let issued = (CAP - genesis_amount) * 15 / 100;
            assert_eq!(ledger.genesis_issued, genesis_amount);
            assert_eq!(ledger.scheduled_issued, issued);
            assert_eq!(ledger.lifetime_issued, genesis_amount + issued);
            assert_eq!(Balances::total_issuance(), genesis_amount + issued);
            assert_eq!(Balances::free_balance(treasury), genesis_amount);
            assert_eq!(ledger.ai_issued, issued * pallet_pow_rewards::AI_PERCENT / 100);
            assert_eq!(ledger.base_issued + ledger.ai_issued, issued);
            assert_eq!(Balances::free_balance(beneficiary), ledger.base_issued);
        });
    }

    #[cfg(not(feature = "gpu-pow-v3-block-rewards-dev"))]
    #[test]
    fn embedded_reward_wasm_characterizes_approved_gregorian_boundaries() {
        use pallet_pow_rewards::gregorian::{RateEmission, UtcDate, CAP};

        let plan = RateEmission::new(
            UtcDate {
                year: 2026,
                month: 11,
                day: 1,
            },
            0,
        )
        .expect("approved first-24-month schedule");
        let checkpoints = [
            (1u32, plan.month_bounds(0).unwrap().1, "month-1-end"),
            (2u32, plan.month_bounds(11).unwrap().1, "month-12-end"),
            (
                3u32,
                plan.month_bounds(23).unwrap().1 - 1,
                "month-24-last-approved-second",
            ),
        ];
        let beneficiary = AccountId::new([7; 32]);
        let ai_pool = <Runtime as pallet_pow_rewards::Config>::AiPool::get();
        let mut ext = fresh_ext();
        let mut parent_hash = Hash::zero();

        for (height, timestamp_seconds, name) in checkpoints {
            let header = ext.execute_with(|| {
                let header = run_builder(
                    height,
                    parent_hash,
                    vec![valid_account_item(beneficiary.clone())],
                    timestamp_seconds * 1_000,
                )
                .unwrap_or_else(|error| panic!("{name} did not finalize in Wasm: {error}"));
                let expected = plan
                    .target_cumulative(timestamp_seconds)
                    .expect("checkpoint remains inside the approved schedule");
                let ledger = PowRewards::ledger().expect("reward ledger");
                assert_eq!(ledger.scheduled_issued, expected, "{name} schedule");
                assert_eq!(ledger.lifetime_issued, expected, "{name} lifetime");
                assert_eq!(Balances::total_issuance(), expected, "{name} supply");
                assert_eq!(ledger.ai_issued, expected * pallet_pow_rewards::AI_PERCENT / 100, "{name} AI");
                assert_eq!(
                    ledger.base_issued + ledger.ai_issued,
                    expected,
                    "{name} split conservation"
                );
                assert_eq!(
                    Balances::free_balance(&beneficiary),
                    ledger.base_issued,
                    "{name} beneficiary balance"
                );
                assert_eq!(
                    Balances::free_balance(&ai_pool),
                    ledger.ai_issued,
                    "{name} AI-pool balance"
                );
                header
            });
            ext.commit_all()
                .unwrap_or_else(|error| panic!("{name} parent commit failed: {error:?}"));
            parent_hash = header.hash();
        }

        let last_approved = plan.month_bounds(23).unwrap().1 - 1;
        let issued_at_last_second = plan.target_cumulative(last_approved).unwrap();
        let unissued_policy_gap = CAP * 25 / 100 - issued_at_last_second;
        // This records the current fixed-start catch-up behavior and the final
        // one-second gap; neither assertion approves catch-up or a tail policy.
        assert_eq!(unissued_policy_gap, 2_794_825_538_538);

        let parent = snapshot(ext);
        let mut rejected = from_snapshot(&parent);
        let result = rejected.execute_with(|| {
            run_builder(
                4,
                parent_hash,
                vec![valid_account_item(beneficiary)],
                plan.month_bounds(23).unwrap().1 * 1_000,
            )
        });
        assert!(
            result.is_err(),
            "month-24 end unexpectedly defined an unapproved tail policy"
        );
        assert_eq!(
            parent.1,
            snapshot(rejected).1,
            "month-24 end changed durable parent state"
        );
    }

    #[test]
    fn embedded_reward_wasm_rejects_invalid_context_without_parent_commit() {
        let mut trailing = PowBeneficiaryV1::new(AccountId::new([7; 32])).encode();
        trailing.push(0);
        let mut wrong_version = PowBeneficiaryV1::new(AccountId::new([7; 32])).encode();
        wrong_version[0] = PowBeneficiaryV1::VERSION + 1;
        let cases = vec![
            ("missing", vec![], 1_793_491_210_000),
            (
                "duplicate",
                vec![valid_item(7), valid_item(7)],
                1_793_491_210_000,
            ),
            (
                "trailing-byte",
                vec![DigestItem::PreRuntime(POW_ENGINE_ID, trailing)],
                1_793_491_210_000,
            ),
            (
                "wrong-version",
                vec![DigestItem::PreRuntime(POW_ENGINE_ID, wrong_version)],
                1_793_491_210_000,
            ),
            (
                "wrong-engine",
                vec![DigestItem::PreRuntime(*b"bad_", vec![1, 2, 3])],
                1_793_491_210_000,
            ),
            // Executive::initialize_block retains only PreRuntime digests.
            // Other and Consensus entries require full-header import checks;
            // this BlockBuilder fixture cannot prove they are rejected.
            ("before-start", vec![valid_item(7)], 1_793_491_199_000),
        ];
        // Only the legacy calendar schedule rejects its undefined month-25
        // boundary. The block-height schedule has dedicated positive tail tests.
        #[cfg(not(feature = "gpu-pow-v3-block-rewards-dev"))]
        let cases = {
            let mut cases = cases;
            cases.push(("after-month-24", vec![valid_item(7)], 1_856_649_600_000));
            cases
        };

        let genesis = snapshot(fresh_ext());

        for (name, items, timestamp) in cases {
            let mut attempt = from_snapshot(&genesis);
            let result = attempt.execute_with(|| run_builder(1, Hash::zero(), items, timestamp));
            assert!(result.is_err(), "{name} unexpectedly finalized in Wasm");
            let durable = snapshot(attempt);
            assert_eq!(
                genesis.1, durable.1,
                "{name} changed durable parent backend"
            );
        }
    }

    #[test]
    fn embedded_reward_wasm_rejects_time_regression_and_repeated_height() {
        for case in ["time-regression", "repeated-height"] {
            let mut ext = fresh_ext();
            let first = ext.execute_with(|| {
                run_builder(1, Hash::zero(), vec![valid_item(7)], 1_793_491_210_000)
                    .expect("block one control")
            });
            ext.commit_all().expect("commit valid block-one control");
            let parent = snapshot(ext);
            let mut attempt = from_snapshot(&parent);
            let result = attempt.execute_with(|| match case {
                "time-regression" => {
                    run_builder(2, first.hash(), vec![valid_item(7)], 1_793_491_209_000)
                }
                "repeated-height" => {
                    run_builder(1, first.hash(), vec![valid_item(7)], 1_793_491_220_000)
                }
                _ => unreachable!(),
            });
            assert!(result.is_err(), "{case} unexpectedly finalized in Wasm");
            let durable = snapshot(attempt);
            assert_eq!(parent.1, durable.1, "{case} changed durable parent backend");
        }
    }

    #[test]
    fn embedded_reward_wasm_accepts_fee_only_with_gross_accounting() {
        let sender = sp_keyring::Sr25519Keyring::Alice.pair();
        let account = AccountId::from(sender.public());
        let mut ext = fresh_ext();
        let first = ext.execute_with(|| {
            run_builder(
                1,
                Hash::zero(),
                vec![valid_account_item(account.clone())],
                1_793_491_210_000,
            )
            .expect("block one funds the fee-paying account")
        });
        ext.commit_all().expect("commit valid block-one control");

        ext.execute_with(|| {
            run_builder_with_extrinsics(
                2,
                first.hash(),
                vec![valid_account_item(account.clone())],
                1_793_491_220_000,
                vec![signed_remark(sender)],
            )
            .expect("tracked fee-only block must finalize");
            let ledger = PowRewards::ledger().expect("reward ledger");
            assert!(ledger.destroyed_issued > 0);
            assert_eq!(
                Balances::total_issuance() + ledger.destroyed_issued,
                ledger.lifetime_issued
            );
            assert_eq!(System::account_nonce(account), 1);
        });
    }

    #[test]
    fn embedded_reward_wasm_rejects_filtered_signed_burn_without_parent_commit() {
        let sender = sp_keyring::Sr25519Keyring::Alice.pair();
        let account = AccountId::from(sender.public());
        let mut ext = fresh_ext();
        let first = ext.execute_with(|| {
            run_builder(
                1,
                Hash::zero(),
                vec![valid_account_item(account.clone())],
                1_793_491_210_000,
            )
            .expect("block one funds the signed burn account")
        });
        ext.commit_all().expect("commit valid block-one control");
        let parent = snapshot(ext);

        let mut positive = from_snapshot(&parent);
        positive.execute_with(|| {
            run_builder(
                2,
                first.hash(),
                vec![valid_account_item(account.clone())],
                1_793_491_220_000,
            )
            .expect("empty block two control must remain valid");
        });

        let mut attempt = from_snapshot(&parent);
        let (result, issuance_before, issuance_after, destroyed_after, nonce_after) = attempt
            .execute_with(|| {
                let issuance_before = Balances::total_issuance();
                let result = run_builder_with_extrinsics(
                    2,
                    first.hash(),
                    vec![valid_account_item(account.clone())],
                    1_793_491_220_000,
                    vec![signed_burn(sender, 1)],
                );
                (
                    result,
                    issuance_before,
                    Balances::total_issuance(),
                    PowRewards::ledger()
                        .expect("reward ledger")
                        .destroyed_issued,
                    System::account_nonce(account),
                )
            });
        assert!(result.is_err(), "burn block unexpectedly finalized in Wasm");
        assert!(
            issuance_before - issuance_after == destroyed_after,
            "signed burn reduced issuance beyond its tracked transaction fee"
        );
        assert!(
            issuance_after < issuance_before,
            "signed burn/fee extrinsic did not execute before reward rejection"
        );
        assert_eq!(
            nonce_after, 1,
            "signed burn extrinsic did not consume nonce"
        );
        let durable = snapshot(attempt);
        assert_eq!(
            parent.1, durable.1,
            "rejected burn attempt changed durable parent backend"
        );
    }
}

// Create the runtime by composing the FRAME pallets that were previously configured.
#[cfg(not(feature = "gpu-pow-rewards-dev"))]
#[frame_support::runtime]
mod runtime {
    #[runtime::runtime]
    #[runtime::derive(
        RuntimeCall,
        RuntimeEvent,
        RuntimeError,
        RuntimeOrigin,
        RuntimeFreezeReason,
        RuntimeHoldReason,
        RuntimeSlashReason,
        RuntimeLockId,
        RuntimeTask,
        RuntimeViewFunction
    )]
    pub struct Runtime;

    #[runtime::pallet_index(0)]
    pub type System = frame_system;

    #[runtime::pallet_index(1)]
    pub type Timestamp = pallet_timestamp;

    #[runtime::pallet_index(2)]
    pub type Aura = pallet_aura;

    #[runtime::pallet_index(3)]
    pub type Grandpa = pallet_grandpa;

    #[runtime::pallet_index(4)]
    pub type Balances = pallet_balances;

    #[runtime::pallet_index(5)]
    pub type TransactionPayment = pallet_transaction_payment;

    #[runtime::pallet_index(6)]
    pub type Sudo = pallet_sudo;

    #[cfg(feature = "incinerator-rehearsal")]
    #[runtime::pallet_index(20)]
    pub type Incinerator = crate::incinerator_rehearsal;

    // Include the custom logic from the pallet-template in the runtime.
    #[runtime::pallet_index(7)]
    pub type AiRouter = pallet_ai_router;
}

// The reward experiment has a separate macro expansion so the optional pallet
// cannot alter Runtime104 metadata, pallet indexes or Wasm when the feature is off.
#[cfg(feature = "gpu-pow-rewards-dev")]
macro_rules! reward_runtime {
    ($($extra:tt)*) => {
    #[frame_support::runtime]
    mod runtime {
    #[runtime::runtime]
    #[runtime::derive(
        RuntimeCall,
        RuntimeEvent,
        RuntimeError,
        RuntimeOrigin,
        RuntimeFreezeReason,
        RuntimeHoldReason,
        RuntimeSlashReason,
        RuntimeLockId,
        RuntimeTask,
        RuntimeViewFunction
    )]
    pub struct Runtime;

    #[runtime::pallet_index(0)]
    pub type System = frame_system;

    #[runtime::pallet_index(1)]
    pub type Timestamp = pallet_timestamp;

    #[runtime::pallet_index(2)]
    pub type Aura = pallet_aura;

    #[runtime::pallet_index(3)]
    pub type Grandpa = pallet_grandpa;

    #[runtime::pallet_index(4)]
    pub type Balances = pallet_balances;

    #[runtime::pallet_index(5)]
    pub type TransactionPayment = pallet_transaction_payment;

    #[runtime::pallet_index(7)]
    pub type AiRouter = pallet_ai_router;

    #[runtime::pallet_index(8)]
    pub type PowRewards = pallet_pow_rewards;

    $($extra)*
    }
    };
}

#[cfg(all(feature = "gpu-pow-rewards-dev", not(feature = "native-dead-mainnet-drill")))]
reward_runtime!();
#[cfg(feature = "native-dead-mainnet-drill")]
reward_runtime!(
    #[runtime::pallet_index(20)]
    pub type Incinerator = crate::native_incinerator_rehearsal::pallet;
);
