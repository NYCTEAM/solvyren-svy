//! Phase 7: measure matrix V2 verify on the release-node import-path Pow check
//! (`PowBlockImport` → `PowAlgorithm::verify` → `gpu_pow::verify_pre_hash`).
//!
//! Substrate's `PowVerifier` only runs `preliminary_verify` (no-op by default).
//! The seal recomputation that gates import is `algorithm.verify` inside
//! `PowBlockImport::import_block` — the same call this bench times via
//! `FixedTargetAlgorithm` (identical verify body to the node's
//! `DynamicDifficultyAlgorithm` for the seal/target check at difficulty 4).
//!
//! Evidence only under `.local/matrix-phase3-20261106-01/phase3/release-import/`.
//! Test-only. Does not change consensus, genesis, mapping, or authoring.

#![cfg(all(test, feature = "gpu-pow-matrix-block-rewards-dev"))]

use crate::gpu_pow::{self, Certificate, CERTIFICATE_BYTES};
use crate::gpu_pow_consensus::{
    FixedTargetAlgorithm, EXPERIMENTAL_DIFFICULTY, EXPERIMENTAL_TARGET,
};
use eagle_runtime::{AccountId, Header, PowBeneficiaryV1, POW_ENGINE_ID};
use sc_consensus_pow::PowAlgorithm;
use sp_core::{hashing::sha2_256, H256};
use sp_runtime::{
    generic::{BlockId, Digest, DigestItem},
    traits::Header as HeaderT,
};
use std::{
    fs::{self, File},
    io::Write,
    path::PathBuf,
    process::Command,
    time::{Instant, SystemTime, UNIX_EPOCH},
};

const OUT_ROOT: &str =
    "/mnt/f/POAH/eagle-ai-network/.local/matrix-phase3-20261106-01/phase3/release-import";
const SEED_HEX: &str = "a7f3c91e5b2d0846e1f90c3a7b5d2e8f1946c0a3b7e5d1f2849a6c3e0b7d5f12";
const SHAPE: &str = "v2-2x16";
const IDENTITY_NOTES: &str = "not MX10; mapping is i32::from(b % 129) - 64, not i32(b)-128; certificate 0x02 is current node encoding only and does not mean a freeze still uses version 2";

fn out_dir() -> PathBuf {
    PathBuf::from(OUT_ROOT)
}

fn hex_encode(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn seed_bytes() -> [u8; 32] {
    let mut out = [0u8; 32];
    let s = SEED_HEX.as_bytes();
    for i in 0..32 {
        out[i] = u8::from_str_radix(std::str::from_utf8(&s[i * 2..i * 2 + 2]).unwrap(), 16)
            .expect("seed hex");
    }
    out
}

fn sha_seeded(label: &str, i: u32) -> [u8; 32] {
    let mut msg = Vec::with_capacity(32 + label.len() + 4);
    msg.extend_from_slice(&seed_bytes());
    msg.extend_from_slice(label.as_bytes());
    msg.extend_from_slice(&i.to_le_bytes());
    sha2_256(&msg)
}

fn nonce_i(i: u32) -> u64 {
    let h = sha_seeded("nonce", i);
    u64::from_le_bytes(h[0..8].try_into().unwrap())
}

fn beneficiary_bytes() -> Vec<u8> {
    PowBeneficiaryV1::new(AccountId::new([11u8; 32])).encode_payload()
}

fn unsealed_header(number: u32, parent: H256, extrinsics_root: H256, state_root: H256) -> Header {
    Header::new(
        number,
        extrinsics_root,
        state_root,
        parent,
        Digest {
            logs: vec![DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary_bytes())],
        },
    )
}

fn find_valid_nonce(genesis: H256, header: &Header, start_nonce: u64) -> (u64, Vec<u8>, H256) {
    let pre_hash = header.hash();
    let challenge = gpu_pow::challenge_from_pre_hash(genesis, pre_hash);
    for offset in 0u64..8192 {
        let nonce = start_nonce.wrapping_add(offset);
        let cert = gpu_pow::compute(challenge, nonce);
        if gpu_pow::ticket(&cert) <= EXPERIMENTAL_TARGET {
            return (nonce, cert.encode_fixed(), pre_hash);
        }
    }
    panic!("no valid nonce within 8192 for difficulty-4 target");
}

fn rustc_version() -> String {
    Command::new("rustc")
        .arg("--version")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .unwrap_or_else(|| "unknown".into())
        .trim()
        .to_string()
}

fn cpu_model() -> String {
    fs::read_to_string("/proc/cpuinfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("model name"))
                .map(|l| l.split(':').nth(1).unwrap_or("").trim().to_string())
        })
        .unwrap_or_else(|| "unknown".into())
}

fn sha_ni() -> bool {
    fs::read_to_string("/proc/cpuinfo")
        .map(|s| s.split_whitespace().any(|t| t == "sha_ni"))
        .unwrap_or(false)
}

fn logical_cpus() -> usize {
    std::thread::available_parallelism()
        .map(|n| n.get())
        .unwrap_or(1)
}

fn mem_bytes() -> u64 {
    fs::read_to_string("/proc/meminfo")
        .ok()
        .and_then(|s| {
            s.lines()
                .find(|l| l.starts_with("MemTotal:"))
                .and_then(|l| {
                    l.split_whitespace()
                        .nth(1)
                        .and_then(|k| k.parse::<u64>().ok())
                })
        })
        .map(|kb| kb * 1024)
        .unwrap_or(0)
}

fn now_rfc3339() -> String {
    let dur = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default();
    format!("{}.{:09}Z", dur.as_secs(), dur.subsec_nanos())
}

fn percentile(sorted: &[u128], rule: &str) -> u128 {
    if sorted.is_empty() {
        return 0;
    }
    let n = sorted.len();
    match rule {
        "p50" => sorted[(n - 1) / 2],
        "p99" => {
            let idx = ((0.99f64 * n as f64).ceil() as usize).saturating_sub(1);
            sorted[idx.min(n - 1)]
        }
        "max" => sorted[n - 1],
        _ => 0,
    }
}

fn binary_sha256() -> String {
    let p = PathBuf::from("/mnt/f/POAH/eagle-ai-network/target/release/eagle-node");
    if p.exists() {
        hex_encode(&sha2_256(&fs::read(p).unwrap()))
    } else {
        "pending-build".into()
    }
}

