//! Explicit CPU fixture emitter for the isolated reward import gate.
//!
//! This module never opens a node database or GPU context. Its output is
//! deliberately labelled CPU work and must only be imported into fresh,
//! disposable databases that use the exact same reward runtime/spec.

use crate::{gpu_pow, gpu_pow_consensus::EXPERIMENTAL_TARGET};
use eagle_runtime::{
    opaque::Block, AccountId, Address, Balance, Hash, Header, PowBeneficiaryV1, RuntimeCall,
    Signature, SignedPayload, TimestampCall, TxExtension, UncheckedExtrinsic, POW_ENGINE_ID,
    VERSION, WASM_BINARY,
};
use sc_executor::{
    HeapAllocStrategy, WasmExecutionMethod, WasmExecutor, WasmtimeInstantiationStrategy,
};
use sp_core::traits::{CallContext, CodeExecutor, RuntimeCode, WrappedRuntimeCode};
use sp_core::Pair as _;
use sp_runtime::{
    codec::{Decode, DecodeAll, Encode},
    generic::{Digest, DigestItem, SignedBlock},
    traits::{BlakeTwo256, Block as BlockT, Hash as HashT, Header as HeaderT},
    OpaqueExtrinsic, StateVersion,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
    path::PathBuf,
};

fn executor() -> WasmExecutor<sp_io::SubstrateHostFunctions> {
    WasmExecutor::<sp_io::SubstrateHostFunctions>::builder()
        .with_execution_method(WasmExecutionMethod::Compiled {
            instantiation_strategy: WasmtimeInstantiationStrategy::RecreateInstance,
        })
        .with_max_runtime_instances(1)
        .with_runtime_cache_size(2)
        .with_onchain_heap_alloc_strategy(HeapAllocStrategy::Static { extra_pages: 1024 })
        .build()
}

fn call_wasm(
    executor: &WasmExecutor<sp_io::SubstrateHostFunctions>,
    method: &str,
    input: &[u8],
) -> Result<Vec<u8>, String> {
    let code = WASM_BINARY.expect("reward runtime Wasm must be embedded");
    let fetch = WrappedRuntimeCode(std::borrow::Cow::Borrowed(code));
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
    .ok_or("externalities are not installed")?;
    if native {
        return Err("fixture must execute embedded Wasm".into());
    }
    result.map_err(|error| format!("{method}: {error:?}"))
}

fn beneficiary_account(account: AccountId) -> DigestItem {
    DigestItem::PreRuntime(
        POW_ENGINE_ID,
        PowBeneficiaryV1::new(account).encode_payload(),
    )
}

fn timestamp_extrinsic(millis: u64) -> (UncheckedExtrinsic, OpaqueExtrinsic) {
    let typed =
        UncheckedExtrinsic::new_bare(RuntimeCall::Timestamp(TimestampCall::set { now: millis }));
    let encoded = typed.encode();
    let opaque = OpaqueExtrinsic::decode(&mut &encoded[..]).expect("opaque timestamp extrinsic");
    (typed, opaque)
}

