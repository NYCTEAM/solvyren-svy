//! Experimental V2 open-work ticket primitives. The opt-in PoW import path
//! uses the matching `PowAlgorithm` adapter and fixed unit chainwork, while
//! authoring, live two-node validation and difficulty adjustment remain
//! incomplete. The bounded matrix is efficiently computable on CPU and its
//! verification proves neither physical GPU execution nor complete consensus.

use blake2::{Blake2s256, Digest as BlakeDigest};
use eagle_runtime::Header;
use sp_core::{hashing::sha2_256, H256};
use sp_runtime::{
    codec::{Decode, Encode},
    generic::DigestItem,
    traits::Header as HeaderT,
};

/// Experimental algorithm and encoding version.
pub const VERSION: u8 = 2;
/// Fixed SCALE size of the V2 certificate.
pub const CERTIFICATE_BYTES: usize = 105;
const CHALLENGE_DOMAIN: &[u8] = b"EAGW-POW-V2-CHALLENGE\0";
const INPUT_DOMAIN: &[u8] = b"EAGW-POW-V2-INPUT\0";
const COMMIT_DOMAIN: &[u8] = b"EAGW-POW-V2-COMMIT\0";
const TICKET_DOMAIN: &[u8] = b"EAGW-POW-V2-TICKET\0";
const K: usize = 2048;
const A_ROWS: usize = 2;
const B_ROWS: usize = 16;
const ELEMENTS: usize = (A_ROWS + B_ROWS) * K;
const OUTPUTS: usize = A_ROWS * B_ROWS;

/// A fixed-size, independently recomputable ticket claim. Difficulty is not
/// stored here: callers must obtain the target from trusted consensus context.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    /// Encoding and algorithm version.
    pub version: u8,
    /// Challenge binding the chain identity and canonical pre-seal header.
    pub challenge: [u8; 32],
    /// Search nonce, included in matrix input generation.
    pub nonce: u64,
    /// SHA-256 of all signed little-endian i32 inputs.
    pub input_digest: [u8; 32],
    /// SHA-256 of 32 signed little-endian i32 dot products.
    pub output_digest: [u8; 32],
}

/// Rejection reasons for the V2 primitives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The supplied header already has a consensus seal.
    HeaderAlreadySealed,
    /// The certificate is malformed, not exactly 105 bytes, or has trailing data.
    InvalidEncoding,
    /// The algorithm version is unsupported.
    UnsupportedVersion,
    /// Certificate does not bind this genesis and candidate.
    ChallengeMismatch,
    /// The matrix input or output digest differs from CPU recomputation.
    WorkMismatch,
    /// The verified ticket exceeds the caller-provided target.
    AboveTarget,
}

impl Certificate {
    /// Encode as a fixed-length SCALE tuple, with nonce little-endian.
    pub fn encode_fixed(&self) -> Vec<u8> {
        (
            self.version,
            self.challenge,
            self.nonce,
            self.input_digest,
            self.output_digest,
        )
            .encode()
    }

    /// Parse exactly one certificate without allocating based on its input.
    pub fn decode_fixed(bytes: &[u8]) -> Result<Self, Error> {
        if bytes.len() != CERTIFICATE_BYTES {
            return Err(Error::InvalidEncoding);
        }
        let mut data = bytes;
        let (version, challenge, nonce, input_digest, output_digest) =
            <(u8, [u8; 32], u64, [u8; 32], [u8; 32])>::decode(&mut data)
                .map_err(|_| Error::InvalidEncoding)?;
        if !data.is_empty() {
            return Err(Error::InvalidEncoding);
        }
        if version != VERSION {
            return Err(Error::UnsupportedVersion);
        }
        Ok(Self {
            version,
            challenge,
            nonce,
            input_digest,
            output_digest,
        })
    }
}

/// Bind genesis and a trusted hash of the complete unsealed header. Parent,
/// height, roots and digests are committed by that header hash. A caller must
/// calculate `pre_hash` from the actual unsealed candidate, not from a miner's
/// unauthenticated claim. `PowBlockImport` supplies its own header hash.
pub fn challenge_from_pre_hash(genesis: H256, pre_hash: H256) -> [u8; 32] {
    let mut message = Vec::with_capacity(CHALLENGE_DOMAIN.len() + 65);
    message.extend_from_slice(CHALLENGE_DOMAIN);
    message.push(VERSION);
    message.extend_from_slice(genesis.as_bytes());
    message.extend_from_slice(pre_hash.as_bytes());
    sha2_256(&message)
}

/// Hash an unsealed header to obtain the canonical challenge. Every seal is
/// rejected to prevent ambiguous candidate representations.
pub fn challenge_for_header(genesis: H256, header: &Header) -> Result<[u8; 32], Error> {
    if header
        .digest()
        .logs
        .iter()
        .any(|item| matches!(item, DigestItem::Seal(_, _)))
    {
        return Err(Error::HeaderAlreadySealed);
    }
    Ok(challenge_from_pre_hash(genesis, header.hash()))
}

