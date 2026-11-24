//! T5 real-path import evidence (test-only, CPU MX10 only, no GPU).
//! Blocks are built by executing the exact raw chain-spec Wasm and imported by
//! the release binary: `eagle-node import-blocks` -> service::new_partial ->
//! BasicQueue(StrictPowVerifier(DynamicDifficultyAlgorithm),
//! PowBlockImport(DynamicDifficultyAlgorithm, sp_timestamp from_system_time))
//! -> pallet-timestamp check_inherents -> DynamicDifficultyAlgorithm::difficulty
//! -> DynamicDifficultyAlgorithm::verify -> WorkAlgorithm::Mx10N16.
use super::{target_for_difficulty, EXPERIMENTAL_DIFFICULTY, EXPERIMENTAL_TARGET};
use crate::gpu_pow_mx10 as mx;
use eagle_runtime::{opaque::Block, AccountId, Header, PowBeneficiaryV1, RuntimeCall, TimestampCall, UncheckedExtrinsic, POW_ENGINE_ID};
use sc_executor::{WasmExecutionMethod, WasmExecutor, WasmtimeInstantiationStrategy};
use sp_core::traits::{CallContext, CodeExecutor, RuntimeCode, WrappedRuntimeCode};
use sp_core::H256;
use sp_runtime::{codec::{Decode, DecodeAll, Encode}, generic::{Digest, DigestItem, SignedBlock}, traits::{BlakeTwo256, Block as _, Hash as _, Header as _}, OpaqueExtrinsic, StateVersion};
use std::{fs, path::PathBuf, process::Command, time::{Instant, SystemTime, UNIX_EPOCH}};

const BIN: &str = "/mnt/f/POAH/eagle-ai-network/target/release/eagle-node";
const CHAIN: &str = "pow-mx10-n16-local";
const MX10_GENESIS: &str = "0x6172a0f0094cba19bfafe0b389dac397189903a6f816deb21378c3dba5200100";
type Exec = WasmExecutor<sp_io::SubstrateHostFunctions>;

fn dir() -> PathBuf { PathBuf::from(std::env::var("MX10_T5_OUT").expect("explicit fresh evidence directory")) }
fn shape() -> mx::EvalShape { mx::EvalShape::new(16, 2048).unwrap() }
fn beneficiary() -> Vec<u8> { PowBeneficiaryV1::new(AccountId::new([11; 32])).encode_payload() }
fn unhex(s: &str) -> Vec<u8> { let s = s.trim_start_matches("0x"); (0..s.len()).step_by(2).map(|i| u8::from_str_radix(&s[i..i + 2], 16).unwrap()).collect() }
fn hexs(b: &[u8]) -> String { b.iter().map(|x| format!("{x:02x}")).collect() }
fn now_ms() -> u64 { SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_millis() as u64 }
fn target(d: u128) -> [u8; 32] { target_for_difficulty(d).unwrap() }

fn call(w: &Exec, code: &[u8], method: &str, input: &[u8]) -> Vec<u8> {
    let fetch = WrappedRuntimeCode(std::borrow::Cow::Borrowed(code));
    let c = RuntimeCode { code_fetcher: &fetch, heap_pages: None, hash: sp_core::blake2_256(code).to_vec() };
    let (result, native) = sp_externalities::with_externalities(|ext| w.call(ext, &c, method, input, CallContext::Onchain)).unwrap();
    assert!(!native);
    result.unwrap()
}

fn timestamp_xt(millis: u64) -> (Vec<u8>, OpaqueExtrinsic) {
    let xt = UncheckedExtrinsic::new_bare(RuntimeCall::Timestamp(TimestampCall::set { now: millis }));
    let encoded = xt.encode();
    let opaque = OpaqueExtrinsic::decode(&mut &encoded[..]).unwrap();
    (encoded, opaque)
}

