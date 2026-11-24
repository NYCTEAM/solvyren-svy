//! Run from the candidate artifacts directory. Default mode only verifies public inputs.
#[path = "../src/gpu_pow.rs"]
mod gpu_pow;
#[path = "../src/svy_address.rs"]
mod svy_address;
const EXPERIMENTAL_TARGET: [u8; 32] = {
    let mut a = [255; 32];
    a[0] = 63;
    a
};
use eagle_runtime::{
    opaque::Block, AccountId, Hash, Header, PowBeneficiaryV1, RuntimeCall, TimestampCall,
    UncheckedExtrinsic, POW_ENGINE_ID, VERSION, WASM_BINARY,
};
use sc_executor::{
    HeapAllocStrategy, WasmExecutionMethod, WasmExecutor, WasmtimeInstantiationStrategy,
};
use sp_core::traits::{CallContext, CodeExecutor, RuntimeCode, WrappedRuntimeCode};

use sp_runtime::{
    codec::{Decode, DecodeAll, Encode},
    generic::{Digest, DigestItem, SignedBlock},
    traits::{BlakeTwo256, Block as BlockT, Hash as HashT, Header as HeaderT},
    OpaqueExtrinsic, StateVersion,
};
use std::{
    fs::{self, OpenOptions},
    io::Write,
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
    let code = WASM_BINARY.expect("RC3 embedded Wasm");
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
        return Err("must execute RC3 Wasm".into());
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

fn finalize_valid(
    executor: &WasmExecutor<sp_io::SubstrateHostFunctions>,
    parent_hash: Hash,
    digest: Digest,
    millis: u64,
) -> Result<(Header, OpaqueExtrinsic), String> {
    let (typed, opaque) = timestamp_extrinsic(millis);
    let root = BlakeTwo256::ordered_trie_root(vec![typed.encode()], StateVersion::V1);
    let header = Header::new(1, Default::default(), root, parent_hash, digest);
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

fn raw_storage(document: &serde_json::Value) -> sp_core::storage::Storage {
    assert_eq!(
        document["genesis"]["raw"]["childrenDefault"],
        serde_json::json!({})
    );
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

const PLAN: &str = "V5-RC3-MAINNET-BLOCK1-PLAN.json";
const PLAN_SHA: &str = "3d7883daf4de48db9d0f2f730df41456c1f93c9b936d4f0162df6156fcaa980d";
const SPEC_SHA: &str = "669568cfc22ef92da64eae6ae15b3116107f9ee58b751cd1cb886ec98291cc16";
const WASM_SHA: &str = "703755cf454479eae6aabc0df4f8cb73235f4d1e6814ac30cdd4581a48afa7ad";
const GENESIS: &str = "0x678107bc4d21a4e7f9b12ee817af0c93f074cf04fbd7ee716976af42ca92f4c8";
const TEAM_KEY: &str = "0x0a098b3f2e2cc6263aa9e844ae589cbb76b8b494d48e0bcc3f0ed0d74f50a40c";

fn pinned(path: &str, expected: &str) -> Vec<u8> {
    let bytes = fs::read(path).expect(path);
    assert_eq!(
        sp_core::hashing::sha2_256(&bytes).as_slice(),
        decode_hex(&format!("0x{expected}")),
        "SHA-256 mismatch: {path}"
    );
    bytes
}

// Called exclusively by author-one. Inspect the selected, connected local node immediately
// before work and before publishing the offline import file. Never submits a block via RPC.
fn check_best(port: u16) {
    for (method, params, expected) in [
        ("chain_getBlockHash", serde_json::json!([0]), GENESIS),
        ("chain_getBlockHash", serde_json::json!([]), GENESIS),
    ] {
        let req = serde_json::json!({"jsonrpc":"2.0","id":1,"method":method,"params":params});
        let resp = std::process::Command::new("curl")
            .args([
                "-q",
                "--fail",
                "--silent",
                "--show-error",
                "--noproxy",
                "*",
                "--max-time",
                "4",
                "-H",
                "Content-Type: application/json",
                "--data-binary",
                &req.to_string(),
                &format!("http://127.0.0.1:{port}/"),
            ])
            .output()
            .expect("local RPC");
        assert!(resp.status.success());
        let value: serde_json::Value = serde_json::from_slice(&resp.stdout).expect("RPC JSON");
        assert_eq!(value["result"], expected, "genesis/best mismatch");
    }
}

fn main() {
    let args: Vec<_> = std::env::args().skip(1).collect();
    let port = match args.as_slice() {
        [] => None,
        [mode] if mode == "check" => None,
        [mode, port] if mode == "author-one" => Some(port.parse::<u16>().expect("local RPC port")),
        _ => panic!("expected check or author-one LOCAL_RPC_PORT"),
    };
    let plan: serde_json::Value = serde_json::from_slice(&pinned(PLAN, PLAN_SHA)).unwrap();
    assert_eq!(plan["blocks"], serde_json::json!([[]]));
    assert_eq!(plan["height"], 1);
    assert_eq!(plan["parentHash"], GENESIS);
    assert_eq!(plan["genesisHash"], GENESIS);
    assert_eq!(plan["authoringDevice"], "local-cpu");
    assert_eq!(plan["rawSpecSHA256"], SPEC_SHA);
    let raw: serde_json::Value =
        serde_json::from_slice(&pinned(plan["rawSpec"].as_str().unwrap(), SPEC_SHA)).unwrap();
    assert_eq!(raw["id"], "solvyren-svy");
    assert_eq!(
        raw["properties"]["workAlgorithm"],
        "integer-matrix-2x2048x16"
    );
    let wasm = pinned(
        "solvyren-svy-v5-rc3/runtime/solvyren-svy-transfer-fee.wasm",
        WASM_SHA,
    );
    assert_eq!(VERSION.spec_version, 6);
    assert!(
        WASM_BINARY == Some(wasm.as_slice()),
        "embedded runtime differs"
    );
    let mut storage = raw_storage(&raw);
    assert!(
        storage.top.remove(b":code".as_slice()).as_deref() == Some(wasm.as_slice()),
        "genesis runtime differs"
    );
    let address = plan["beneficiaryAddress"].as_str().unwrap();
    let beneficiary = svy_address::decode(address).expect("canonical team address");
    let key: &[u8] = beneficiary.as_ref();
    assert_eq!(key, decode_hex(TEAM_KEY));
    assert_eq!(plan["beneficiaryPublicKey"], TEAM_KEY);
    assert_eq!(plan["pool"], serde_json::json!(key));
    let mut account_key = sp_core::twox_128(b"System").to_vec();
    account_key.extend(sp_core::twox_128(b"Account"));
    account_key.extend(sp_core::blake2_128(key));
    account_key.extend(key);
    let account = &storage.top[&account_key];
    assert_eq!(account.len(), 80);
    assert_eq!(
        u128::from_le_bytes(account[16..32].try_into().unwrap()),
        10_000_000 * 10u128.pow(12)
    );
    let ext = sp_io::TestExternalities::new_with_code_and_state(&wasm, storage, StateVersion::V1);
    let (snapshot, state_root) = ext.into_raw_snapshot();
    let genesis = Header::new(
        0,
        BlakeTwo256::ordered_trie_root(vec![], StateVersion::V1),
        state_root,
        Default::default(),
        Default::default(),
    )
    .hash();
    assert_eq!(format!("{genesis:?}"), GENESIS);
    println!("Verified RC3 raw spec {SPEC_SHA}, Wasm {WASM_SHA}, parent {genesis:?}, team {address} ({TEAM_KEY}), genesis balance 10000000 SVY, CPU height 1 only.");
    let Some(port) = port else { return };
    check_best(port);
    // A permanent exclusive marker prevents accidental repeat authoring in this workspace.
    let mut marker = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open("RC3-FIRST-BLOCK.started")
        .expect("existing attempt or unwritable directory");
    writeln!(marker, "{genesis:?}").unwrap();
    marker.sync_all().unwrap();
    let mut ext =
        sp_io::TestExternalities::from_raw_snapshot(snapshot, state_root, StateVersion::V1);
    let millis = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_millis()
        .try_into()
        .unwrap();
    let (header, timestamp) = ext
        .execute_with(|| {
            finalize_valid(
                &executor(),
                genesis,
                Digest {
                    logs: vec![beneficiary_account(beneficiary)],
                },
                millis,
            )
        })
        .expect("build #1");
    assert_eq!(*header.number(), 1);
    assert_eq!(*header.parent_hash(), genesis);
    let sealed = cpu_seal(genesis, header);
    check_best(port);
    let block = SignedBlock {
        block: Block::new(sealed, vec![timestamp]),
        justifications: None,
    };
    let bytes = serde_json::to_vec(&block).unwrap();
    let mut out = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open("RC3-FIRST-BLOCK.jsonl")
        .expect("exclusive output");
    out.write_all(&bytes).unwrap();
    out.write_all(b"\n").unwrap();
    out.sync_all().unwrap();
}