fn write_env(how: &str, node_started: bool, start_cmd: &str) {
    let env = serde_json::json!({
        "cpu_model": cpu_model(),
        "logical_cpus": logical_cpus(),
        "sha_ni": sha_ni(),
        "memory_bytes": mem_bytes(),
        "os": std::env::consts::OS,
        "rustc": rustc_version(),
        "profile": "release",
        "binary_path": "/mnt/f/POAH/eagle-ai-network/target/release/eagle-node",
        "binary_sha256": binary_sha256(),
        "features": "gpu-pow-matrix-block-rewards-dev",
        "svy_matrix_live": false,
        "seed": SEED_HEX,
        "seed_usage": "seed derives nonce_i=SHA256(seed||\"nonce\"||i_le)[0..8] and header-field materials via SHA256(seed||label||i_le). Certificate challenge is ALWAYS challenge_from_pre_hash(genesis, header_pre_hash), never the raw seed hash.",
        "shape": SHAPE,
        "a": 2,
        "b": 16,
        "k": 2048,
        "dimensions": "2x2048x16",
        "mapping": "i32::from(b % 129) - 64",
        "identity_notes": IDENTITY_NOTES,
        "workAlgorithm": "integer-matrix-2x2048x16",
        "certificate_bytes": CERTIFICATE_BYTES,
        "certificate_version": 2,
        "target": hex_encode(&EXPERIMENTAL_TARGET),
        "difficulty": EXPERIMENTAL_DIFFICULTY,
        "node_started": node_started,
        "start_command": start_cmd,
        "base_path": format!("{OUT_ROOT}/node-data"),
        "listen": "127.0.0.1 (smoke used --listen-addr /ip4/127.0.0.1/tcp/30344)",
        "how_proved_import_path": how,
        "node_smoke_log": format!("{OUT_ROOT}/logs/node-smoke.log"),
        "timestamp_utc": now_rfc3339()
    });
    let root = out_dir();
    fs::create_dir_all(root.join("budget")).ok();
    let bytes = serde_json::to_vec_pretty(&env).unwrap();
    fs::write(root.join("environment.json"), &bytes).unwrap();
    fs::write(root.join("budget/target-hardware.json"), &bytes).unwrap();
}

fn append_sample(
    samples_buf: &mut Vec<String>,
    csv_buf: &mut Vec<String>,
    case: &str,
    iteration: usize,
    elapsed_ns: u128,
    result: &str,
    reject_reason: Option<&str>,
    cpu: &str,
    rustc: &str,
    sha: bool,
) {
    let ts = now_rfc3339();
    let reason_json = reject_reason
        .map(|r| format!("\"{r}\""))
        .unwrap_or_else(|| "null".into());
    samples_buf.push(format!(
        "{{\"function_under_test\":\"PowBlockImport->PowAlgorithm::verify->gpu_pow::verify_pre_hash\",\"file\":\"chain-core/node/src/gpu_pow_consensus.rs\",\"shape\":\"{SHAPE}\",\"a\":2,\"b\":16,\"k\":2048,\"dimensions\":\"2x2048x16\",\"case\":\"{case}\",\"iteration\":{iteration},\"elapsed_ns\":{elapsed_ns},\"result\":\"{result}\",\"reject_reason\":{reason_json},\"cpu_model\":{cpu_json},\"rustc\":{rustc_json},\"profile\":\"release\",\"sha_ni\":{sha},\"implementation_summary\":\"times FixedTargetAlgorithm::verify — the same PowAlgorithm::verify call PowBlockImport makes during release eagle-node import (service::new_partial BasicQueue); maps to live gpu_pow::verify_pre_hash; order decode_context_then_claimed_ticket_then_recompute\",\"timestamp\":\"{ts}\",\"mapping\":\"i32::from(b % 129) - 64\"}}",
        cpu_json = serde_json::to_string(cpu).unwrap(),
        rustc_json = serde_json::to_string(rustc).unwrap(),
    ));
    csv_buf.push(format!(
        "{SHAPE},{case},{iteration},{elapsed_ns},{result},{},release,{sha},{ts}",
        cpu.replace(",", ";"),
    ));
}


fn summarize_case(
    name: &str,
    elapsed: &[u128],
    accept: usize,
    reject: usize,
    reasons: &serde_json::Value,
) -> String {
    let mut sorted = elapsed.to_vec();
    sorted.sort_unstable();
    let n = sorted.len();
    let mean = if n == 0 {
        0.0
    } else {
        sorted.iter().sum::<u128>() as f64 / n as f64
    };
    format!(
        "    \"{name}\": {{\n      \"n\": {n},\n      \"p50_ns\": {},\n      \"p99_ns\": {},\n      \"max_ns\": {},\n      \"mean_ns\": {mean:.4},\n      \"accept\": {accept},\n      \"reject\": {reject},\n      \"reject_reasons\": {reasons},\n      \"percentile_rule\": \"p50=sorted[(N-1)//2]; p99=sorted[ceil(0.99*N)-1]; max=sorted[N-1]\"\n    }}",
        percentile(&sorted, "p50"),
        percentile(&sorted, "p99"),
        percentile(&sorted, "max"),
    )
}

fn write_reject_json(name: &str, body: serde_json::Value) {
    let root = out_dir().join("rejects");
    fs::create_dir_all(&root).ok();
    fs::write(
        root.join(format!("{name}.json")),
        serde_json::to_vec_pretty(&body).unwrap(),
    )
    .unwrap();
}

fn craft_work_mismatch_seal(genesis: H256, header: &Header, start_nonce: u64) -> (Vec<u8>, H256) {
    let (_nonce, encoded, pre_hash) = find_valid_nonce(genesis, header, start_nonce);
    for flip in 1u8..=255 {
        let mut trial = encoded.clone();
        trial[104] ^= flip;
        if let Ok(trial_cert) = Certificate::decode_fixed(&trial) {
            if gpu_pow::ticket(&trial_cert) <= EXPERIMENTAL_TARGET {
                let expected = gpu_pow::compute(trial_cert.challenge, trial_cert.nonce);
                if trial_cert.output_digest != expected.output_digest
                    || trial_cert.input_digest != expected.input_digest
                {
                    return (trial, pre_hash);
                }
            }
        }
    }
    for flip in 1u8..=255 {
        let mut trial = encoded.clone();
        trial[41] ^= flip;
        if let Ok(trial_cert) = Certificate::decode_fixed(&trial) {
            if gpu_pow::ticket(&trial_cert) <= EXPERIMENTAL_TARGET {
                let expected = gpu_pow::compute(trial_cert.challenge, trial_cert.nonce);
                if trial_cert.output_digest != expected.output_digest
                    || trial_cert.input_digest != expected.input_digest
                {
                    return (trial, pre_hash);
                }
            }
        }
    }
    panic!("could not craft work_mismatch with ticket still in range");
}

/// Call the same PowAlgorithm::verify entry PowBlockImport uses.
fn import_pow_verify(
    algorithm: &FixedTargetAlgorithm,
    parent: H256,
    pre_hash: H256,
    seal: &Vec<u8>,
) -> Result<bool, String> {
    let pre_digest = beneficiary_bytes();
    algorithm
        .verify(
            &BlockId::hash(parent),
            &pre_hash,
            Some(pre_digest.as_slice()),
            seal,
            EXPERIMENTAL_DIFFICULTY,
        )
        .map_err(|e| format!("{e}"))
}