/// Executes one block in the runtime. With `keep == false` all state changes
/// are rolled back (used for fork siblings built on the same parent state).
fn build(ext: &mut sp_io::TestExternalities, w: &Exec, code: &[u8], parent: H256, number: u32, millis: u64, keep: bool) -> (Header, OpaqueExtrinsic) {
    ext.execute_with(|| {
        if !keep { sp_io::storage::start_transaction(); }
        let (encoded, opaque) = timestamp_xt(millis);
        let root = BlakeTwo256::ordered_trie_root(vec![encoded.clone()], StateVersion::V1);
        let h = Header::new(number, Default::default(), root, parent, Digest { logs: vec![DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary())] });
        let init = call(w, code, "Core_initialize_block", &h.encode());
        let _: sp_runtime::ExtrinsicInclusionMode = DecodeAll::decode_all(&mut &init[..]).unwrap();
        let applied = call(w, code, "BlockBuilder_apply_extrinsic", &encoded);
        let result: sp_runtime::ApplyExtrinsicResult = DecodeAll::decode_all(&mut &applied[..]).unwrap();
        assert!(matches!(result, Ok(Ok(()))), "runtime refused timestamp {millis}");
        let fin = call(w, code, "BlockBuilder_finalize_block", &[]);
        let header = Header::decode_all(&mut &fin[..]).unwrap();
        if !keep { sp_io::storage::rollback_transaction(); }
        (header, opaque)
    })
}

/// The runtime refuses to *build* a block whose timestamp violates
/// MinimumPeriod, so such import-negative blocks reuse a validly built header
/// and only swap the timestamp extrinsic (extrinsics root recomputed). They are
/// expected to be rejected by pallet-timestamp check_inherents, which runs
/// before execution on the PowBlockImport path.
fn retimestamp(valid: &Header, millis: u64) -> (Header, OpaqueExtrinsic) {
    let (encoded, opaque) = timestamp_xt(millis);
    let mut h = valid.clone();
    h.set_extrinsics_root(BlakeTwo256::ordered_trie_root(vec![encoded], StateVersion::V1));
    (h, opaque)
}

fn cert_in(g: H256, pre: H256, above: Option<[u8; 32]>, at_most: [u8; 32]) -> mx::Certificate {
    let ch = mx::challenge_from_pre_hash(g, pre);
    for nonce in 0..200_000u64 {
        let c = mx::compute(shape(), ch, nonce);
        let t = mx::ticket(&c);
        if t <= at_most && above.map_or(true, |lo| t > lo) { return c; }
    }
    panic!("nonce budget exhausted")
}

fn sealed(h: &Header, body: &OpaqueExtrinsic, c: &mx::Certificate) -> (Vec<u8>, H256) {
    let mut sh = h.clone();
    sh.digest_mut().push(DigestItem::Seal(POW_ENGINE_ID, c.encode_fixed()));
    let hash = sh.hash();
    let mut line = serde_json::to_vec(&SignedBlock { block: Block::new(sh, vec![body.clone()]), justifications: None }).unwrap();
    line.push(b'\n');
    (line, hash)
}

fn cli(label: &str, args: &[&str]) -> (Vec<u8>, String, Option<i32>) {
    let t = Instant::now();
    let started_ms = now_ms();
    let o = Command::new(BIN).args(args).env_remove("EAGW_POW_FINALITY_ONLY").env("MX10_T3_TRACE", "1").output().unwrap();
    let ns = t.elapsed().as_nanos();
    let err = String::from_utf8_lossy(&o.stderr).into_owned();
    let d = dir();
    fs::write(d.join(format!("logs/{label}.log")), &err).unwrap();
    fs::write(d.join(format!("logs/{label}.run.json")), serde_json::to_vec_pretty(&serde_json::json!({"args": args, "exit": o.status.code(), "wall_ns": ns, "started_unix_ms": started_ms})).unwrap()).unwrap();
    (o.stdout, err, o.status.code())
}