fn signed_call(
    sender: sp_core::sr25519::Pair,
    call: RuntimeCall,
    genesis_hash: Hash,
) -> (UncheckedExtrinsic, OpaqueExtrinsic) {
    let tx_ext: TxExtension = (
        frame_system::AuthorizeCall::<eagle_runtime::Runtime>::new(),
        frame_system::CheckNonZeroSender::<eagle_runtime::Runtime>::new(),
        frame_system::CheckSpecVersion::<eagle_runtime::Runtime>::new(),
        frame_system::CheckTxVersion::<eagle_runtime::Runtime>::new(),
        frame_system::CheckGenesis::<eagle_runtime::Runtime>::new(),
        frame_system::CheckEra::<eagle_runtime::Runtime>::from(sp_runtime::generic::Era::Immortal),
        frame_system::CheckNonce::<eagle_runtime::Runtime>::from(0),
        frame_system::CheckWeight::<eagle_runtime::Runtime>::new(),
        eagle_runtime::PaymentExtension::from(0),
        frame_metadata_hash_extension::CheckMetadataHash::<eagle_runtime::Runtime>::new(false),
        frame_system::WeightReclaim::<eagle_runtime::Runtime>::new(),
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
    let typed = UncheckedExtrinsic::new_signed(
        call,
        Address::Id(AccountId::from(sender.public())),
        Signature::Sr25519(signature),
        tx_ext,
    );
    let encoded = typed.encode();
    let opaque = OpaqueExtrinsic::decode(&mut &encoded[..]).expect("opaque signed extrinsic");
    (typed, opaque)
}

fn signed_burn(
    sender: sp_core::sr25519::Pair,
    value: Balance,
    genesis_hash: Hash,
) -> (UncheckedExtrinsic, OpaqueExtrinsic) {
    signed_call(
        sender,
        RuntimeCall::Balances(pallet_balances::Call::burn {
            value,
            keep_alive: true,
        }),
        genesis_hash,
    )
}

fn signed_remark(
    sender: sp_core::sr25519::Pair,
    genesis_hash: Hash,
) -> (UncheckedExtrinsic, OpaqueExtrinsic) {
    signed_call(
        sender,
        RuntimeCall::System(frame_system::Call::remark {
            remark: b"eagw-fee-only-fixture-v1".to_vec(),
        }),
        genesis_hash,
    )
}

fn signed_ai_router_report(
    sender: sp_core::sr25519::Pair,
    genesis_hash: Hash,
) -> (UncheckedExtrinsic, OpaqueExtrinsic) {
    // Runtime pallet index 7, AiRouter call index 0, followed by the SCALE
    // fields of report_hardware. Keep the node test independent of the pallet
    // crate while still decoding through the exact RuntimeCall type.
    let mut encoded_call = vec![7, 0];
    encoded_call.extend(24u64.encode());
    encoded_call.extend(12u64.encode());
    encoded_call.extend(true.encode());
    let call = RuntimeCall::decode_all(&mut &encoded_call[..])
        .expect("decode exact AiRouter report_hardware call");
    signed_call(sender, call, genesis_hash)
}

fn ai_router_node_count_raw() -> Option<Vec<u8>> {
    storage_value_raw(b"AiRouter", b"NodeCount")
}

fn storage_value_raw(pallet: &[u8], item: &[u8]) -> Option<Vec<u8>> {
    let mut key = sp_core::twox_128(pallet).to_vec();
    key.extend(sp_core::twox_128(item));
    sp_io::storage::get(&key).map(|value| value.to_vec())
}

fn finalize_valid(
    executor: &WasmExecutor<sp_io::SubstrateHostFunctions>,
    number: u32,
    parent_hash: Hash,
    digest: Digest,
    millis: u64,
) -> Result<(Header, OpaqueExtrinsic), String> {
    let (typed, opaque) = timestamp_extrinsic(millis);
    let root = BlakeTwo256::ordered_trie_root(vec![typed.encode()], StateVersion::V1);
    let header = Header::new(number, Default::default(), root, parent_hash, digest);
    let initialized = call_wasm(executor, "Core_initialize_block", &header.encode())?;
    let _: sp_runtime::ExtrinsicInclusionMode =
        DecodeAll::decode_all(&mut &initialized[..]).map_err(|error| format!("{error:?}"))?;
    let applied = call_wasm(executor, "BlockBuilder_apply_extrinsic", &typed.encode())?;
    let outcome: sp_runtime::ApplyExtrinsicResult =
        DecodeAll::decode_all(&mut &applied[..]).map_err(|error| format!("{error:?}"))?;
    if !matches!(outcome, Ok(Ok(()))) {
        return Err(format!("timestamp inherent failed: {outcome:?}"));
    }
    let finalized = call_wasm(executor, "BlockBuilder_finalize_block", &[])?;
    let header = Header::decode_all(&mut &finalized[..]).map_err(|error| format!("{error:?}"))?;
    Ok((header, opaque))
}

fn build_filtered_burn_body(
    executor: &WasmExecutor<sp_io::SubstrateHostFunctions>,
    parent: &Header,
    genesis_hash: Hash,
    beneficiary: AccountId,
    sender: sp_core::sr25519::Pair,
    millis: u64,
) -> (Header, Vec<OpaqueExtrinsic>) {
    let issuance_before = pallet_balances::Pallet::<eagle_runtime::Runtime>::total_issuance();
    let sender_account = AccountId::from(sender.public());
    let balance_before =
        pallet_balances::Pallet::<eagle_runtime::Runtime>::free_balance(&sender_account);
    let nonce_before =
        frame_system::Pallet::<eagle_runtime::Runtime>::account_nonce(&sender_account);
    let (timestamp_typed, timestamp_opaque) = timestamp_extrinsic(millis);
    let (burn_typed, burn_opaque) = signed_burn(sender, 1, genesis_hash);
    let encoded = vec![timestamp_typed.encode(), burn_typed.encode()];
    let root = BlakeTwo256::ordered_trie_root(encoded.clone(), StateVersion::V1);
    let header = Header::new(
        *parent.number() + 1,
        *parent.state_root(),
        root,
        parent.hash(),
        Digest {
            logs: vec![beneficiary_account(beneficiary)],
        },
    );
    let initialized = call_wasm(executor, "Core_initialize_block", &header.encode())
        .expect("burn fixture initializes");
    let _: sp_runtime::ExtrinsicInclusionMode =
        DecodeAll::decode_all(&mut &initialized[..]).expect("inclusion mode");
    let timestamp_applied = call_wasm(
        executor,
        "BlockBuilder_apply_extrinsic",
        &timestamp_typed.encode(),
    )
    .expect("burn fixture applies timestamp");
    let timestamp_outcome: sp_runtime::ApplyExtrinsicResult =
        DecodeAll::decode_all(&mut &timestamp_applied[..]).expect("timestamp apply result");
    assert!(matches!(timestamp_outcome, Ok(Ok(()))));
    let burn_applied = call_wasm(
        executor,
        "BlockBuilder_apply_extrinsic",
        &burn_typed.encode(),
    )
    .expect("burn fixture applies signed extrinsic");
    let burn_outcome: sp_runtime::ApplyExtrinsicResult =
        DecodeAll::decode_all(&mut &burn_applied[..]).expect("burn apply result");
    let expected_filtered: sp_runtime::DispatchError =
        frame_system::Error::<eagle_runtime::Runtime>::CallFiltered.into();
    assert_eq!(
        burn_outcome,
        Ok(Err(expected_filtered)),
        "signed burn was not filtered"
    );
    let issuance_after = pallet_balances::Pallet::<eagle_runtime::Runtime>::total_issuance();
    let balance_after =
        pallet_balances::Pallet::<eagle_runtime::Runtime>::free_balance(&sender_account);
    let nonce_after =
        frame_system::Pallet::<eagle_runtime::Runtime>::account_nonce(&sender_account);
    assert!(
        issuance_after < issuance_before,
        "filtered burn did not charge a transaction fee"
    );
    assert!(
        balance_after < balance_before,
        "filtered burn did not reduce sender balance by its fee"
    );
    assert_eq!(
        issuance_before - issuance_after,
        balance_before - balance_after,
        "filtered burn changed issuance by more than its fee"
    );
    assert_eq!(nonce_after, nonce_before + 1, "burn did not consume nonce");
    let finalized = call_wasm(executor, "BlockBuilder_finalize_block", &[])
        .expect("filtered burn with tracked fee must finalize");
    let finalized =
        Header::decode_all(&mut &finalized[..]).expect("filtered-burn finalized header");
    let ledger = eagle_runtime::PowRewards::ledger().expect("filtered-burn reward ledger");
    assert!(
        ledger.destroyed_issued > 0
            && pallet_balances::Pallet::<eagle_runtime::Runtime>::total_issuance()
                + ledger.destroyed_issued
                == ledger.lifetime_issued,
        "filtered-burn fee was not reconciled to gross issuance"
    );
    (finalized, vec![timestamp_opaque, burn_opaque])
}

fn build_tracked_fee_body(
    executor: &WasmExecutor<sp_io::SubstrateHostFunctions>,
    parent: &Header,
    genesis_hash: Hash,
    beneficiary: AccountId,
    sender: sp_core::sr25519::Pair,
    millis: u64,
) -> (Header, Vec<OpaqueExtrinsic>) {
    let issuance_before = pallet_balances::Pallet::<eagle_runtime::Runtime>::total_issuance();
    let sender_account = AccountId::from(sender.public());
    let balance_before =
        pallet_balances::Pallet::<eagle_runtime::Runtime>::free_balance(&sender_account);
    let nonce_before =
        frame_system::Pallet::<eagle_runtime::Runtime>::account_nonce(&sender_account);
    let (timestamp_typed, timestamp_opaque) = timestamp_extrinsic(millis);
    let (remark_typed, remark_opaque) = signed_remark(sender, genesis_hash);
    let encoded = vec![timestamp_typed.encode(), remark_typed.encode()];
    let root = BlakeTwo256::ordered_trie_root(encoded.clone(), StateVersion::V1);
    let header = Header::new(
        *parent.number() + 1,
        *parent.state_root(),
        root,
        parent.hash(),
        Digest {
            logs: vec![beneficiary_account(beneficiary)],
        },
    );
    let initialized = call_wasm(executor, "Core_initialize_block", &header.encode())
        .expect("fee fixture initializes");
    let _: sp_runtime::ExtrinsicInclusionMode =
        DecodeAll::decode_all(&mut &initialized[..]).expect("inclusion mode");
    for (index, extrinsic) in encoded.iter().enumerate() {
        let applied = call_wasm(executor, "BlockBuilder_apply_extrinsic", extrinsic)
            .expect("fee fixture applies extrinsic");
        let outcome: sp_runtime::ApplyExtrinsicResult =
            DecodeAll::decode_all(&mut &applied[..]).expect("apply result");
        assert!(
            matches!(outcome, Ok(Ok(()))),
            "fee fixture extrinsic {index} failed: {outcome:?}"
        );
    }
    let issuance_after = pallet_balances::Pallet::<eagle_runtime::Runtime>::total_issuance();
    let balance_after =
        pallet_balances::Pallet::<eagle_runtime::Runtime>::free_balance(&sender_account);
    let nonce_after =
        frame_system::Pallet::<eagle_runtime::Runtime>::account_nonce(&sender_account);
    let charged = balance_before
        .checked_sub(balance_after)
        .expect("fee-only remark must not increase sender balance");
    assert!(charged > 0, "signed remark did not charge a fee");
    assert_eq!(
        issuance_before.checked_sub(issuance_after),
        Some(charged),
        "fee-only remark changed issuance by a different amount than sender balance"
    );
    assert_eq!(
        nonce_after,
        nonce_before + 1,
        "remark did not consume nonce"
    );
    let finalized = call_wasm(executor, "BlockBuilder_finalize_block", &[])
        .expect("tracked fee-only block must finalize");
    let finalized = Header::decode_all(&mut &finalized[..]).expect("fee-only finalized header");
    let ledger = eagle_runtime::PowRewards::ledger().expect("fee-only reward ledger");
    assert!(
        ledger.destroyed_issued > 0
            && pallet_balances::Pallet::<eagle_runtime::Runtime>::total_issuance()
                + ledger.destroyed_issued
                == ledger.lifetime_issued,
        "fee-only destruction was not reconciled to gross issuance"
    );
    (finalized, vec![timestamp_opaque, remark_opaque])
}

fn build_filtered_ai_router_body(
    executor: &WasmExecutor<sp_io::SubstrateHostFunctions>,
    parent: &Header,
    genesis_hash: Hash,
    beneficiary: AccountId,
    sender: sp_core::sr25519::Pair,
    millis: u64,
) -> (Header, Vec<OpaqueExtrinsic>) {
    let sender_account = AccountId::from(sender.public());
    let issuance_before = pallet_balances::Pallet::<eagle_runtime::Runtime>::total_issuance();
    let balance_before =
        pallet_balances::Pallet::<eagle_runtime::Runtime>::free_balance(&sender_account);
    let nonce_before =
        frame_system::Pallet::<eagle_runtime::Runtime>::account_nonce(&sender_account);
    let router_node_count_before = ai_router_node_count_raw();
    let (timestamp_typed, timestamp_opaque) = timestamp_extrinsic(millis);
    let (report_typed, report_opaque) = signed_ai_router_report(sender, genesis_hash);
    let encoded = vec![timestamp_typed.encode(), report_typed.encode()];
    let root = BlakeTwo256::ordered_trie_root(encoded.clone(), StateVersion::V1);
    let header = Header::new(
        *parent.number() + 1,
        *parent.state_root(),
        root,
        parent.hash(),
        Digest {
            logs: vec![beneficiary_account(beneficiary)],
        },
    );
    let initialized = call_wasm(executor, "Core_initialize_block", &header.encode())
        .expect("AiRouter-filter fixture initializes");
    let _: sp_runtime::ExtrinsicInclusionMode =
        DecodeAll::decode_all(&mut &initialized[..]).expect("inclusion mode");
    for (index, extrinsic) in encoded.iter().enumerate() {
        let applied = call_wasm(executor, "BlockBuilder_apply_extrinsic", extrinsic)
            .expect("AiRouter-filter fixture applies extrinsic");
        let outcome: sp_runtime::ApplyExtrinsicResult =
            DecodeAll::decode_all(&mut &applied[..]).expect("apply result");
        if index == 0 {
            assert!(
                matches!(outcome, Ok(Ok(()))),
                "timestamp failed: {outcome:?}"
            );
        } else {
            let expected: sp_runtime::DispatchError =
                frame_system::Error::<eagle_runtime::Runtime>::CallFiltered.into();
            assert_eq!(outcome, Ok(Err(expected)), "AiRouter call was not filtered");
        }
    }
    assert_eq!(
        ai_router_node_count_raw(),
        router_node_count_before,
        "filtered AiRouter call mutated node count"
    );
    assert_eq!(
        frame_system::Pallet::<eagle_runtime::Runtime>::account_nonce(&sender_account),
        nonce_before + 1,
        "filtered AiRouter call did not consume nonce"
    );
    let issuance_after = pallet_balances::Pallet::<eagle_runtime::Runtime>::total_issuance();
    let balance_after =
        pallet_balances::Pallet::<eagle_runtime::Runtime>::free_balance(&sender_account);
    assert!(
        issuance_after < issuance_before,
        "filtered call charged no fee"
    );
    assert_eq!(
        issuance_before - issuance_after,
        balance_before - balance_after,
        "filtered AiRouter call changed balances beyond its fee"
    );
    let finalized = call_wasm(executor, "BlockBuilder_finalize_block", &[])
        .expect("filtered AiRouter block with tracked fee must finalize");
    let finalized = Header::decode_all(&mut &finalized[..]).expect("filtered header");
    let ledger = eagle_runtime::PowRewards::ledger().expect("reward ledger");
    assert_eq!(
        pallet_balances::Pallet::<eagle_runtime::Runtime>::total_issuance()
            + ledger.destroyed_issued,
        ledger.lifetime_issued,
        "filtered AiRouter fee was not reconciled"
    );
    (finalized, vec![timestamp_opaque, report_opaque])
}

fn cpu_seal(genesis_hash: Hash, mut header: Header) -> Header {
    let challenge = gpu_pow::challenge_for_header(genesis_hash, &header).expect("unsealed header");
    let certificate = (0u64..1024)
        .map(|nonce| gpu_pow::compute(challenge, nonce))
        .find(|certificate| gpu_pow::ticket(certificate) <= EXPERIMENTAL_TARGET)
        .expect("difficulty-four work must be found within bounded CPU search");
    let encoded = certificate.encode_fixed();
    gpu_pow::verify(genesis_hash, &header, &encoded, EXPERIMENTAL_TARGET)
        .expect("emitted CPU seal must verify");
    header
        .digest_mut()
        .push(DigestItem::Seal(POW_ENGINE_ID, encoded));
    header
}

fn write_block(path: PathBuf, header: Header, body: Vec<OpaqueExtrinsic>) {
    let signed = SignedBlock {
        block: Block::new(header, body),
        justifications: None,
    };
    let bytes = serde_json::to_vec(&signed).expect("serialize SignedBlock");
    write_new_synced(path, &bytes);
}

fn write_new_synced(path: PathBuf, bytes: &[u8]) {
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)
        .expect("fixture file must not already exist");
    file.write_all(bytes).expect("write fixture file");
    file.sync_all().expect("sync fixture file");
}