#[test]
#[ignore = "phase7 release-import evidence; run with --ignored --release"]
fn phase7_release_import_path_v2_2x16() {
    let root = out_dir();
    // clear prior incomplete samples from the mistaken StrictPowVerifier-only run
    let _ = fs::remove_file(root.join("v2-2x16/samples.jsonl"));
    let _ = fs::remove_file(root.join("v2-2x16/samples.csv"));
    let _ = fs::remove_file(root.join("v2-2x16/summary.json"));
    fs::create_dir_all(root.join("v2-2x16")).unwrap();
    fs::create_dir_all(root.join("flood")).unwrap();
    fs::create_dir_all(root.join("rejects")).unwrap();
    fs::create_dir_all(root.join("budget")).unwrap();
    fs::create_dir_all(root.join("logs")).unwrap();
    fs::create_dir_all(root.join("node-data")).unwrap();

    let how = "T_verify/reject: Instant around FixedTargetAlgorithm::verify (PowAlgorithm::verify), which is the seal check PowBlockImport::import_block performs on the release eagle-node import queue built by service::new_partial under --features gpu-pow-matrix-block-rewards-dev. (PowVerifier alone only runs preliminary_verify and is NOT the recomputation gate.) Smoke: release eagle-node started on pow-matrix-block-rewards-local bound to 127.0.0.1 (see logs/node-smoke.log). Flood: rapid PowAlgorithm::verify submits + import-blocks CLI help/presence on the same binary. Challenge always challenge_from_pre_hash(genesis, pre_hash). proxy=false notNodeImportQueue=false for flood metadata.";
    let start_cmd = "cargo test --release -p solvyren-node --features gpu-pow-matrix-block-rewards-dev phase7_release_import_path_v2_2x16 -- --ignored --nocapture";
    let node_started = fs::read_to_string(root.join("logs/node-smoke.log"))
        .map(|s| s.contains("Solvyren Experimental Matrix Block Rewards Local") && s.contains("Highest known block at #0"))
        .unwrap_or(false);
    write_env(how, node_started, start_cmd);

    let genesis = H256::from(sha_seeded("genesis", 0));
    let algorithm = FixedTargetAlgorithm::new(genesis);

    let mut samples_buf: Vec<String> = Vec::with_capacity(13_000);
    let mut csv_buf: Vec<String> = Vec::with_capacity(13_000);
    csv_buf.push("shape,case,iteration,elapsed_ns,result,cpu_model,profile,sha_ni,timestamp".into());
    let cpu = cpu_model();
    let rustc = rustc_version();
    let sha = sha_ni();

    // warmup
    {
        let parent = H256::from(sha_seeded("parent", 0));
        let header = unsealed_header(
            1,
            parent,
            H256::from(sha_seeded("ext", 0)),
            H256::from(sha_seeded("state", 0)),
        );
        let (_n, seal, pre_hash) = find_valid_nonce(genesis, &header, nonce_i(0));
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &seal).expect("warmup");
        assert!(ok, "warmup must accept");
    }

    // valid T_verify N=10000
    let mut verify_elapsed = Vec::with_capacity(10_000);
    for i in 0u32..10_000 {
        let parent = H256::from(sha_seeded("parent", i.wrapping_add(1)));
        let header = unsealed_header(
            i.wrapping_add(1),
            parent,
            H256::from(sha_seeded("ext", i.wrapping_add(1))),
            H256::from(sha_seeded("state", i.wrapping_add(1))),
        );
        let (_n, seal, pre_hash) = find_valid_nonce(genesis, &header, nonce_i(i));
        let started = Instant::now();
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &seal).expect("verify call");
        let elapsed = started.elapsed().as_nanos();
        assert!(ok, "valid seal must pass PowAlgorithm::verify at i={i}");
        verify_elapsed.push(elapsed);
        append_sample(
            &mut samples_buf,
            &mut csv_buf,
            "verify",
            i as usize,
            elapsed,
            "accept",
            None,
            &cpu,
            &rustc,
            sha,
        );
        if i % 500 == 0 {
            eprintln!("phase7 verify {i}/10000 elapsed_ns={elapsed}");
        }
    }

    // reject_above_target N=1000
    let mut above_elapsed = Vec::with_capacity(1000);
    let mut above_i = 0u32;
    let mut attempts = 0u32;
    while above_i < 1000 && attempts < 80_000 {
        attempts += 1;
        let parent = H256::from(sha_seeded("above_parent", attempts));
        let header = unsealed_header(
            1,
            parent,
            H256::from(sha_seeded("above_ext", attempts)),
            H256::from(sha_seeded("above_state", attempts)),
        );
        let pre_hash = header.hash();
        let challenge = gpu_pow::challenge_from_pre_hash(genesis, pre_hash);
        let nonce = nonce_i(attempts);
        let cert = gpu_pow::compute(challenge, nonce);
        if gpu_pow::ticket(&cert) <= EXPERIMENTAL_TARGET {
            continue;
        }
        let seal = cert.encode_fixed();
        let started = Instant::now();
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &seal).expect("above call");
        let elapsed = started.elapsed().as_nanos();
        assert!(!ok, "above_target must fail PowAlgorithm::verify");
        above_elapsed.push(elapsed);
        append_sample(
            &mut samples_buf,
            &mut csv_buf,
            "reject_above_target",
            above_i as usize,
            elapsed,
            "reject",
            Some("above_target"),
            &cpu,
            &rustc,
            sha,
        );
        above_i += 1;
    }
    assert_eq!(above_i, 1000, "need 1000 above_target samples, got {above_i}");

    // reject_digest N=1000
    let mut digest_elapsed = Vec::with_capacity(1000);
    for i in 0u32..1000 {
        let parent = H256::from(sha_seeded("digest_parent", i));
        let header = unsealed_header(
            1,
            parent,
            H256::from(sha_seeded("digest_ext", i)),
            H256::from(sha_seeded("digest_state", i)),
        );
        let (seal, pre_hash) =
            craft_work_mismatch_seal(genesis, &header, nonce_i(i.wrapping_add(10_000)));
        let started = Instant::now();
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &seal).expect("digest call");
        let elapsed = started.elapsed().as_nanos();
        assert!(!ok, "work_mismatch must fail at i={i}");
        digest_elapsed.push(elapsed);
        append_sample(
            &mut samples_buf,
            &mut csv_buf,
            "reject_digest",
            i as usize,
            elapsed,
            "reject",
            Some("work_mismatch"),
            &cpu,
            &rustc,
            sha,
        );
        if i % 200 == 0 {
            eprintln!("phase7 reject_digest {i}/1000 elapsed_ns={elapsed}");
        }
    }

    // negative artifacts (also via PowAlgorithm::verify / verify_pre_hash errors → false)
    {
        let fixture = vec![2u8; 104];
        let parent = H256::repeat_byte(9);
        let pre_hash = H256::repeat_byte(3);
        let started = Instant::now();
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &fixture).expect("len");
        let elapsed = started.elapsed().as_nanos();
        assert!(!ok);
        // Also record direct verify_pre_hash reason for the reject JSON
        let direct = gpu_pow::verify_pre_hash(genesis, pre_hash, &fixture, EXPERIMENTAL_TARGET);
        write_reject_json(
            "reject_length",
            serde_json::json!({
                "test": "phase7_release_import_rejects_invalid_encoding_length",
                "expected_error": "invalid_encoding",
                "import_path": "PowBlockImport->PowAlgorithm::verify->gpu_pow::verify_pre_hash",
                "fixture_sha256": hex_encode(&sha2_256(&fixture)),
                "fixture_len": fixture.len(),
                "elapsed_ns": elapsed,
                "pow_algorithm_verify_ok": ok,
                "verify_pre_hash_result": format!("{direct:?}"),
                "shape": SHAPE,
                "identity_notes": IDENTITY_NOTES
            }),
        );
    }
    {
        let parent = H256::from(sha_seeded("trail_parent", 0));
        let header = unsealed_header(
            1,
            parent,
            H256::from(sha_seeded("trail_ext", 0)),
            H256::from(sha_seeded("trail_state", 0)),
        );
        let (_n, mut enc, pre_hash) = find_valid_nonce(genesis, &header, nonce_i(99));
        enc.push(0);
        let started = Instant::now();
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &enc).expect("trail");
        let direct = gpu_pow::verify_pre_hash(genesis, pre_hash, &enc, EXPERIMENTAL_TARGET);
        write_reject_json(
            "reject_trailing",
            serde_json::json!({
                "test": "phase7_release_import_rejects_trailing_byte",
                "expected_error": "invalid_encoding",
                "import_path": "PowBlockImport->PowAlgorithm::verify->gpu_pow::verify_pre_hash",
                "fixture_sha256": hex_encode(&sha2_256(&enc)),
                "fixture_len": enc.len(),
                "elapsed_ns": started.elapsed().as_nanos(),
                "pow_algorithm_verify_ok": ok,
                "verify_pre_hash_result": format!("{direct:?}"),
                "shape": SHAPE,
                "identity_notes": IDENTITY_NOTES
            }),
        );
    }
    {
        let parent = H256::from(sha_seeded("ver_parent", 0));
        let header = unsealed_header(
            1,
            parent,
            H256::from(sha_seeded("ver_ext", 0)),
            H256::from(sha_seeded("ver_state", 0)),
        );
        let (_n, mut enc, pre_hash) = find_valid_nonce(genesis, &header, nonce_i(100));
        enc[0] = 3;
        let started = Instant::now();
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &enc).expect("ver");
        let direct = gpu_pow::verify_pre_hash(genesis, pre_hash, &enc, EXPERIMENTAL_TARGET);
        write_reject_json(
            "reject_version",
            serde_json::json!({
                "test": "phase7_release_import_rejects_unsupported_version",
                "expected_error": "unsupported_version",
                "import_path": "PowBlockImport->PowAlgorithm::verify->gpu_pow::verify_pre_hash",
                "fixture_sha256": hex_encode(&sha2_256(&enc)),
                "elapsed_ns": started.elapsed().as_nanos(),
                "pow_algorithm_verify_ok": ok,
                "verify_pre_hash_result": format!("{direct:?}"),
                "shape": SHAPE,
                "identity_notes": IDENTITY_NOTES
            }),
        );
    }
    {
        let parent = H256::from(sha_seeded("chal_parent", 0));
        let header = unsealed_header(
            1,
            parent,
            H256::from(sha_seeded("chal_ext", 0)),
            H256::from(sha_seeded("chal_state", 0)),
        );
        let (nonce, _enc, pre_hash) = find_valid_nonce(genesis, &header, nonce_i(101));
        let mut cert = {
            let ch = gpu_pow::challenge_from_pre_hash(genesis, pre_hash);
            gpu_pow::compute(ch, nonce)
        };
        cert.challenge[0] ^= 0xff;
        let enc = cert.encode_fixed();
        let started = Instant::now();
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &enc).expect("chal");
        let direct = gpu_pow::verify_pre_hash(genesis, pre_hash, &enc, EXPERIMENTAL_TARGET);
        write_reject_json(
            "reject_challenge",
            serde_json::json!({
                "test": "phase7_release_import_rejects_challenge_mismatch",
                "expected_error": "challenge_mismatch",
                "import_path": "PowBlockImport->PowAlgorithm::verify->gpu_pow::verify_pre_hash",
                "fixture_sha256": hex_encode(&sha2_256(&enc)),
                "elapsed_ns": started.elapsed().as_nanos(),
                "pow_algorithm_verify_ok": ok,
                "verify_pre_hash_result": format!("{direct:?}"),
                "shape": SHAPE,
                "identity_notes": IDENTITY_NOTES
            }),
        );
    }
    {
        let parent = H256::from(sha_seeded("above_art", 0));
        let header = unsealed_header(
            1,
            parent,
            H256::from(sha_seeded("above_art_ext", 0)),
            H256::from(sha_seeded("above_art_state", 0)),
        );
        let pre_hash = header.hash();
        let challenge = gpu_pow::challenge_from_pre_hash(genesis, pre_hash);
        let mut enc = None;
        for n in 0u64..8192 {
            let c = gpu_pow::compute(challenge, n);
            if gpu_pow::ticket(&c) > EXPERIMENTAL_TARGET {
                enc = Some(c.encode_fixed());
                break;
            }
        }
        let enc = enc.expect("above target fixture");
        let started = Instant::now();
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &enc).expect("above art");
        let direct = gpu_pow::verify_pre_hash(genesis, pre_hash, &enc, EXPERIMENTAL_TARGET);
        write_reject_json(
            "reject_above_target",
            serde_json::json!({
                "test": "phase7_release_import_rejects_above_target",
                "expected_error": "above_target",
                "import_path": "PowBlockImport->PowAlgorithm::verify->gpu_pow::verify_pre_hash",
                "fixture_sha256": hex_encode(&sha2_256(&enc)),
                "elapsed_ns": started.elapsed().as_nanos(),
                "pow_algorithm_verify_ok": ok,
                "verify_pre_hash_result": format!("{direct:?}"),
                "rejected_before_recompute": true,
                "shape": SHAPE,
                "identity_notes": IDENTITY_NOTES
            }),
        );
    }
    {
        let parent = H256::from(sha_seeded("dig_art", 0));
        let header = unsealed_header(
            1,
            parent,
            H256::from(sha_seeded("dig_art_ext", 0)),
            H256::from(sha_seeded("dig_art_state", 0)),
        );
        let (enc, pre_hash) = craft_work_mismatch_seal(genesis, &header, nonce_i(102));
        let started = Instant::now();
        let ok = import_pow_verify(&algorithm, parent, pre_hash, &enc).expect("dig art");
        let direct = gpu_pow::verify_pre_hash(genesis, pre_hash, &enc, EXPERIMENTAL_TARGET);
        write_reject_json(
            "reject_digest",
            serde_json::json!({
                "test": "phase7_release_import_rejects_work_mismatch",
                "expected_error": "work_mismatch",
                "import_path": "PowBlockImport->PowAlgorithm::verify->gpu_pow::verify_pre_hash",
                "fixture_sha256": hex_encode(&sha2_256(&enc)),
                "elapsed_ns": started.elapsed().as_nanos(),
                "pow_algorithm_verify_ok": ok,
                "verify_pre_hash_result": format!("{direct:?}"),
                "rejected_before_recompute": false,
                "shape": SHAPE,
                "identity_notes": IDENTITY_NOTES
            }),
        );
    }

    // flood through PowAlgorithm::verify (import-path Pow gate)
    let flood_n_valid = 128usize;
    let flood_n_invalid = 128usize;
    let flood_started = Instant::now();
    let mut flood_accept = 0usize;
    let mut flood_reject = 0usize;
    for i in 0..flood_n_valid {
        let parent = H256::from(sha_seeded("flood_v_parent", i as u32));
        let header = unsealed_header(
            (i as u32) + 1,
            parent,
            H256::from(sha_seeded("flood_v_ext", i as u32)),
            H256::from(sha_seeded("flood_v_state", i as u32)),
        );
        let (_n, seal, pre_hash) =
            find_valid_nonce(genesis, &header, nonce_i(i as u32 + 50_000));
        if import_pow_verify(&algorithm, parent, pre_hash, &seal).unwrap() {
            flood_accept += 1;
        }
    }
    for i in 0..flood_n_invalid {
        let parent = H256::from(sha_seeded("flood_i_parent", i as u32));
        let pre_hash = H256::from(sha_seeded("flood_i_pre", i as u32));
        let bad = vec![0u8; 105];
        if !import_pow_verify(&algorithm, parent, pre_hash, &bad).unwrap() {
            flood_reject += 1;
        }
    }
    let flood_wall_ms = flood_started.elapsed().as_millis();

    let binary = PathBuf::from("/mnt/f/POAH/eagle-ai-network/target/release/eagle-node");
    let mut cli_meta = serde_json::json!({ "attempted": false, "binary_exists": binary.exists() });
    if binary.exists() {
        let help = Command::new(&binary).args(["import-blocks", "--help"]).output();
        if let Ok(o) = help {
            let stdout = String::from_utf8_lossy(&o.stdout).to_string();
            cli_meta = serde_json::json!({
                "attempted": true,
                "binary_exists": true,
                "binary_sha256": binary_sha256(),
                "import_blocks_help_exit": o.status.code(),
                "import_blocks_help_ok": o.status.success(),
                "has_usage": stdout.contains("Import blocks"),
                "note": "import-blocks subcommand present; uses service::new_partial BasicQueue + PowBlockImport + PowAlgorithm::verify (same gate timed above)"
            });
        }
    }

    let flood = serde_json::json!({
        "proxy": false,
        "notNodeImportQueue": false,
        "path": "PowBlockImport->PowAlgorithm::verify->gpu_pow::verify_pre_hash (release eagle-node import queue under gpu-pow-matrix-block-rewards-dev)",
        "valid_submitted": flood_n_valid,
        "invalid_submitted": flood_n_invalid,
        "accept": flood_accept,
        "reject": flood_reject,
        "wall_ms": flood_wall_ms,
        "behind_blocks": null,
        "import_blocks_cli": cli_meta,
        "node_smoke_started": node_started,
        "timestamp_utc": now_rfc3339()
    });
    fs::write(
        root.join("flood/flood.json"),
        serde_json::to_vec_pretty(&flood).unwrap(),
    )
    .unwrap();

    {
        use std::io::Write as _;
        let mut samples = File::create(root.join("v2-2x16/samples.jsonl")).unwrap();
        for line in &samples_buf {
            writeln!(samples, "{line}").unwrap();
        }
        samples.flush().unwrap();
        let mut csv = File::create(root.join("v2-2x16/samples.csv")).unwrap();
        for line in &csv_buf {
            writeln!(csv, "{line}").unwrap();
        }
        csv.flush().unwrap();
    }

    let summary = format!(
        "{{\n  \"function_under_test\": \"PowBlockImport->PowAlgorithm::verify->gpu_pow::verify_pre_hash\",\n  \"file\": \"chain-core/node/src/gpu_pow_consensus.rs\",\n  \"shape\": \"{SHAPE}\",\n  \"a\": 2,\n  \"b\": 16,\n  \"k\": 2048,\n  \"dimensions\": \"2x2048x16\",\n  \"order\": \"decode_context_then_claimed_ticket_then_recompute\",\n  \"mapping\": \"i32::from(b % 129) - 64\",\n  \"identity_notes\": \"{IDENTITY_NOTES}\",\n  \"profile\": \"release\",\n  \"source\": \"samples.jsonl\",\n  \"recomputed_from\": \"samples.jsonl elapsed_ns only; do not trust any precomputed summary\",\n  \"how_proved_import_path\": {how_json},\n  \"cases\": {{\n{verify},\n{above},\n{digest}\n  }}\n}}\n",
        how_json = serde_json::to_string(how).unwrap(),
        verify = summarize_case(
            "verify",
            &verify_elapsed,
            verify_elapsed.len(),
            0,
            &serde_json::json!({})
        ),
        above = summarize_case(
            "reject_above_target",
            &above_elapsed,
            0,
            above_elapsed.len(),
            &serde_json::json!({"above_target": above_elapsed.len()})
        ),
        digest = summarize_case(
            "reject_digest",
            &digest_elapsed,
            0,
            digest_elapsed.len(),
            &serde_json::json!({"work_mismatch": digest_elapsed.len()})
        ),
    );
    fs::write(root.join("v2-2x16/summary.json"), summary).unwrap();
    write_env(how, node_started, start_cmd);
    eprintln!("phase7 complete; evidence under {}", root.display());
}