fn key_lines(log: &str) -> Vec<String> {
    let keys = ["Imported", "rror", "TooEarly", "TooFarInFuture", "nvalid", "nherent", "Too", "minimum period", "too far in the future"];
    log.lines().filter(|l| keys.iter().any(|k| l.contains(k))).take(24).map(|l| l.chars().take(400).collect()).collect()
}

/// Independent restatement of the T5 rule for expectations (not the node code).
fn expected_next(old: u128, elapsed: u64) -> u128 { (old * 600_000 / u128::from(elapsed.clamp(300_000, 1_200_000))).max(1) }

#[test]
#[ignore = "explicit T5 release import evidence only; CPU MX10 only; no GPU"]
fn t5_real_import_dda_continuous_window_and_timestamp_rules() {
    let d = dir();
    fs::create_dir_all(d.join("logs")).unwrap();
    fs::create_dir_all(d.join("blocks")).unwrap();
    let base = d.join("mx10-db").to_string_lossy().into_owned();
    assert!(!PathBuf::from(&base).exists(), "fresh DB required");
    let (spec, _, code_exit) = cli("mx10-build-spec", &["build-spec", "--chain", CHAIN, "--raw"]);
    assert_eq!(code_exit, Some(0));
    fs::write(d.join("mx10-chain-spec-raw.json"), &spec).unwrap();
    let gexport = d.join("mx10-genesis.json").to_string_lossy().into_owned();
    let (_, _, gexit) = cli("mx10-genesis", &["export-blocks", "--chain", CHAIN, "--base-path", &base, "--from", "0", "--to", "0", &gexport]);
    assert_eq!(gexit, Some(0));
    let genesis: SignedBlock<Block> = serde_json::from_slice(&fs::read(&gexport).unwrap()).unwrap();
    let g = genesis.block.header().hash();
    assert_eq!(format!("{g:?}"), MX10_GENESIS, "MX10 isolated chain identity must be unchanged");

    let doc: serde_json::Value = serde_json::from_slice(&spec).unwrap();
    let mut storage = sp_core::storage::Storage::default();
    for (k, v) in doc["genesis"]["raw"]["top"].as_object().unwrap() { storage.top.insert(unhex(k), unhex(v.as_str().unwrap())); }
    let mut now_key = sp_core::hashing::twox_128(b"Timestamp").to_vec();
    now_key.extend_from_slice(&sp_core::hashing::twox_128(b"Now"));
    let genesis_now_present = storage.top.contains_key(&now_key);
    assert_eq!(doc["properties"]["workAlgorithm"].as_str(), Some("25cdf8f5acd57e409d8324c71c15b271314e4effd3ef2ca75ff2c897d2275d17"));
    let code = storage.top.remove(b":code".as_slice()).unwrap();
    let ext0 = sp_io::TestExternalities::new_with_code_and_state(&code, storage, StateVersion::V1);
    let (raw, root) = ext0.into_raw_snapshot();
    let mut ext = sp_io::TestExternalities::from_raw_snapshot(raw, root, StateVersion::V1);
    assert_eq!(root, *genesis.block.header().state_root());
    let w = Exec::builder().with_execution_method(WasmExecutionMethod::Compiled { instantiation_strategy: WasmtimeInstantiationStrategy::RecreateInstance }).with_max_runtime_instances(1).with_runtime_cache_size(2).build();

    // Timestamps: past, 10_000 ms apart, with a 300_000 ms stall on the 60 -> 61 seam.
    let genesis_ts: u64 = 0; // pallet_timestamp::Now is absent in genesis storage
    assert!(!genesis_now_present, "genesis timestamp is expected to be unset (TimestampApi = 0)");
    let t0 = (now_ms() - 3 * 3600 * 1000) / 1000 * 1000;
    let mut ts = vec![genesis_ts];
    for i in 1..=121u32 { let prev = if i == 1 { t0 } else { ts[i as usize - 1] }; ts.push(prev + if i == 61 { 300_000 } else { 10_000 }); }
    let d_first = EXPERIMENTAL_DIFFICULTY;
    // T5-02: the first window starts at genesis whose timestamp is 0, so the
    // node must skip that retarget and keep the parent difficulty (no halving).
    assert_eq!(ts[0], 0);
    let d61 = d_first;
    // Second window starts at block 60 (non-zero timestamp) and is retargeted,
    // seam 60 -> 61 included: elapsed = ts(120) - ts(60).
    let d121 = expected_next(d61, ts[120] - ts[60]);
    assert_eq!(ts[120] - ts[60], 890_000);
    assert_eq!((d_first, d61, d121), (4, 4, 2));
    // Seam excluded (ts(120) - ts(61) = 590_000) would leave 4: d121 = 2 proves the seam is counted.
    assert_eq!(expected_next(d61, ts[120] - ts[61]), 4);
    // What T5-01 (genesis ts 0 clamped to the ceiling) demanded:
    let t5_01_d61 = expected_next(d_first, ts[60] - ts[0]);
    let t5_01_d121 = expected_next(t5_01_d61, ts[120] - ts[60]);
    assert_eq!((t5_01_d61, t5_01_d121), (2, 1));
    // What the superseded 59-interval rule would have demanded:
    let legacy = |old: u128, e: u64| (old * 590_000 / u128::from(e.clamp(295_000, 1_180_000))).max(1);
    let legacy_d61 = legacy(4, ts[60] - ts[1]);
    let legacy_d121 = legacy(legacy_d61, ts[120] - ts[61]);
    assert_eq!((legacy_d61, legacy_d121), (4, 4));

    let mut parent = g;
    let mut hashes = vec![g];
    let mut stream = Vec::new();
    let mut meta = Vec::new();
    let mut forks: Vec<(String, Vec<u8>, serde_json::Value)> = Vec::new();
    for number in 1..=121u32 {
        let n = number as usize;
        if number == 60 || number == 61 || number == 121 {
            // Fork sibling on the same parent: ticket easier than the node's difficulty.
            let (lo, hi, label, node_d) = match number {
                60 => (target(4), target(3), "reject-fork60-ticket-between-target4-and-target3", 4u128),
                61 => (target(4), target(3), "reject-fork61-ticket-between-target4-and-target3", 4u128),
                _ => (target(2), target(1), "reject-fork121-ticket-between-target2-and-target1", 2u128),
            };
            let (h, body) = build(&mut ext, &w, &code, parent, number, ts[n], false);
            let c = cert_in(g, h.hash(), Some(lo), hi);
            let (line, hash) = sealed(&h, &body, &c);
            forks.push((label.into(), line, serde_json::json!({"height": number, "parent": format!("{parent:?}"), "hash": format!("{hash:?}"), "timestamp_ms": ts[n], "nonce": c.nonce, "ticket": hexs(&mx::ticket(&c)), "node_expected_difficulty": node_d, "expected": "rejected: invalid seal"})));
        }
        let (h, body) = build(&mut ext, &w, &code, parent, number, ts[n], true);
        // Ticket windows pin the node difficulty: 60 and 61 tickets in (target(5), target(4)]
        // (accepted only if d <= 4; their forks in (target(4), target(3)] rejected only if d >= 4);
        // 121 ticket in (target(3), target(2)] (accepted only if d <= 2; fork121 in
        // (target(2), target(1)] rejected only if d >= 2).
        let (lo, hi, d_node) = match number { 1..=59 => (None, target(4), 4u128), 60 | 61 => (Some(target(5)), target(4), 4), 62..=120 => (None, target(4), 4), _ => (Some(target(3)), target(2), 2) };
        let c = cert_in(g, h.hash(), lo, hi);
        let (line, hash) = sealed(&h, &body, &c);
        stream.extend_from_slice(&line);
        meta.push(serde_json::json!({"height": number, "parent": format!("{parent:?}"), "hash": format!("{hash:?}"), "timestamp_ms": ts[n], "nonce": c.nonce, "ticket": hexs(&mx::ticket(&c)), "node_expected_difficulty": d_node, "legacy_59_interval_difficulty": if number <= 60 { 4 } else if number <= 120 { legacy_d61 } else { legacy_d121 }, "t5_01_difficulty": if number <= 60 { 4 } else if number <= 120 { t5_01_d61 } else { t5_01_d121 }}));
        parent = hash;
        hashes.push(hash);
    }
    let main = d.join("blocks/main-1-121.json");
    fs::write(&main, &stream).unwrap();
    fs::write(d.join("fixtures.json"), serde_json::to_vec_pretty(&serde_json::json!({"genesis": format!("{g:?}"), "genesis_timestamp_ms": genesis_ts, "genesis_now_storage_present": genesis_now_present, "t0_ms": t0, "seam_60_61_ms": ts[61] - ts[60], "first_window_elapsed_ms": ts[60] - ts[0], "second_window_elapsed_ms": ts[120] - ts[60], "genesis_ts0_skip_retarget": true, "expected": {"d1_60": 4, "d61_120": d61, "d121": d121}, "t5_01_superseded": {"d61_120": t5_01_d61, "d121": t5_01_d121}, "legacy_59_interval": {"d61_120": legacy_d61, "d121": legacy_d121}, "blocks": meta})).unwrap()).unwrap();

    let mut results = Vec::new();
    let import = |label: &str, file: &PathBuf| -> (String, Option<i32>) {
        let (_, log, exit) = cli(label, &["import-blocks", file.to_str().unwrap(), "--chain", CHAIN, "--base-path", &base, "--detailed-log-output", "-l", "sync=trace,pow=trace"]);
        (log, exit)
    };
    let (main_log, main_exit) = import("import-main-1-121", &main);
    let main_ok = main_log.contains("Imported 121 blocks") && !main_log.contains("rror importing");
    let mx10_traces = main_log.matches("MX10_VERIFY_PRE_HASH").count();
    let export = d.join("mx10-export-1-121.json").to_string_lossy().into_owned();
    let (_, _, eexit) = cli("mx10-export-1-121", &["export-blocks", "--chain", CHAIN, "--base-path", &base, "--from", "1", "--to", "121", &export]);
    let exported = fs::read_to_string(&export).unwrap_or_default();
    let exported_hashes: Vec<String> = serde_json::Deserializer::from_str(&exported).into_iter::<SignedBlock<Block>>().map(|b| format!("{:?}", b.unwrap().block.header().hash())).collect();
    let expected_hashes: Vec<String> = hashes[1..].iter().map(|h| format!("{h:?}")).collect();
    results.push(serde_json::json!({"case": "import_main_chain_1_121_continuous_window", "expected": "accepted; genesis ts 0 -> first retarget skipped: blocks 60 and 61 tickets in (target(5),target(4)] prove node difficulty 4 at 61 (no halving); block 121 ticket in (target(3),target(2)] proves node difficulty 2 at 121 (second window from block 60, seam counted, elapsed 890_000)", "accepted": main_ok, "exit": main_exit, "mx10_verify_traces": mx10_traces, "export_exit": eexit, "export_hashes_match": exported_hashes == expected_hashes, "key_lines": key_lines(&main_log)}));

    for (label, line, info) in &forks {
        let f = d.join(format!("blocks/{label}.json"));
        fs::write(&f, line).unwrap();
        let (log, exit) = import(label, &f);
        let rejected = !log.contains("Imported 1 blocks");
        results.push(serde_json::json!({"case": label, "block": info, "rejected": rejected, "invalid_seal_logged": log.to_lowercase().contains("invalid seal"), "exit": exit, "key_lines": key_lines(&log)}));
    }

    // Adjacent-timestamp cases on parent #121 (node difficulty 2 at 122: tickets <= target(2)).
    let p121 = hashes[121];
    let (valid, valid_body) = build(&mut ext, &w, &code, p121, 122, ts[121] + 5_000, false);
    let adj = [
        ("import_reject_adjacent_timestamp_regression", ts[121] - 1, false),
        ("import_reject_adjacent_timestamp_equal", ts[121], false),
        ("import_reject_adjacent_increment_below_5000ms", ts[121] + 4_999, false),
        ("import_accept_adjacent_increment_at_5000ms", ts[121] + 5_000, true),
    ];
    for (label, millis, accept) in adj {
        let (h, body) = if accept { (valid.clone(), valid_body.clone()) } else { retimestamp(&valid, millis) };
        let c = cert_in(g, h.hash(), None, target(d121));
        let (line, hash) = sealed(&h, &body, &c);
        let f = d.join(format!("blocks/{label}.json"));
        fs::write(&f, &line).unwrap();
        let (log, exit) = import(label, &f);
        let accepted = log.contains("Imported 1 blocks");
        results.push(serde_json::json!({"case": label, "parent_timestamp_ms": ts[121], "timestamp_ms": millis, "increment_ms": millis as i64 - ts[121] as i64, "hash": format!("{hash:?}"), "expected_accept": accept, "accepted": accepted, "too_early_logged": log.contains("TooEarly") || log.contains("The time since the last timestamp is lower than the minimum period"), "built_by_runtime": accept, "exit": exit, "key_lines": key_lines(&log)}));
    }

    // Future-drift cases: generated immediately before import (system clock).
    for (label, ahead, accept) in [("import_accept_timestamp_exactly_30000ms_ahead", 30_000u64, true), ("import_reject_timestamp_more_than_30000ms_ahead", 45_000u64, false)] {
        let gen_ms = now_ms();
        let millis = gen_ms + ahead;
        let (h, body) = build(&mut ext, &w, &code, p121, 122, millis, false);
        let c = cert_in(g, h.hash(), None, target(d121));
        let (line, hash) = sealed(&h, &body, &c);
        let f = d.join(format!("blocks/{label}.json"));
        fs::write(&f, &line).unwrap();
        let before_import_ms = now_ms();
        let (log, exit) = import(label, &f);
        let after_import_ms = now_ms();
        let accepted = log.contains("Imported 1 blocks");
        results.push(serde_json::json!({"case": label, "generated_at_unix_ms": gen_ms, "timestamp_ms": millis, "ahead_of_generation_ms": ahead, "import_started_unix_ms": before_import_ms, "import_finished_unix_ms": after_import_ms, "ahead_of_import_start_ms": millis as i64 - before_import_ms as i64, "hash": format!("{hash:?}"), "expected_accept": accept, "accepted": accepted, "too_far_in_future_logged": log.contains("TooFarInFuture") || log.contains("The timestamp of the block is too far in the future"), "exit": exit, "key_lines": key_lines(&log)}));
    }
    fs::write(d.join("t5-import-results.json"), serde_json::to_vec_pretty(&results).unwrap()).unwrap();

    assert!(main_ok, "main chain import");
    assert_eq!(mx10_traces, 121);
    assert!(results[0]["export_hashes_match"].as_bool().unwrap());
    for r in &results[1..4] { assert!(r["rejected"].as_bool().unwrap() && r["invalid_seal_logged"].as_bool().unwrap(), "{r}"); }
    for r in &results[4..8] {
        let accept = r["expected_accept"].as_bool().unwrap();
        assert_eq!(r["accepted"].as_bool().unwrap(), accept, "{r}");
        if !accept { assert!(r["too_early_logged"].as_bool().unwrap(), "{r}"); }
    }
    assert_eq!(results.len(), 10);
    assert!(results[8]["accepted"].as_bool().unwrap(), "{}", results[8]);
    assert!(!results[9]["accepted"].as_bool().unwrap() && results[9]["too_far_in_future_logged"].as_bool().unwrap(), "{}", results[9]);
    let _ = EXPERIMENTAL_TARGET;
}
