//! MX10 evaluation-only GPU path (CUDA, see `gpu_pow_mx10_gpu.cu`).
//!
//! Evaluation harness only: not wired into chain spec, consensus, import,
//! mining or `workAlgorithm`. The CUDA program is a fresh MX10 implementation
//! (BLAKE2s input derivation, int8 dp4a n x 2048 matrix product, SHA-256
//! digests); it does not call or reuse the V2 `gpu_pow` kernel or `mine()`.
//! This module only builds the CUDA program and cross-checks it bit-for-bit
//! against the CPU reference `gpu_pow_mx10::compute`.
// The original cross-check stays test-only via #[test]. The unchanged CUDA
// batch kernel is now exposed to a separate bounded isolated author adapter.
#[cfg(unix)]
#[path = "mx10_gpu_author.rs"]
mod author;
#[cfg(unix)]
pub use author::GpuSession;

// CUDA authoring is intentionally unavailable on the Windows follower.  Keep
// the command's automatic CPU fallback compilable without shipping a second
// GPU implementation or enabling any authoring on the deployed node.
#[cfg(windows)]
pub struct GpuSession;

#[cfg(windows)]
impl GpuSession {
    pub fn open(_: &std::path::Path) -> Result<Self, String> {
        Err("MX10 CUDA authoring is unavailable in the Windows follower build".into())
    }

    pub fn batch(
        &mut self,
        _: [u8; 32],
        _: u64,
        _: u32,
    ) -> Result<Vec<crate::gpu_pow_mx10::Certificate>, String> {
        Err("MX10 CUDA authoring is unavailable in the Windows follower build".into())
    }

    pub fn finish(self) -> Result<serde_json::Value, String> {
        Err("MX10 CUDA authoring is unavailable in the Windows follower build".into())
    }
}

use crate::gpu_pow_mx10::{compute, ticket, EvalShape, INNER_DIM, N_CANDIDATES};
use std::io::Write;
use std::path::PathBuf;
use std::process::{Command, Stdio};

pub const CUDA_SOURCE: &str = include_str!("gpu_pow_mx10_gpu.cu");
pub const GPU_UUID: &str = "GPU-a023db6b-556b-d862-514d-1413ba7b7cc6";
pub const GPU_LOCK: &str =
    "/var/lib/eagle-ai/gpu-pow/GPU-a023db6b-556b-d862-514d-1413ba7b7cc6.lock";

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Build the CUDA program with nvcc into `<workspace>/target/mx10-gpu/mx10_gpu`.
pub fn build_gpu_binary() -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../target/mx10-gpu");
    std::fs::create_dir_all(&dir).expect("mkdir mx10-gpu");
    let src = dir.join("gpu_pow_mx10_gpu.cu");
    std::fs::write(&src, CUDA_SOURCE).expect("write cu");
    let bin = dir.join("mx10_gpu");
    let status = Command::new("nvcc")
        .args(["-O3", "-arch=sm_86", "-o"])
        .arg(&bin)
        .arg(&src)
        .status()
        .expect("nvcc not available");
    assert!(status.success(), "nvcc failed");
    bin
}

fn run_locked(bin: &PathBuf, args: &[String], stdin: &str) -> String {
    let mut child = Command::new("flock")
        .args(["-w", "120", GPU_LOCK])
        .arg(bin)
        .args(args)
        .env("CUDA_VISIBLE_DEVICES", GPU_UUID)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .spawn()
        .expect("spawn mx10_gpu");
    child.stdin.take().unwrap().write_all(stdin.as_bytes()).unwrap();
    let out = child.wait_with_output().expect("mx10_gpu");
    assert!(out.status.success(), "mx10_gpu failed: {:?}", out.status);
    String::from_utf8(out.stdout).unwrap()
}

fn check_lines(out: &str, challenge: [u8; 32], log: &mut Vec<String>, mode: &str) -> usize {
    let mut count = 0;
    for line in out.lines() {
        let f: Vec<&str> = line.split_whitespace().collect();
        assert_eq!(f.len(), 4, "bad gpu line {line}");
        let n: u32 = f[0].parse().unwrap();
        let nonce: u64 = f[1].parse().unwrap();
        let cert = compute(EvalShape::new(n, INNER_DIM).unwrap(), challenge, nonce);
        let ok = hex(&cert.input_digest) == f[2] && hex(&cert.output_digest) == f[3];
        log.push(format!(
            "{{\"mode\":\"{mode}\",\"n\":{n},\"nonce\":{nonce},\"challenge\":\"{}\",\"gpu_input_digest\":\"{}\",\"gpu_output_digest\":\"{}\",\"cpu_input_digest\":\"{}\",\"cpu_output_digest\":\"{}\",\"cpu_ticket\":\"{}\",\"match\":{ok}}}",
            hex(&challenge), f[2], f[3], hex(&cert.input_digest), hex(&cert.output_digest), hex(&ticket(&cert))
        ));
        assert!(ok, "GPU/CPU mismatch n={n} nonce={nonce}");
        count += 1;
    }
    count
}

/// Bit-exact cross-check of the CUDA MX10 path against CPU `compute()`.
/// Covers all four n, fixed-vector (challenge 0, nonce 7) and several other
/// nonces/challenges, both single-nonce and the batched launch used by bench.
#[test]
fn mx10_gpu_matches_cpu_compute_bit_exact() {
    let bin = build_gpu_binary();
    let challenges: [[u8; 32]; 2] = [[0u8; 32], {
        let mut c = [0u8; 32];
        for (i, b) in c.iter_mut().enumerate() {
            *b = (i as u8).wrapping_mul(37).wrapping_add(0xa5);
        }
        c
    }];
    let nonces: [u64; 6] = [0, 1, 7, 255, 0x0123_4567_89ab_cdef, u64::MAX];
    let mut log = Vec::new();
    let mut total = 0;
    for ch in challenges {
        let mut input = String::new();
        for n in N_CANDIDATES {
            for nonce in nonces {
                input.push_str(&format!("{n} {nonce} {}\n", hex(&ch)));
            }
        }
        let out = run_locked(&bin, &["vectors".to_string()], &input);
        total += check_lines(&out, ch, &mut log, "single");
        for n in N_CANDIDATES {
            let args = ["batch".to_string(), n.to_string(), "1000000".to_string(), "24".to_string(), hex(&ch)];
            let out = run_locked(&bin, &args, "");
            total += check_lines(&out, ch, &mut log, "batch24");
        }
    }
    assert_eq!(total, 2 * (4 * 6 + 4 * 24));
    // Fixed vectors (challenge 0, nonce 7) must be present and match the pinned hex.
    assert!(log.iter().any(|l| l.contains("\"n\":128,\"nonce\":7,\"challenge\":\"0000") && l.contains("19b8feabd0a53fe5")));
    if let Ok(path) = std::env::var("MX10_GPU_CROSS_LOG") {
        std::fs::write(path, log.join("\n") + "\n").expect("write cross log");
    }
}