fn decode_hex(value: &str) -> Vec<u8> {
    let value = value
        .strip_prefix("0x")
        .expect("hex value must start with 0x");
    assert_eq!(value.len() % 2, 0, "hex value must have even length");
    (0..value.len())
        .step_by(2)
        .map(|index| u8::from_str_radix(&value[index..index + 2], 16).expect("valid hex"))
        .collect()
}

fn exact_raw_spec_storage() -> sp_core::storage::Storage {
    let path = PathBuf::from(
        std::env::var_os("EAGW_REWARD_RAW_SPEC").expect("EAGW_REWARD_RAW_SPEC is required"),
    );
    assert!(path.is_absolute(), "raw spec path must be absolute");
    let document: serde_json::Value =
        serde_json::from_slice(&fs::read(path).expect("read exact raw spec"))
            .expect("raw spec JSON");
    let top = document["genesis"]["raw"]["top"]
        .as_object()
        .expect("raw spec genesis.raw.top");
    let mut storage = sp_core::storage::Storage::default();
    for (key, value) in top {
        storage.top.insert(
            decode_hex(key),
            decode_hex(value.as_str().expect("raw storage value is hex")),
        );
    }
    storage
}

/// Generate exact-current-spec fixtures only when an explicit unused output
/// directory is supplied. Work is CPU reference work, never GPU evidence.
#[test]
#[ignore = "requires EAGW_REWARD_FIXTURE_DIR; emits files but imports nothing"]
fn emit_current_reward_import_fixtures() {
    let output = PathBuf::from(
        std::env::var_os("EAGW_REWARD_FIXTURE_DIR").expect("EAGW_REWARD_FIXTURE_DIR is required"),
    );
    assert!(output.is_absolute(), "fixture directory must be absolute");
    fs::create_dir(&output).expect("fixture directory must not exist");

    let mut storage = exact_raw_spec_storage();
    // TestExternalities' constructors always insert the separately supplied
    // runtime code. Remove it from `top` first so it is reinserted unchanged,
    // rather than silently overwritten with an empty byte string.
    let code = storage
        .top
        .remove(b":code".as_slice())
        .expect("raw spec must contain :code");
    let ext = sp_io::TestExternalities::new_with_code_and_state(&code, storage, StateVersion::V1);
    let (raw, genesis_root) = ext.into_raw_snapshot();
    let genesis_export = PathBuf::from(
        std::env::var_os("EAGW_REWARD_GENESIS_EXPORT")
            .expect("EAGW_REWARD_GENESIS_EXPORT is required"),
    );
    let exported: SignedBlock<Block> =
        serde_json::from_slice(&fs::read(genesis_export).expect("read exported genesis block"))
            .expect("standard genesis SignedBlock JSON");
    let genesis_header = exported.block.header().clone();
    assert_eq!(*genesis_header.number(), 0);
    assert_eq!(*genesis_header.state_root(), genesis_root);
    assert!(exported.block.extrinsics().is_empty());
    let genesis_hash = genesis_header.hash();
    let mut ext = sp_io::TestExternalities::from_raw_snapshot(raw, genesis_root, StateVersion::V1);
    let wasm = executor();
    let sender = sp_keyring::Sr25519Keyring::Alice.pair();
    let reward_account = AccountId::from(sender.public());

    ext.execute_with(|| {
        assert!(
            storage_value_raw(b"AiRouter", b"DevFundingPlan").is_none(),
            "exact raw spec must not seed development funding"
        );
        assert!(
            storage_value_raw(b"System", b"AuthorizedUpgrade").is_none(),
            "exact raw spec must not seed an authorized runtime upgrade"
        );
    });

    let (valid_header, valid_body) = ext.execute_with(|| {
        finalize_valid(
            &wasm,
            1,
            genesis_hash,
            Digest {
                logs: vec![beneficiary_account(reward_account.clone())],
            },
            1_793_491_210_000,
        )
        .expect("valid block must finalize")
    });
    let valid_header = cpu_seal(genesis_hash, valid_header);
    write_block(
        output.join("positive-block1.json"),
        valid_header.clone(),
        vec![valid_body.clone()],
    );
    ext.commit_all().expect("commit positive block-one state");

    let parent_snapshot = ext.into_raw_snapshot();
    let mut burn_ext = sp_io::TestExternalities::from_raw_snapshot(
        parent_snapshot.0.clone(),
        parent_snapshot.1,
        StateVersion::V1,
    );
    let (burn_header, burn_body) = burn_ext.execute_with(|| {
        build_filtered_burn_body(
            &wasm,
            &valid_header,
            genesis_hash,
            reward_account.clone(),
            sender,
            1_793_491_220_000,
        )
    });
    write_block(
        output.join("runtime-filtered-burn-block2.json"),
        cpu_seal(genesis_hash, burn_header),
        burn_body,
    );

    let mut fee_ext = sp_io::TestExternalities::from_raw_snapshot(
        parent_snapshot.0.clone(),
        parent_snapshot.1,
        StateVersion::V1,
    );
    let (fee_header, fee_body) = fee_ext.execute_with(|| {
        build_tracked_fee_body(
            &wasm,
            &valid_header,
            genesis_hash,
            reward_account.clone(),
            sp_keyring::Sr25519Keyring::Alice.pair(),
            1_793_491_220_000,
        )
    });
    write_block(
        output.join("runtime-signed-fee-block2.json"),
        cpu_seal(genesis_hash, fee_header),
        fee_body,
    );

    let mut router_ext = sp_io::TestExternalities::from_raw_snapshot(
        parent_snapshot.0.clone(),
        parent_snapshot.1,
        StateVersion::V1,
    );
    let (router_header, router_body) = router_ext.execute_with(|| {
        build_filtered_ai_router_body(
            &wasm,
            &valid_header,
            genesis_hash,
            reward_account.clone(),
            sp_keyring::Sr25519Keyring::Alice.pair(),
            1_793_491_220_000,
        )
    });
    write_block(
        output.join("runtime-filtered-ai-router-block2.json"),
        cpu_seal(genesis_hash, router_header),
        router_body,
    );

    let mut strict_cases = vec![
        ("missing", vec![]),
        (
            "duplicate",
            vec![
                beneficiary_account(reward_account.clone()),
                beneficiary_account(reward_account.clone()),
            ],
        ),
    ];
    let mut trailing = PowBeneficiaryV1::new(reward_account.clone()).encode_payload();
    trailing.push(0);
    strict_cases.push((
        "trailing-byte",
        vec![DigestItem::PreRuntime(POW_ENGINE_ID, trailing)],
    ));
    let mut wrong_version = PowBeneficiaryV1::new(reward_account.clone()).encode_payload();
    wrong_version[0] = PowBeneficiaryV1::VERSION + 1;
    strict_cases.push((
        "wrong-version",
        vec![DigestItem::PreRuntime(POW_ENGINE_ID, wrong_version)],
    ));
    for (name, logs) in strict_cases {
        let mut candidate = valid_header.clone();
        candidate.digest_mut().logs.pop().expect("PoW seal");
        candidate.digest_mut().logs = logs;
        let sealed = cpu_seal(genesis_hash, candidate);
        write_block(
            output.join(format!("strict-{name}-block1.json")),
            sealed,
            vec![valid_body.clone()],
        );
    }

    // This candidate has a structurally valid beneficiary and valid CPU PoW,
    // but its timestamp lies at the first unapproved second after month 24.
    // The copied state root is intentionally wrong; a useful import result must
    // report the runtime schedule rejection before any state-root comparison.
    let (typed, opaque) = timestamp_extrinsic(1_856_649_600_000);
    let root = BlakeTwo256::ordered_trie_root(vec![typed.encode()], StateVersion::V1);
    let runtime_header = Header::new(
        1,
        *valid_header.state_root(),
        root,
        genesis_hash,
        Digest {
            logs: vec![beneficiary_account(reward_account.clone())],
        },
    );
    write_block(
        output.join("runtime-after-month24-block1.json"),
        cpu_seal(genesis_hash, runtime_header),
        vec![opaque],
    );

    // The second Runtime-attribution fixture is one second before the approved
    // emission start and remains close enough to the development epoch that the
    // host inherent checker does not mask the Runtime schedule rejection.
    let (typed, opaque) = timestamp_extrinsic(1_793_491_199_000);
    let root = BlakeTwo256::ordered_trie_root(vec![typed.encode()], StateVersion::V1);
    let runtime_header = Header::new(
        1,
        *valid_header.state_root(),
        root,
        genesis_hash,
        Digest {
            logs: vec![beneficiary_account(reward_account.clone())],
        },
    );
    write_block(
        output.join("runtime-before-start-block1.json"),
        cpu_seal(genesis_hash, runtime_header),
        vec![opaque],
    );

    let manifest = serde_json::json!({
        "schema": "EAGW-REWARD-IMPORT-FIXTURES-V1",
        "workKind": "bounded CPU reference work; not GPU evidence",
        "genesisStateRoot": format!("{genesis_root:#x}"),
        "genesisHash": format!("{genesis_hash:#x}"),
        "positiveHash": format!("{:#x}", valid_header.hash()),
        "genesisDevFundingPlanAbsent": true,
        "genesisAuthorizedUpgradeAbsent": true,
        "files": [
            "positive-block1.json",
            "strict-missing-block1.json",
            "strict-duplicate-block1.json",
            "strict-trailing-byte-block1.json",
            "strict-wrong-version-block1.json",
            "runtime-after-month24-block1.json",
            "runtime-before-start-block1.json",
            "runtime-filtered-burn-block2.json",
            "runtime-filtered-ai-router-block2.json",
            "runtime-signed-fee-block2.json"
        ]
    });
    write_new_synced(
        output.join("manifest.json"),
        &serde_json::to_vec_pretty(&manifest).unwrap(),
    );
    write_new_synced(output.join("COMPLETE"), b"complete\n");
}