#[cfg(test)]
thread_local! {
    static COMPUTE_INVOCATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn compute_invocations() -> u64 {
    COMPUTE_INVOCATIONS.with(|c| c.get())
}

/// Derive the V2 input and compute all dot products. Allocation and running
/// time are bounded by fixed protocol dimensions; nonce changes every matrix.
pub fn compute(challenge: [u8; 32], nonce: u64) -> Certificate {
    #[cfg(test)]
    COMPUTE_INVOCATIONS.with(|c| c.set(c.get().saturating_add(1)));
    let mut values = Vec::with_capacity(ELEMENTS);
    let mut input = Vec::with_capacity(ELEMENTS * 4);
    for counter in 0..(ELEMENTS / 32) as u32 {
        let mut hash = Blake2s256::new();
        hash.update(INPUT_DOMAIN);
        hash.update(challenge);
        hash.update(nonce.to_le_bytes());
        hash.update(counter.to_le_bytes());
        for b in hash.finalize() {
            let value = i32::from(b % 129) - 64;
            values.push(value);
            input.extend_from_slice(&value.to_le_bytes());
        }
    }
    let mut output = [0u8; OUTPUTS * 4];
    for row in 0..A_ROWS {
        for col in 0..B_ROWS {
            let mut dot = 0i32;
            for k in 0..K {
                dot += values[row * K + k] * values[(A_ROWS + col) * K + k];
            }
            let index = row * B_ROWS + col;
            output[index * 4..index * 4 + 4].copy_from_slice(&dot.to_le_bytes());
        }
    }
    Certificate {
        version: VERSION,
        challenge,
        nonce,
        input_digest: sha2_256(&input),
        output_digest: sha2_256(&output),
    }
}

/// Derive a ticket, returned as an unsigned 256-bit big-endian byte string.
/// Only call this after independently verifying the certificate's work.
pub fn ticket(certificate: &Certificate) -> [u8; 32] {
    let mut commitment_data = Vec::with_capacity(COMMIT_DOMAIN.len() + 104);
    commitment_data.extend_from_slice(COMMIT_DOMAIN);
    commitment_data.extend_from_slice(&certificate.challenge);
    commitment_data.extend_from_slice(&certificate.nonce.to_le_bytes());
    commitment_data.extend_from_slice(&certificate.input_digest);
    commitment_data.extend_from_slice(&certificate.output_digest);
    let commitment = sha2_256(&commitment_data);
    let mut ticket_data = Vec::with_capacity(TICKET_DOMAIN.len() + 136);
    ticket_data.extend_from_slice(TICKET_DOMAIN);
    ticket_data.extend_from_slice(&certificate.challenge);
    ticket_data.extend_from_slice(&certificate.nonce.to_le_bytes());
    ticket_data.extend_from_slice(&certificate.input_digest);
    ticket_data.extend_from_slice(&certificate.output_digest);
    ticket_data.extend_from_slice(&commitment);
    sha2_256(&ticket_data)
}

/// Check the supplied certificate against the trusted chain context and target.
/// Mandatory order: decode and context, then a ticket from claimed fields
/// compared with the trusted parent target, then a full `compute()` and digest
/// check only if that claimed ticket is within target. A ticket prefilter must
/// not replace recomputation. `target` must be derived independently from the
/// parent consensus rules. It is compared as a 32-byte big-endian unsigned
/// integer, inclusive.
pub fn verify_pre_hash(
    genesis: H256,
    pre_hash: H256,
    encoded: &[u8],
    target: [u8; 32],
) -> Result<[u8; 32], Error> {
    let certificate = Certificate::decode_fixed(encoded)?;
    let expected = challenge_from_pre_hash(genesis, pre_hash);
    if certificate.challenge != expected {
        return Err(Error::ChallengeMismatch);
    }
    let ticket = ticket(&certificate);
    if ticket > target {
        return Err(Error::AboveTarget);
    }
    let calculated = compute(expected, certificate.nonce);
    if certificate.input_digest != calculated.input_digest
        || certificate.output_digest != calculated.output_digest
    {
        return Err(Error::WorkMismatch);
    }
    Ok(ticket)
}

/// Convenience wrapper which rejects sealed headers before verification.
pub fn verify(
    genesis: H256,
    header: &Header,
    encoded: &[u8],
    target: [u8; 32],
) -> Result<[u8; 32], Error> {
    challenge_for_header(genesis, header)?;
    verify_pre_hash(genesis, header.hash(), encoded, target)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_runtime::generic::Digest;
    fn header() -> Header {
        Header::new(
            42,
            H256::repeat_byte(1),
            H256::repeat_byte(2),
            H256::repeat_byte(3),
            Digest::default(),
        )
    }
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn nonce_changes_matrix_and_output() {
        let first = compute([0; 32], 7);
        let second = compute([0; 32], 8);
        // Vector independently generated with Python hashlib and struct.
        assert_eq!(
            hex(&first.input_digest),
            "760f1de584e4e26ba35574e417a82481755ab1bf5246cf572a3a34d8498be735"
        );
        assert_eq!(
            hex(&first.output_digest),
            "be45b788b1a80e670c21d9a2ea8ba139ba33beb703392483d08ed97c890b0e20"
        );
        assert_eq!(
            hex(&ticket(&first)),
            "83c78c78dda8cb7b080423ee7dfcb5c34240dfd4125e78c3518281e506a4489b"
        );
        assert_ne!(first.input_digest, second.input_digest);
        assert_ne!(first.output_digest, second.output_digest);
        assert_eq!(first.encode_fixed().len(), CERTIFICATE_BYTES);
        assert_eq!(Certificate::decode_fixed(&first.encode_fixed()), Ok(first));
    }

    #[test]
    fn header_chain_nonce_encoding_and_target_are_checked() {
        let genesis = H256::repeat_byte(7);
        let h = header();
        assert_eq!(
            challenge_for_header(genesis, &h).unwrap(),
            challenge_from_pre_hash(genesis, h.hash())
        );
        let cert = compute(challenge_for_header(genesis, &h).unwrap(), 5);
        let bytes = cert.encode_fixed();
        let result = ticket(&cert);
        assert_eq!(verify(genesis, &h, &bytes, result), Ok(result));
        assert_eq!(verify(genesis, &h, &bytes, [255; 32]), Ok(result));
        assert_eq!(
            verify(genesis, &h, &bytes, [0; 32]),
            Err(Error::AboveTarget)
        );
        let mut below = result;
        let i = below.iter().rposition(|x| *x != 0).unwrap();
        below[i] -= 1;
        for b in &mut below[i + 1..] {
            *b = 255;
        }
        assert_eq!(verify(genesis, &h, &bytes, below), Err(Error::AboveTarget));
        assert_eq!(
            verify(H256::repeat_byte(8), &h, &bytes, [255; 32]),
            Err(Error::ChallengeMismatch)
        );
        let mut changed = h.clone();
        changed.set_parent_hash(H256::repeat_byte(9));
        assert_eq!(
            verify(genesis, &changed, &bytes, [255; 32]),
            Err(Error::ChallengeMismatch)
        );
        changed = h.clone();
        changed.set_number(43);
        assert_eq!(
            verify(genesis, &changed, &bytes, [255; 32]),
            Err(Error::ChallengeMismatch)
        );
        changed = h.clone();
        changed.set_state_root(H256::repeat_byte(9));
        assert_eq!(
            verify(genesis, &changed, &bytes, [255; 32]),
            Err(Error::ChallengeMismatch)
        );
        changed = h.clone();
        changed.set_extrinsics_root(H256::repeat_byte(9));
        assert_eq!(
            verify(genesis, &changed, &bytes, [255; 32]),
            Err(Error::ChallengeMismatch)
        );
        changed = h.clone();
        changed.digest_mut().push(DigestItem::Other(vec![42]));
        assert_ne!(h.hash(), changed.hash());
        assert_ne!(
            challenge_for_header(genesis, &h),
            challenge_for_header(genesis, &changed)
        );
        changed = h.clone();
        changed
            .digest_mut()
            .push(DigestItem::Seal(*b"TEST", vec![]));
        assert_eq!(
            challenge_for_header(genesis, &changed),
            Err(Error::HeaderAlreadySealed)
        );
        let mut tampered = cert.clone();
        tampered.nonce += 1;
        assert_eq!(
            verify(genesis, &h, &tampered.encode_fixed(), [255; 32]),
            Err(Error::WorkMismatch)
        );
        let mut truncated = bytes.clone();
        truncated.pop();
        assert_eq!(
            Certificate::decode_fixed(&truncated),
            Err(Error::InvalidEncoding)
        );
        let mut trailing = bytes.clone();
        trailing.push(0);
        assert_eq!(
            Certificate::decode_fixed(&trailing),
            Err(Error::InvalidEncoding)
        );
        let mut unsupported = bytes;
        unsupported[0] = 3;
        assert_eq!(
            Certificate::decode_fixed(&unsupported),
            Err(Error::UnsupportedVersion)
        );
    }

    fn just_below(ticket: [u8; 32]) -> [u8; 32] {
        let mut below = ticket;
        let i = below
            .iter()
            .rposition(|x| *x != 0)
            .expect("ticket is unexpectedly zero");
        below[i] -= 1;
        for b in &mut below[i + 1..] {
            *b = 255;
        }
        below
    }

    fn reject_reason(err: Error) -> &'static str {
        match err {
            Error::HeaderAlreadySealed => "header_already_sealed",
            Error::InvalidEncoding => "invalid_encoding",
            Error::UnsupportedVersion => "unsupported_version",
            Error::ChallengeMismatch => "challenge_mismatch",
            Error::WorkMismatch => "work_mismatch",
            Error::AboveTarget => "above_target",
        }
    }

    fn phase3_profile() -> &'static str {
        if cfg!(debug_assertions) {
            "debug"
        } else {
            "release"
        }
    }

    fn rustc_version_string() -> String {
        std::process::Command::new("rustc")
            .arg("--version")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .map(|s| s.trim().to_string())
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn cpu_model_string() -> String {
        std::fs::read_to_string("/proc/cpuinfo")
            .ok()
            .and_then(|text| {
                text.lines()
                    .find(|line| line.starts_with("model name"))
                    .map(|line| line.split(':').nth(1).unwrap_or("").trim().to_string())
            })
            .filter(|s| !s.is_empty())
            .unwrap_or_else(|| "unknown".to_string())
    }

    fn json_escape(s: &str) -> String {
        let mut out = String::new();
        for c in s.chars() {
            match c {
                '"' => out.push_str("\\\""),
                '\\' => out.push_str("\\\\"),
                '\n' => out.push_str("\\n"),
                '\r' => out.push_str("\\r"),
                '\t' => out.push_str("\\t"),
                c if (c as u32) < 0x20 => out.push_str(&format!("\\u{code:04x}", code = c as u32)),
                c => out.push(c),
            }
        }
        out
    }

    struct OrderCase {
        case: &'static str,
        before: u64,
        after: u64,
        rejected_before_recompute: bool,
        result: String,
        reject_reason: String,
        elapsed_ns: u128,
    }

    fn t_verify_ref_p50_ns(genesis: H256, pre_hash: H256, encoded: &[u8]) -> u128 {
        let target = [255u8; 32];
        let _ = verify_pre_hash(genesis, pre_hash, encoded, target);
        let mut samples = Vec::with_capacity(10);
        for _ in 0..10 {
            let started = std::time::Instant::now();
            let _ = verify_pre_hash(genesis, pre_hash, encoded, target);
            samples.push(started.elapsed().as_nanos());
        }
        samples.sort_unstable();
        samples[(samples.len() - 1) / 2]
    }

    fn write_order_json(test: &str, cases: &[OrderCase], t_verify_ref_p50_ns: u128) {
        let Some(dir) = std::env::var_os("PHASE3_ORDER_OUT") else {
            return;
        };
        std::fs::create_dir_all(&dir).expect("create PHASE3_ORDER_OUT");
        let path = std::path::Path::new(&dir).join(format!("{test}.json"));
        let first = &cases[0];
        let elapsed_ratio = if t_verify_ref_p50_ns == 0 {
            0.0
        } else {
            first.elapsed_ns as f64 / t_verify_ref_p50_ns as f64
        };
        let mut sub = String::new();
        for (i, c) in cases.iter().enumerate() {
            if i > 0 {
                sub.push(',');
            }
            let ratio = if t_verify_ref_p50_ns == 0 {
                0.0
            } else {
                c.elapsed_ns as f64 / t_verify_ref_p50_ns as f64
            };
            sub.push_str(&format!(
                "{{\n      \"case\": \"{case}\",\n      \"compute_invocations_before\": {before},\n      \"compute_invocations_after\": {after},\n      \"rejected_before_recompute\": {rejected},\n      \"result\": \"{result}\",\n      \"reject_reason\": {reason},\n      \"elapsed_ns\": {elapsed},\n      \"elapsed_ratio\": {ratio:.8}\n    }}",
                case = json_escape(c.case),
                before = c.before,
                after = c.after,
                rejected = if c.rejected_before_recompute { "true" } else { "false" },
                result = json_escape(&c.result),
                reason = if c.reject_reason.is_empty() {
                    "null".to_string()
                } else {
                    format!("\"{}\"", json_escape(&c.reject_reason))
                },
                elapsed = c.elapsed_ns,
                ratio = ratio
            ));
        }
        let body = format!(
            "{{\n  \"test\": \"{test}\",\n  \"function_under_test\": \"gpu_pow::verify_pre_hash\",\n  \"file\": \"chain-core/node/src/gpu_pow.rs\",\n  \"shape\": \"v2-2x16\",\n  \"a\": 2,\n  \"b\": 16,\n  \"k\": 2048,\n  \"dimensions\": \"2x2048x16\",\n  \"order\": \"decode_context_then_claimed_ticket_then_recompute\",\n  \"case\": \"{case}\",\n  \"compute_invocations_before\": {before},\n  \"compute_invocations_after\": {after},\n  \"rejected_before_recompute\": {rejected},\n  \"result\": \"{result}\",\n  \"reject_reason\": {reason},\n  \"elapsed_ns\": {elapsed},\n  \"t_verify_ref_p50_ns\": {tref},\n  \"elapsed_ratio\": {ratio:.8},\n  \"profile\": \"{profile}\",\n  \"rust<c>\": \"{rustc}\",\n  \"cpu_model\": \"{cpu}\",\n  \"mapping\": \"i32::from(b % 129) - 64\",\n  \"identity_notes\": \"not MX10; mapping is i32::from(b % 129) - 64, not i32(b)-128; certificate 0x02 is current node encoding only and does not mean a freeze still uses version 2\",\n  \"subcases\": [\n    {sub}\n  ]\n}}\n",
            test = json_escape(test),
            case = json_escape(first.case),
            before = first.before,
            after = first.after,
            rejected = if first.rejected_before_recompute { "true" } else { "false" },
            result = json_escape(&first.result),
            reason = if first.reject_reason.is_empty() {
                "null".to_string()
            } else {
                format!("\"{}\"", json_escape(&first.reject_reason))
            },
            elapsed = first.elapsed_ns,
            tref = t_verify_ref_p50_ns,
            ratio = elapsed_ratio,
            profile = json_escape(phase3_profile()),
            rustc = json_escape(&rustc_version_string()),
            cpu = json_escape(&cpu_model_string()),
            sub = sub
        );
        // fix accidental rustc key split
        let body = body.replace("\"rust<c>\":", "\"rustc\":");
        std::fs::write(&path, body).expect("write order-test json");
    }

    fn record_call(
        case: &'static str,
        before: u64,
        after: u64,
        outcome: Result<[u8; 32], Error>,
        elapsed_ns: u128,
    ) -> OrderCase {
        match outcome {
            Ok(_) => OrderCase {
                case,
                before,
                after,
                rejected_before_recompute: false,
                result: "ok".to_string(),
                reject_reason: String::new(),
                elapsed_ns,
            },
            Err(err) => OrderCase {
                case,
                before,
                after,
                rejected_before_recompute: after == before,
                result: "err".to_string(),
                reject_reason: reject_reason(err).to_string(),
                elapsed_ns,
            },
        }
    }

    fn valid_v2_fixture() -> (H256, H256, Certificate, Vec<u8>, [u8; 32]) {
        let genesis = H256::repeat_byte(7);
        let h = header();
        let pre_hash = h.hash();
        let expected = challenge_from_pre_hash(genesis, pre_hash);
        let cert = compute(expected, 5);
        let bytes = cert.encode_fixed();
        let claimed = ticket(&cert);
        (genesis, pre_hash, cert, bytes, claimed)
    }

    #[test]
    fn verify_pre_hash_rejects_above_target_before_recompute() {
        let (genesis, pre_hash, _cert, bytes, claimed) = valid_v2_fixture();
        let t_ref = t_verify_ref_p50_ns(genesis, pre_hash, &bytes);
        let mut recorded = Vec::new();
        for (case, target) in [
            ("target_all_zero", [0u8; 32]),
            ("target_just_below_ticket", just_below(claimed)),
        ] {
            let before = compute_invocations();
            let started = std::time::Instant::now();
            let outcome = verify_pre_hash(genesis, pre_hash, &bytes, target);
            let elapsed_ns = started.elapsed().as_nanos();
            let after = compute_invocations();
            assert_eq!(outcome, Err(Error::AboveTarget), "{case}");
            assert_eq!(after, before, "{case} must reject before recompute");
            assert!(
                elapsed_ns < t_ref / 10,
                "{case} elapsed_ns={elapsed_ns} t_verify_ref_p50_ns={t_ref}"
            );
            recorded.push(record_call(case, before, after, outcome, elapsed_ns));
            assert!(recorded.last().unwrap().rejected_before_recompute);
        }
        write_order_json(
            "verify_pre_hash_rejects_above_target_before_recompute",
            &recorded,
            t_ref,
        );
    }

    #[test]
    fn verify_pre_hash_rejects_work_mismatch_after_ticket_passes() {
        let (genesis, pre_hash, _cert, bytes, _claimed) = valid_v2_fixture();
        let t_ref = t_verify_ref_p50_ns(genesis, pre_hash, &bytes);
        let mut recorded = Vec::new();
        for (case, offset) in [("xor_input_digest_offset_41", 41usize), ("xor_output_digest_offset_73", 73usize)]
        {
            let mut tampered = bytes.clone();
            tampered[offset] ^= 0xff;
            let decoded = Certificate::decode_fixed(&tampered).expect("tampered certificate still decodes");
            let claimed_ticket = ticket(&decoded);
            let before = compute_invocations();
            let started = std::time::Instant::now();
            let outcome = verify_pre_hash(genesis, pre_hash, &tampered, claimed_ticket);
            let elapsed_ns = started.elapsed().as_nanos();
            let after = compute_invocations();
            assert_eq!(outcome, Err(Error::WorkMismatch), "{case}");
            assert_eq!(after, before + 1, "{case} must fully recompute once");
            assert!(
                elapsed_ns > t_ref / 2,
                "{case} elapsed_ns={elapsed_ns} t_verify_ref_p50_ns={t_ref}"
            );
            recorded.push(record_call(case, before, after, outcome, elapsed_ns));
            assert!(!recorded.last().unwrap().rejected_before_recompute);
        }
        write_order_json(
            "verify_pre_hash_rejects_work_mismatch_after_ticket_passes",
            &recorded,
            t_ref,
        );
    }

    #[test]
    fn verify_pre_hash_rejects_invalid_encoding_before_recompute() {
        let (genesis, pre_hash, _cert, bytes, _claimed) = valid_v2_fixture();
        let t_ref = t_verify_ref_p50_ns(genesis, pre_hash, &bytes);
        let mut recorded = Vec::new();
        for (case, payload) in {
            let mut truncated = bytes.clone();
            truncated.pop();
            let mut trailing = bytes.clone();
            trailing.push(0);
            [
                ("truncated", truncated),
                ("trailing_byte", trailing),
            ]
        } {
            let before = compute_invocations();
            let started = std::time::Instant::now();
            let outcome = verify_pre_hash(genesis, pre_hash, &payload, [255u8; 32]);
            let elapsed_ns = started.elapsed().as_nanos();
            let after = compute_invocations();
            assert_eq!(outcome, Err(Error::InvalidEncoding), "{case}");
            assert_eq!(after, before, "{case} must reject before recompute");
            recorded.push(record_call(case, before, after, outcome, elapsed_ns));
            assert!(recorded.last().unwrap().rejected_before_recompute);
        }
        write_order_json(
            "verify_pre_hash_rejects_invalid_encoding_before_recompute",
            &recorded,
            t_ref,
        );
    }

    #[test]
    fn verify_pre_hash_rejects_unsupported_version_before_recompute() {
        let (genesis, pre_hash, _cert, bytes, _claimed) = valid_v2_fixture();
        let t_ref = t_verify_ref_p50_ns(genesis, pre_hash, &bytes);
        let mut unsupported = bytes.clone();
        unsupported[0] = 3;
        let before = compute_invocations();
        let started = std::time::Instant::now();
        let outcome = verify_pre_hash(genesis, pre_hash, &unsupported, [255u8; 32]);
        let elapsed_ns = started.elapsed().as_nanos();
        let after = compute_invocations();
        assert_eq!(outcome, Err(Error::UnsupportedVersion));
        assert_eq!(after, before, "unsupported version must reject before recompute");
        let recorded = [record_call(
            "unsupported_version_byte_3",
            before,
            after,
            outcome,
            elapsed_ns,
        )];
        assert!(recorded[0].rejected_before_recompute);
        write_order_json(
            "verify_pre_hash_rejects_unsupported_version_before_recompute",
            &recorded,
            t_ref,
        );
    }

    #[test]
    fn verify_pre_hash_rejects_challenge_mismatch_before_recompute() {
        let (genesis, pre_hash, _cert, bytes, _claimed) = valid_v2_fixture();
        let t_ref = t_verify_ref_p50_ns(genesis, pre_hash, &bytes);
        let before = compute_invocations();
        let started = std::time::Instant::now();
        let outcome = verify_pre_hash(H256::repeat_byte(8), pre_hash, &bytes, [255u8; 32]);
        let elapsed_ns = started.elapsed().as_nanos();
        let after = compute_invocations();
        assert_eq!(outcome, Err(Error::ChallengeMismatch));
        assert_eq!(after, before, "challenge mismatch must reject before recompute");
        let recorded = [record_call(
            "wrong_genesis",
            before,
            after,
            outcome,
            elapsed_ns,
        )];
        assert!(recorded[0].rejected_before_recompute);
        let _ = genesis;
        write_order_json(
            "verify_pre_hash_rejects_challenge_mismatch_before_recompute",
            &recorded,
            t_ref,
        );
    }

    #[test]
    fn verify_pre_hash_accepts_matching_work_with_single_recompute() {
        let (genesis, pre_hash, _cert, bytes, claimed) = valid_v2_fixture();
        let t_ref = t_verify_ref_p50_ns(genesis, pre_hash, &bytes);
        let before = compute_invocations();
        let started = std::time::Instant::now();
        let outcome = verify_pre_hash(genesis, pre_hash, &bytes, claimed);
        let elapsed_ns = started.elapsed().as_nanos();
        let after = compute_invocations();
        assert_eq!(outcome, Ok(claimed));
        assert_eq!(after, before + 1, "matching work must recompute exactly once");
        write_order_json(
            "verify_pre_hash_accepts_matching_work_with_single_recompute",
            &[record_call("matching_work", before, after, outcome, elapsed_ns)],
            t_ref,
        );
    }

    fn percentile(sorted: &[u128], kind: &str) -> u128 {
        if sorted.is_empty() {
            return 0;
        }
        match kind {
            "p50" => sorted[(sorted.len() - 1) / 2],
            "p99" => {
                let idx = ((0.99 * sorted.len() as f64).ceil() as usize).saturating_sub(1);
                sorted[idx.min(sorted.len() - 1)]
            }
            "max" => sorted[sorted.len() - 1],
            _ => sorted[sorted.len() - 1],
        }
    }

    fn write_timing_case(
        out: &std::path::Path,
        samples: &mut String,
        csv: &mut String,
        summaries: &mut String,
        case: &str,
        elapsed: &[u128],
        accept: u64,
        reject: u64,
        reject_reason: Option<&str>,
        first: bool,
    ) {
        let mut sorted = elapsed.to_vec();
        sorted.sort_unstable();
        let n = sorted.len() as u64;
        let sum: u128 = sorted.iter().copied().sum();
        let mean = if n == 0 { 0.0 } else { sum as f64 / n as f64 };
        let reasons = match reject_reason {
            Some(reason) if reject > 0 => format!("{{\"{reason}\": {reject}}}"),
            _ => "{}".to_string(),
        };
        if !first {
            summaries.push_str(",\n");
        }
        summaries.push_str(&format!(
            "    \"{case}\": {{\n      \"n\": {n},\n      \"p50_ns\": {p50},\n      \"p99_ns\": {p99},\n      \"max_ns\": {max},\n      \"mean_ns\": {mean:.4},\n      \"accept\": {accept},\n      \"reject\": {reject},\n      \"reject_reasons\": {reasons},\n      \"percentile_rule\": \"p50=sorted[(N-1)//2]; p99=sorted[ceil(0.99*N)-1]; max=sorted[N-1]\"\n    }}",
            case = json_escape(case),
            n = n,
            p50 = percentile(&sorted, "p50"),
            p99 = percentile(&sorted, "p99"),
            max = percentile(&sorted, "max"),
            mean = mean,
            accept = accept,
            reject = reject,
            reasons = reasons
        ));
        for (i, value) in elapsed.iter().enumerate() {
            let result = if reject_reason.is_some() { "reject" } else { "accept" };
            let reason_json = match reject_reason {
                Some(reason) => format!("\"{}\"", json_escape(reason)),
                None => "null".to_string(),
            };
            samples.push_str(&format!(
                "{{\"function_under_test\":\"gpu_pow::verify_pre_hash\",\"file\":\"chain-core/node/src/gpu_pow.rs\",\"shape\":\"v2-2x16\",\"a\":2,\"b\":16,\"k\":2048,\"order\":\"decode_context_then_claimed_ticket_then_recompute\",\"case\":\"{case}\",\"iteration\":{i},\"nonce\":{i},\"result\":\"{result}\",\"reject_reason\":{reason},\"elapsed_ns\":{elapsed},\"derive_ns\":null,\"matmul_ns\":null,\"digest_ns\":null,\"ticket_ns\":null,\"encoded_len\":105,\"mapping\":\"i32::from(b % 129) - 64\"}}\n",
                case = json_escape(case),
                i = i,
                result = result,
                reason = reason_json,
                elapsed = value
            ));
            csv.push_str(&format!(
                "{case},{i},{result},{reason},{elapsed}\n",
                case = case,
                i = i,
                result = result,
                reason = reject_reason.unwrap_or(""),
                elapsed = value
            ));
        }
        let _ = out;
    }

    #[test]
    fn verify_pre_hash_phase3_timings_v2_2x16() {
        let Some(dir) = std::env::var_os("PHASE3_VERIFY_OUT") else {
            return;
        };
        let out = std::path::PathBuf::from(dir);
        std::fs::create_dir_all(&out).expect("create PHASE3_VERIFY_OUT");
        let (genesis, pre_hash, _cert, bytes, _claimed) = valid_v2_fixture();
        let expected = challenge_from_pre_hash(genesis, pre_hash);

        let warmup = verify_pre_hash(genesis, pre_hash, &bytes, [255u8; 32]);
        assert_eq!(warmup, Ok(ticket(&Certificate::decode_fixed(&bytes).unwrap())));

        let mut verify_elapsed = Vec::with_capacity(10_000);
        for nonce in 0..10_000u64 {
            let cert = compute(expected, nonce);
            let encoded = cert.encode_fixed();
            let started = std::time::Instant::now();
            let outcome = verify_pre_hash(genesis, pre_hash, &encoded, [255u8; 32]);
            verify_elapsed.push(started.elapsed().as_nanos());
            assert!(outcome.is_ok(), "verify nonce {nonce}");
        }

        let reject_above_cert = compute(expected, 5);
        let reject_above_bytes = reject_above_cert.encode_fixed();
        let mut above_elapsed = Vec::with_capacity(1_000);
        for _ in 0..1_000u64 {
            let started = std::time::Instant::now();
            let outcome = verify_pre_hash(genesis, pre_hash, &reject_above_bytes, [0u8; 32]);
            above_elapsed.push(started.elapsed().as_nanos());
            assert_eq!(outcome, Err(Error::AboveTarget));
        }

        let mut digest_elapsed = Vec::with_capacity(1_000);
        for nonce in 0..1_000u64 {
            let cert = compute(expected, nonce);
            let mut tampered = cert.encode_fixed();
            tampered[41] ^= 0xff;
            let decoded = Certificate::decode_fixed(&tampered).unwrap();
            let claimed_ticket = ticket(&decoded);
            let started = std::time::Instant::now();
            let outcome = verify_pre_hash(genesis, pre_hash, &tampered, claimed_ticket);
            digest_elapsed.push(started.elapsed().as_nanos());
            assert_eq!(outcome, Err(Error::WorkMismatch));
        }

        let mut length_elapsed = Vec::with_capacity(1_000);
        let mut truncated = bytes.clone();
        truncated.pop();
        for _ in 0..1_000u64 {
            let started = std::time::Instant::now();
            let outcome = verify_pre_hash(genesis, pre_hash, &truncated, [255u8; 32]);
            length_elapsed.push(started.elapsed().as_nanos());
            assert_eq!(outcome, Err(Error::InvalidEncoding));
        }

        let mut version_elapsed = Vec::with_capacity(1_000);
        let mut unsupported = bytes.clone();
        unsupported[0] = 3;
        for _ in 0..1_000u64 {
            let started = std::time::Instant::now();
            let outcome = verify_pre_hash(genesis, pre_hash, &unsupported, [255u8; 32]);
            version_elapsed.push(started.elapsed().as_nanos());
            assert_eq!(outcome, Err(Error::UnsupportedVersion));
        }

        let mut challenge_elapsed = Vec::with_capacity(1_000);
        for _ in 0..1_000u64 {
            let started = std::time::Instant::now();
            let outcome = verify_pre_hash(H256::repeat_byte(8), pre_hash, &bytes, [255u8; 32]);
            challenge_elapsed.push(started.elapsed().as_nanos());
            assert_eq!(outcome, Err(Error::ChallengeMismatch));
        }

        let mut samples = String::new();
        let mut csv = String::from("case,iteration,result,reject_reason,elapsed_ns\n");
        let mut summaries = String::new();
        write_timing_case(&out, &mut samples, &mut csv, &mut summaries, "verify", &verify_elapsed, 10_000, 0, None, true);
        write_timing_case(&out, &mut samples, &mut csv, &mut summaries, "reject_above_target", &above_elapsed, 0, 1_000, Some("above_target"), false);
        write_timing_case(&out, &mut samples, &mut csv, &mut summaries, "reject_digest", &digest_elapsed, 0, 1_000, Some("work_mismatch"), false);
        write_timing_case(&out, &mut samples, &mut csv, &mut summaries, "reject_length", &length_elapsed, 0, 1_000, Some("invalid_encoding"), false);
        write_timing_case(&out, &mut samples, &mut csv, &mut summaries, "reject_version", &version_elapsed, 0, 1_000, Some("unsupported_version"), false);
        write_timing_case(&out, &mut samples, &mut csv, &mut summaries, "reject_challenge", &challenge_elapsed, 0, 1_000, Some("challenge_mismatch"), false);

        let summary = format!(
            "{{\n  \"function_under_test\": \"gpu_pow::verify_pre_hash\",\n  \"file\": \"chain-core/node/src/gpu_pow.rs\",\n  \"shape\": \"v2-2x16\",\n  \"a\": 2,\n  \"b\": 16,\n  \"k\": 2048,\n  \"order\": \"decode_context_then_claimed_ticket_then_recompute\",\n  \"mapping\": \"i32::from(b % 129) - 64\",\n  \"identity_notes\": \"not MX10; mapping is i32::from(b % 129) - 64, not i32(b)-128; certificate 0x02 is current node encoding only and does not mean a freeze still uses version 2\",\n  \"source\": \"samples.jsonl\",\n  \"recomputed_from\": \"samples.jsonl elapsed_ns only; do not trust any precomputed summary\",\n  \"cases\": {{\n{summaries}\n  }}\n}}\n",
            summaries = summaries
        );
        std::fs::write(out.join("samples.jsonl"), samples).expect("samples.jsonl");
        std::fs::write(out.join("samples.csv"), csv).expect("samples.csv");
        std::fs::write(out.join("summary.json"), summary).expect("summary.json");
    }
}