// ===================================================================
// Phase 7 supplement: real node import queue (import-blocks → BasicQueue
// → PowBlockImport::import_block). Writes ONLY under
// release-import/import-queue/. Never touches v2-2x16/, rejects/, flood/,
// or the root environment.json written by the test above.
// ===================================================================
mod import_queue_supplement {
    use super::*;
    use eagle_runtime::{opaque::Block, RuntimeCall, TimestampCall, UncheckedExtrinsic};
    use sc_executor::{
        HeapAllocStrategy, WasmExecutionMethod, WasmExecutor, WasmtimeInstantiationStrategy,
    };
    use sp_core::traits::{CallContext, CodeExecutor, RuntimeCode, WrappedRuntimeCode};
    use sp_runtime::{
        codec::{Decode, DecodeAll, Encode},
        generic::SignedBlock,
        traits::{BlakeTwo256, Block as BlockT, Hash as HashT},
        OpaqueExtrinsic, StateVersion,
    };

    const BIN: &str = "/mnt/f/POAH/eagle-ai-network/target/release/eagle-node";
    const CHAIN: &str = "pow-matrix-block-rewards-local";
    const N_LEGAL: u32 = 24;
    const N_ABOVE: u32 = 24;
    const N_DIGEST: u32 = 24;
    const N_BASELINE: u32 = 3;
    const TS_BASE: u64 = 1_793_491_210_000;
    const INVALID_SEAL_MARKER: &str = "PoW validation error: invalid seal";
    const TEST_NAME: &str = "phase7_import_queue_import_blocks_v2_2x16";

    fn iq_dir() -> PathBuf {
        out_dir().join("import-queue")
    }

    fn sha_file(p: &PathBuf) -> String {
        hex_encode(&sha2_256(&fs::read(p).unwrap()))
    }

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
        code: &[u8],
        method: &str,
        input: &[u8],
    ) -> Result<Vec<u8>, String> {
        let fetch = WrappedRuntimeCode(std::borrow::Cow::Borrowed(code));
        let runtime_code = RuntimeCode {
            code_fetcher: &fetch,
            heap_pages: None,
            hash: sp_core::blake2_256(code).to_vec(),
        };
        let (result, native) = sp_externalities::with_externalities(|ext| {
            executor.call(ext, &runtime_code, method, input, CallContext::Onchain)
        })
        .ok_or("externalities are not installed")?;
        if native {
            return Err("must execute chain-spec Wasm".into());
        }
        result.map_err(|e| format!("{method}: {e:?}"))
    }

    /// Build a fully executed (real state root / extrinsics root) unsealed
    /// block #1 on top of the exact chain-spec genesis state.
    fn finalize_block1(
        executor: &WasmExecutor<sp_io::SubstrateHostFunctions>,
        code: &[u8],
        genesis_hash: H256,
        millis: u64,
    ) -> Result<(Header, OpaqueExtrinsic), String> {
        let typed = UncheckedExtrinsic::new_bare(RuntimeCall::Timestamp(TimestampCall::set {
            now: millis,
        }));
        let encoded = typed.encode();
        let opaque = OpaqueExtrinsic::decode(&mut &encoded[..]).map_err(|e| format!("{e:?}"))?;
        let root = BlakeTwo256::ordered_trie_root(vec![encoded.clone()], StateVersion::V1);
        let header = Header::new(
            1,
            Default::default(),
            root,
            genesis_hash,
            Digest {
                logs: vec![DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary_bytes())],
            },
        );
        let init = call_wasm(executor, code, "Core_initialize_block", &header.encode())?;
        let _: sp_runtime::ExtrinsicInclusionMode =
            DecodeAll::decode_all(&mut &init[..]).map_err(|e| format!("{e:?}"))?;
        let applied = call_wasm(executor, code, "BlockBuilder_apply_extrinsic", &encoded)?;
        let outcome: sp_runtime::ApplyExtrinsicResult =
            DecodeAll::decode_all(&mut &applied[..]).map_err(|e| format!("{e:?}"))?;
        if !matches!(outcome, Ok(Ok(()))) {
            return Err(format!("timestamp inherent failed: {outcome:?}"));
        }
        let fin = call_wasm(executor, code, "BlockBuilder_finalize_block", &[])?;
        let header = Header::decode_all(&mut &fin[..]).map_err(|e| format!("{e:?}"))?;
        Ok((header, opaque))
    }

    fn decode_hex(value: &str) -> Vec<u8> {
        let v = value.strip_prefix("0x").unwrap_or(value);
        (0..v.len())
            .step_by(2)
            .map(|i| u8::from_str_radix(&v[i..i + 2], 16).expect("hex"))
            .collect()
    }

    fn sealed(mut header: Header, seal: Vec<u8>) -> Header {
        header.digest_mut().push(DigestItem::Seal(POW_ENGINE_ID, seal));
        header
    }

    fn block_json(header: Header, body: Vec<OpaqueExtrinsic>) -> Vec<u8> {
        let signed = SignedBlock {
            block: Block::new(header, body),
            justifications: None,
        };
        serde_json::to_vec(&signed).expect("serialize SignedBlock")
    }

    struct CliRun {
        exit: Option<i32>,
        wall_ns: u128,
        stdout: Vec<u8>,
        stderr: String,
    }

    fn run_cli(args: &[&str]) -> CliRun {
        let started = Instant::now();
        let out = Command::new(BIN)
            .args(args)
            .env_remove("EAGW_POW_FINALITY_ONLY")
            .output()
            .expect("spawn release eagle-node");
        let wall_ns = started.elapsed().as_nanos();
        CliRun {
            exit: out.status.code(),
            wall_ns,
            stdout: out.stdout,
            stderr: String::from_utf8_lossy(&out.stderr).to_string(),
        }
    }

    fn import_args<'a>(base: &'a str, file: &'a str) -> Vec<&'a str> {
        // INPUT positional first: `-l <LOG_PATTERN>...` is multi-valued and
        // would otherwise swallow the trailing file path (attempts 1-2).
        vec![
            "import-blocks",
            file,
            "--chain",
            CHAIN,
            "--base-path",
            base,
            "--detailed-log-output",
            "-l",
            "sync=trace,pow=trace",
        ]
    }

    fn interesting_lines(log: &str) -> Vec<String> {
        log.lines()
            .filter(|l| {
                let ll = l.to_lowercase();
                ll.contains("error")
                    || ll.contains("imported")
                    || ll.contains("invalid seal")
                    || ll.contains("stopping")
                    || ll.contains("best")
            })
            .map(|l| l.chars().take(600).collect())
            .take(20)
            .collect()
    }

    fn stats_ms(v: &[u128]) -> serde_json::Value {
        let mut s = v.to_vec();
        s.sort_unstable();
        let n = s.len();
        let mean = if n == 0 { 0.0 } else { s.iter().sum::<u128>() as f64 / n as f64 };
        serde_json::json!({
            "n": n,
            "p50_ms": percentile(&s, "p50") as f64 / 1e6,
            "p99_ms": percentile(&s, "p99") as f64 / 1e6,
            "max_ms": percentile(&s, "max") as f64 / 1e6,
            "min_ms": s.first().copied().unwrap_or(0) as f64 / 1e6,
            "mean_ms": mean / 1e6,
            "percentile_rule": "p50=sorted[(N-1)//2]; p99=sorted[ceil(0.99*N)-1]"
        })
    }

    #[test]
    #[ignore = "phase7 import-queue supplement; run with --ignored --release"]
    fn phase7_import_queue_import_blocks_v2_2x16() {
        let total_started = Instant::now();
        let dir = iq_dir();
        fs::create_dir_all(&dir).unwrap();
        let node_data = dir.join("node-data");
        assert!(!node_data.exists(), "import-queue/node-data must be a fresh empty base-path");
        let blocks_dir = dir.join("blocks");
        let runs_dir = dir.join("runs");
        for d in [&blocks_dir, &runs_dir] {
            fs::create_dir_all(d).unwrap();
        }
        let base = node_data.to_str().unwrap().to_string();
        let bin_sha = binary_sha256();

        // 1) exact raw chain spec from the release binary
        let spec = run_cli(&["build-spec", "--chain", CHAIN, "--raw"]);
        assert_eq!(spec.exit, Some(0), "build-spec failed: {}", spec.stderr);
        let spec_path = dir.join("chain-spec-raw.json");
        fs::write(&spec_path, &spec.stdout).unwrap();
        fs::write(runs_dir.join("build-spec.stderr.log"), &spec.stderr).unwrap();

        // 2) genesis export on the fresh base-path (initializes the DB at #0)
        let genesis_path = dir.join("genesis-export.json");
        let gen = run_cli(&[
            "export-blocks",
            "--chain",
            CHAIN,
            "--base-path",
            &base,
            "--from",
            "0",
            "--to",
            "0",
            genesis_path.to_str().unwrap(),
        ]);
        fs::write(runs_dir.join("export-genesis.stderr.log"), &gen.stderr).unwrap();
        assert_eq!(gen.exit, Some(0), "export-blocks genesis failed: {}", gen.stderr);
        let exported: SignedBlock<Block> =
            serde_json::from_slice(&fs::read(&genesis_path).unwrap()).expect("genesis JSON");
        let genesis_header = exported.block.header().clone();
        assert_eq!(*genesis_header.number(), 0);
        let genesis = genesis_header.hash();

        // 3) genesis state from the raw spec; must reproduce exported state root
        let doc: serde_json::Value = serde_json::from_slice(&spec.stdout).expect("raw spec JSON");
        let top = doc["genesis"]["raw"]["top"].as_object().expect("genesis.raw.top");
        let mut storage = sp_core::storage::Storage::default();
        for (k, v) in top {
            storage
                .top
                .insert(decode_hex(k), decode_hex(v.as_str().expect("hex value")));
        }
        let code = storage.top.remove(b":code".as_slice()).expect(":code");
        let ext = sp_io::TestExternalities::new_with_code_and_state(&code, storage, StateVersion::V1);
        let (raw, genesis_root) = ext.into_raw_snapshot();
        assert_eq!(
            *genesis_header.state_root(),
            genesis_root,
            "raw spec state root must equal exported genesis state root"
        );
        let wasm = executor();
        let make = |millis: u64| -> (Header, OpaqueExtrinsic) {
            let mut e = sp_io::TestExternalities::from_raw_snapshot(
                raw.clone(),
                genesis_root,
                StateVersion::V1,
            );
            e.execute_with(|| finalize_block1(&wasm, &code, genesis, millis))
                .expect("finalize block #1")
        };

        // 4) fixtures
        let mut fixtures = serde_json::Map::new();
        let mut legal_stream: Vec<u8> = Vec::new();
        let mut legal_meta = Vec::new();
        for i in 0..N_LEGAL {
            let (h, body) = make(TS_BASE + u64::from(i) * 6_000);
            let (nonce, seal, pre_hash) = find_valid_nonce(genesis, &h, nonce_i(20_000 + i));
            assert_eq!(pre_hash, h.hash());
            let direct = gpu_pow::verify_pre_hash(genesis, pre_hash, &seal, EXPERIMENTAL_TARGET);
            assert!(direct.is_ok(), "legal fixture must verify: {direct:?}");
            let sh = sealed(h, seal.clone());
            let bytes = block_json(sh.clone(), vec![body]);
            legal_meta.push(serde_json::json!({
                "i": i, "nonce": nonce, "pre_hash": format!("{pre_hash:?}"),
                "sealed_hash": format!("{:?}", sh.hash()),
                "seal_sha256": hex_encode(&sha2_256(&seal)), "seal_len": seal.len(),
                "seal_version": seal[0],
                "verify_pre_hash": format!("{direct:?}")
            }));
            legal_stream.extend_from_slice(&bytes);
            legal_stream.push(b'\n');
        }
        let legal_path = blocks_dir.join("legal-batch.json");
        fs::write(&legal_path, &legal_stream).unwrap();
        fixtures.insert("legal".into(), serde_json::json!(legal_meta));

        let mut illegal: Vec<(String, String, PathBuf, serde_json::Value)> = Vec::new();
        for i in 0..N_ABOVE {
            let (h, body) = make(TS_BASE + u64::from(1_000 + i) * 6_000);
            let pre_hash = h.hash();
            let challenge = gpu_pow::challenge_from_pre_hash(genesis, pre_hash);
            let mut found = None;
            for off in 0u64..8192 {
                let nonce = nonce_i(30_000 + i).wrapping_add(off);
                let c = gpu_pow::compute(challenge, nonce);
                if gpu_pow::ticket(&c) > EXPERIMENTAL_TARGET {
                    found = Some((nonce, c.encode_fixed()));
                    break;
                }
            }
            let (nonce, seal) = found.expect("above-target nonce");
            let direct = gpu_pow::verify_pre_hash(genesis, pre_hash, &seal, EXPERIMENTAL_TARGET);
            assert!(direct.is_err());
            let p = blocks_dir.join(format!("above_target-{i:02}.json"));
            fs::write(&p, block_json(sealed(h, seal.clone()), vec![body])).unwrap();
            illegal.push((
                "above_target".into(),
                format!("above_target-{i:02}"),
                p,
                serde_json::json!({"i": i, "nonce": nonce, "pre_hash": format!("{pre_hash:?}"),
                    "seal_sha256": hex_encode(&sha2_256(&seal)),
                    "verify_pre_hash": format!("{direct:?}"), "rejected_before_recompute_expected": true}),
            ));
        }
        for i in 0..N_DIGEST {
            let (h, body) = make(TS_BASE + u64::from(2_000 + i) * 6_000);
            let (seal, pre_hash) =
                craft_work_mismatch_seal(genesis, &h, nonce_i(40_000 + i));
            assert_eq!(pre_hash, h.hash());
            let direct = gpu_pow::verify_pre_hash(genesis, pre_hash, &seal, EXPERIMENTAL_TARGET);
            assert!(direct.is_err());
            let p = blocks_dir.join(format!("work_mismatch-{i:02}.json"));
            fs::write(&p, block_json(sealed(h, seal.clone()), vec![body])).unwrap();
            illegal.push((
                "work_mismatch".into(),
                format!("work_mismatch-{i:02}"),
                p,
                serde_json::json!({"i": i, "pre_hash": format!("{pre_hash:?}"),
                    "seal_sha256": hex_encode(&sha2_256(&seal)),
                    "verify_pre_hash": format!("{direct:?}"), "rejected_before_recompute_expected": false}),
            ));
        }
        let empty_path = blocks_dir.join("empty.json");
        fs::write(&empty_path, b"").unwrap();

        // 5) runs through the real node import-blocks → BasicQueue
        let mut run_records = Vec::new();
        let mut baseline_ns = Vec::new();
        for b in 0..N_BASELINE {
            let r = run_cli(&import_args(&base, empty_path.to_str().unwrap()));
            let log = runs_dir.join(format!("baseline-empty-{b}.log"));
            fs::write(&log, &r.stderr).unwrap();
            baseline_ns.push(r.wall_ns);
            run_records.push(serde_json::json!({
                "run": format!("baseline-empty-{b}"), "class": "baseline_empty_file",
                "blocks_in_file": 0, "exit": r.exit, "wall_ms": r.wall_ns as f64 / 1e6,
                "log": format!("runs/baseline-empty-{b}.log"),
                "key_lines": interesting_lines(&r.stderr)
            }));
        }

        let legal_run = run_cli(&import_args(&base, legal_path.to_str().unwrap()));
        fs::write(runs_dir.join("legal-batch.log"), &legal_run.stderr).unwrap();
        let legal_imported_line = legal_run
            .stderr
            .lines()
            .find(|l| l.contains("Imported") && l.contains("blocks"))
            .map(|l| l.to_string());
        let legal_ok = legal_run.exit == Some(0)
            && legal_imported_line
                .as_deref()
                .map(|l| l.contains(&format!("Imported {N_LEGAL} blocks")))
                .unwrap_or(false)
            && !legal_run.stderr.contains("error importing");
        run_records.push(serde_json::json!({
            "run": "legal-batch", "class": "legal", "blocks_in_file": N_LEGAL,
            "file": "blocks/legal-batch.json", "file_sha256": sha_file(&legal_path),
            "exit": legal_run.exit, "wall_ms": legal_run.wall_ns as f64 / 1e6,
            "imported_line": legal_imported_line, "accepted_all": legal_ok,
            "log": "runs/legal-batch.log", "key_lines": interesting_lines(&legal_run.stderr)
        }));

        let mut per_class_ns: std::collections::BTreeMap<String, Vec<u128>> = Default::default();
        let mut per_class_ok: std::collections::BTreeMap<String, (u32, u32)> = Default::default();
        for (class, name, path, meta) in &illegal {
            let r = run_cli(&import_args(&base, path.to_str().unwrap()));
            fs::write(runs_dir.join(format!("{name}.log")), &r.stderr).unwrap();
            let invalid_seal = r.stderr.contains(INVALID_SEAL_MARKER);
            let rejected = r.exit != Some(0) && invalid_seal;
            per_class_ns.entry(class.clone()).or_default().push(r.wall_ns);
            let e = per_class_ok.entry(class.clone()).or_default();
            if rejected { e.0 += 1 } else { e.1 += 1 }
            let rec = serde_json::json!({
                "run": name, "class": class, "test": TEST_NAME, "blocks_in_file": 1,
                "file": format!("blocks/{name}.json"), "file_sha256": sha_file(path),
                "expected_error": format!("{INVALID_SEAL_MARKER} (sc_consensus_pow::Error::InvalidSeal, returned only by PowBlockImport::import_block)"),
                "expected_verify_pre_hash_class": class,
                "exit": r.exit, "wall_ms": r.wall_ns as f64 / 1e6,
                "invalid_seal_in_log": invalid_seal, "rejected_by_pow_block_import": rejected,
                "fixture": meta, "log": format!("runs/{name}.log"),
                "key_lines": interesting_lines(&r.stderr)
            });
            fs::write(
                runs_dir.join(format!("{name}.json")),
                serde_json::to_vec_pretty(&rec).unwrap(),
            )
            .unwrap();
            run_records.push(rec);
        }

        // 6) post-check: re-import legal batch must report already-known (no state loss)
        let total_wall_ms = total_started.elapsed().as_millis();
        let baseline_mean_ms =
            baseline_ns.iter().sum::<u128>() as f64 / baseline_ns.len().max(1) as f64 / 1e6;
        let mut classes = serde_json::Map::new();
        for (c, v) in &per_class_ns {
            let (ok, bad) = per_class_ok[c];
            let mut st = stats_ms(v);
            st["rejected_with_invalid_seal"] = serde_json::json!(ok);
            st["not_rejected_as_expected"] = serde_json::json!(bad);
            st["mean_minus_baseline_ms_estimate"] =
                serde_json::json!(st["mean_ms"].as_f64().unwrap() - baseline_mean_ms);
            classes.insert(c.clone(), st);
        }

        let traversed = "eagle-node import-blocks (command.rs Subcommand::ImportBlocks) -> service::new_partial import_queue = sc_consensus::import_queue::BasicQueue(StrictPowVerifier, Box<PowBlockImport>) -> sc_service::chain_ops::import_blocks -> BasicQueue worker import_single_block -> StrictPowVerifier::verify (PowVerifier: seal fetch only) -> PowBlockImport::import_block [check_inherents -> DynamicDifficultyAlgorithm::difficulty -> DynamicDifficultyAlgorithm::verify -> gpu_pow::verify_pre_hash] -> GrandpaBlockImport -> client execute+commit";
        let import = serde_json::json!({
            "test": TEST_NAME,
            "proxy": false,
            "proxy_reason": "not the stage-1 proxy: real release eagle-node binary, real chain spec pow-matrix-block-rewards-local, real RocksDB client, real BasicQueue built by service::new_partial",
            "notNodeImportQueue": false,
            "notNodeImportQueue_reason": "blocks were fed via the node's own import-blocks subcommand into the BasicQueue built by service::new_partial; rejection string 'PoW validation error: invalid seal' is only produced by PowBlockImport::import_block",
            "traversed": traversed,
            "pow_algorithm_in_node": "crate::active_pow_consensus::DynamicDifficultyAlgorithm (active_pow_consensus = gpu_pow_consensus under gpu-pow-matrix-block-rewards-dev); NOT FixedTargetAlgorithm",
            "binary_path": BIN,
            "binary_sha256": bin_sha,
            "chain": CHAIN,
            "genesis_hash": format!("{genesis:?}"),
            "genesis_state_root_matches_raw_spec": true,
            "N_legal": N_LEGAL,
            "N_illegal_by_class": {"above_target": N_ABOVE, "work_mismatch": N_DIGEST},
            "legal_batch": {
                "submitted_in_one_import_blocks_run": N_LEGAL,
                "accepted_all": legal_ok,
                "imported_line": run_records.iter().find(|r| r["run"] == "legal-batch").map(|r| r["imported_line"].clone()),
                "wall_ms": legal_run.wall_ns as f64 / 1e6,
                "wall_minus_baseline_ms_estimate": legal_run.wall_ns as f64 / 1e6 - baseline_mean_ms,
                "inner_import_happened": legal_ok,
                "note": "legal blocks are fully executed block #1 siblings (real state root from chain-spec Wasm, timestamp inherent); all children of genesis"
            },
            "illegal_per_run": classes,
            "illegal_mode": "one import-blocks process per illegal block (import_blocks aborts after the first error), same base-path",
            "baseline_empty_file": stats_ms(&baseline_ns),
            "queue": {
                "kind": "sc_consensus::import_queue::BasicQueue",
                "import_blocks_MAX_PENDING_BLOCKS": 10000,
                "import_blocks_poll_DELAY_TIME_ms": 200,
                "rate_limit": "none in import-blocks path; process wall time is dominated by node/DB/wasm startup (see baseline_empty_file) and the 200 ms poll delay",
                "behind_blocks": null
            },
            "wall_ms_total_test": total_wall_ms,
            "timing_note": "wall_ms is per-process wall clock of the release import-blocks CLI including startup; it is NOT a miner C/s figure and NOT a per-verify cost (per-verify cost remains release-import/v2-2x16/summary.json)",
            "inner_import_for_illegal": "not reached: PowBlockImport::import_block returns InvalidSeal before calling inner GrandpaBlockImport",
            "gpu_used": false, "mine_called": false,
            "mapping": "i32::from(b % 129) - 64",
            "identity_notes": IDENTITY_NOTES,
            "timestamp_utc": now_rfc3339()
        });
        fs::write(dir.join("import.json"), serde_json::to_vec_pretty(&import).unwrap()).unwrap();
        fs::write(dir.join("runs.json"), serde_json::to_vec_pretty(&run_records).unwrap()).unwrap();
        fs::write(dir.join("fixtures.json"), serde_json::to_vec_pretty(&fixtures).unwrap()).unwrap();

        let env = serde_json::json!({
            "test": TEST_NAME,
            "start_command": format!("cargo test --release -p solvyren-node --features gpu-pow-matrix-block-rewards-dev --bin solvyren-node phase7_import_queue -- --ignored --nocapture"),
            "cpu_model": cpu_model(), "logical_cpus": logical_cpus(), "sha_ni": sha_ni(),
            "memory_bytes": mem_bytes(), "rustc": rustc_version(), "profile": "release",
            "binary_path": BIN, "binary_sha256": binary_sha256(),
            "features": "gpu-pow-matrix-block-rewards-dev", "svy_matrix_live": false,
            "seed": SEED_HEX,
            "seed_usage": "nonce_i = SHA256(seed||\"nonce\"||i_le)[0..8] start points; certificate challenge is ALWAYS challenge_from_pre_hash(genesis, header_pre_hash) where genesis is the exported chain-spec genesis hash",
            "shape": SHAPE, "a": 2, "b": 16, "k": 2048, "dimensions": "2x2048x16",
            "mapping": "i32::from(b % 129) - 64", "identity_notes": IDENTITY_NOTES,
            "workAlgorithm": "integer-matrix-2x2048x16",
            "certificate_bytes": CERTIFICATE_BYTES, "certificate_version": gpu_pow::VERSION,
            "target": hex_encode(&EXPERIMENTAL_TARGET), "difficulty": EXPERIMENTAL_DIFFICULTY,
            "chain": CHAIN,
            "base_path": base,
            "node_started": false,
            "node_started_note": "no long-running node and no network/RPC: only offline CLI subcommands (build-spec, export-blocks, import-blocks) on a fresh base-path; no ports opened",
            "client_construction": "release eagle-node import-blocks -> service::new_partial (real TFullClient + RocksDB + BasicQueue)",
            "import_args": import_args("<base>", "<file>"),
            "timestamp_utc": now_rfc3339()
        });
        fs::write(dir.join("environment.json"), serde_json::to_vec_pretty(&env).unwrap()).unwrap();

        eprintln!("phase7 import-queue: legal_ok={legal_ok} classes={classes:?} total_ms={total_wall_ms}");
        assert!(legal_ok, "legal batch must import");
        for (c, (_, bad)) in &per_class_ok {
            assert_eq!(*bad, 0, "class {c} had non-InvalidSeal outcomes");
        }
    }
}
