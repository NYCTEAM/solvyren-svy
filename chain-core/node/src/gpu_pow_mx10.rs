//! MX10 evaluation encoding: node-side CPU reference computation, verifier and
//! unit tests for the draft "矩阵规格草案：身份、时间与输入映射".
//!
//! EVALUATION ONLY. This module is not wired into any import queue, chain spec,
//! service, mining or consensus path. It does not select an official `n`, does
//! not freeze the algorithm, does not close T1/T2/T3/T4, and it is not a
//! production `workAlgorithm`. The V2 module (`gpu_pow.rs`) is untouched.
//!
//! Shape: A is n x 2048, B is 2048 x n, C = A x B is n x n. `n` is a caller
//! supplied evaluation parameter restricted to the draft candidates
//! 16 / 32 / 64 / 128; the inner dimension is fixed at 2048. Neither is read
//! from the certificate or any submitter field.
#![allow(dead_code)]

use blake2::{Blake2s256, Digest as BlakeDigest};
use eagle_runtime::Header;
use sp_core::{hashing::sha2_256, H256};
use sp_runtime::{
    codec::{Decode, Encode},
    generic::DigestItem,
    traits::Header as HeaderT,
};

/// MX10 algorithm and encoding version byte (draft T1).
pub const VERSION: u8 = 10;
/// Fixed SCALE size of the certificate: 1 + 32 + 8 + 32 + 32.
pub const CERTIFICATE_BYTES: usize = 105;
/// Challenge domain (draft T1), with trailing NUL.
pub const CHALLENGE_DOMAIN: &[u8] = b"SVY-POW-MX10-CHALLENGE\0";
/// Input derivation domain (draft T1), with trailing NUL.
pub const INPUT_DOMAIN: &[u8] = b"SVY-POW-MX10-INPUT\0";
/// Commitment domain (draft T1), with trailing NUL.
pub const COMMIT_DOMAIN: &[u8] = b"SVY-POW-MX10-COMMIT\0";
/// Ticket domain (draft T1), with trailing NUL.
pub const TICKET_DOMAIN: &[u8] = b"SVY-POW-MX10-TICKET\0";
/// Mapping rule identifier (draft T1/A9), with trailing NUL: map(b)=i32(b)-128.
pub const MAP_ID: &[u8] = b"SVY-POW-MX10-MAP-I32-SUB-128\0";
/// workAlgorithm digest prefix (draft T1), with trailing NUL.
pub const WORKID_DOMAIN: &[u8] = b"SVY-POW-MX10-WORKID\0";
/// Inner dimension K. Changing it is a redesign (draft T1 gate).
pub const INNER_DIM: u32 = 2048;
/// Draft evaluation candidates for n. None of them is selected.
pub const N_CANDIDATES: [u32; 4] = [16, 32, 64, 128];

/// Evaluation shape supplied by the trusted caller (runtime / evaluation
/// constants), never by a certificate or submitter.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct EvalShape {
    n: u32,
    inner_dim: u32,
}

impl EvalShape {
    /// Accept only a draft candidate n and the fixed inner dimension 2048.
    pub fn new(n: u32, inner_dim: u32) -> Result<Self, Error> {
        if !N_CANDIDATES.contains(&n) || inner_dim != INNER_DIM {
            return Err(Error::UnsupportedShape);
        }
        let shape = Self { n, inner_dim };
        if shape.input_elements() % 32 != 0 {
            return Err(Error::UnsupportedShape);
        }
        Ok(shape)
    }
    /// Rows of A and columns of B.
    pub fn n(&self) -> u32 {
        self.n
    }
    /// Inner dimension (always 2048).
    pub fn inner_dim(&self) -> u32 {
        self.inner_dim
    }
    /// Element count of A and B together: 2 x n x 2048.
    pub fn input_elements(&self) -> usize {
        2 * self.n as usize * self.inner_dim as usize
    }
    /// Element count of C: n x n.
    pub fn output_elements(&self) -> usize {
        self.n as usize * self.n as usize
    }
}

/// A fixed-size, independently recomputable MX10 ticket claim.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Certificate {
    /// Encoding and algorithm version; must be 10.
    pub version: u8,
    /// Challenge binding genesis and the canonical pre-seal header hash.
    pub challenge: [u8; 32],
    /// Search nonce (little-endian on the wire).
    pub nonce: u64,
    /// SHA-256 of all 2 x n x 2048 signed little-endian i32 inputs.
    pub input_digest: [u8; 32],
    /// SHA-256 of the n x n signed little-endian i32 outputs, row-major.
    pub output_digest: [u8; 32],
}

/// Rejection reasons for the MX10 evaluation primitives.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    /// The supplied header already has a consensus seal.
    HeaderAlreadySealed,
    /// Not exactly 105 bytes, malformed, or trailing data.
    InvalidEncoding,
    /// Version byte is not 10 (includes V2 certificates, version 2).
    UnsupportedVersion,
    /// Certificate does not bind this genesis and candidate.
    ChallengeMismatch,
    /// Input or output digest differs from CPU recomputation.
    WorkMismatch,
    /// Ticket from claimed fields exceeds the caller-provided target.
    AboveTarget,
    /// Caller supplied an n outside the draft candidates or inner dim != 2048.
    UnsupportedShape,
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

    /// Parse exactly one certificate; reject any version other than 10.
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

/// A9 mapping: unsigned byte 0..=255 to i32 -128..=127. Not an i8 reinterpretation.
#[inline]
pub fn map_byte(b: u8) -> i32 {
    (b as i32) - 128
}

/// challenge = SHA-256(CHALLENGE_DOMAIN || VERSION || genesis || pre_hash).
pub fn challenge_from_pre_hash(genesis: H256, pre_hash: H256) -> [u8; 32] {
    let mut message = Vec::with_capacity(CHALLENGE_DOMAIN.len() + 65);
    message.extend_from_slice(CHALLENGE_DOMAIN);
    message.push(VERSION);
    message.extend_from_slice(genesis.as_bytes());
    message.extend_from_slice(pre_hash.as_bytes());
    sha2_256(&message)
}

/// Hash an unsealed header to obtain the canonical challenge.
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

/// Derive all 2 x n x 2048 mapped inputs. For counter in 0..elements/32:
/// BLAKE2s-256(INPUT_DOMAIN || challenge || nonce_le_u64 || n_le_u32 ||
/// inner_dim_le_u32 || counter_le_u32), each output byte mapped by `map_byte`.
pub fn derive_input(shape: EvalShape, challenge: [u8; 32], nonce: u64) -> Vec<i32> {
    let elements = shape.input_elements();
    let mut values = Vec::with_capacity(elements);
    for counter in 0..(elements / 32) as u32 {
        let mut hash = Blake2s256::new();
        hash.update(INPUT_DOMAIN);
        hash.update(challenge);
        hash.update(nonce.to_le_bytes());
        hash.update(shape.n.to_le_bytes());
        hash.update(shape.inner_dim.to_le_bytes());
        hash.update(counter.to_le_bytes());
        for b in hash.finalize() {
            values.push(map_byte(b));
        }
    }
    values
}

#[cfg(test)]
thread_local! {
    static COMPUTE_INVOCATIONS: std::cell::Cell<u64> = const { std::cell::Cell::new(0) };
}

#[cfg(test)]
fn compute_invocations() -> u64 {
    COMPUTE_INVOCATIONS.with(|c| c.get())
}

/// Nanosecond phase timings collected inside a single compute+ticket call.
/// Hooks only observe; they do not alter digests, tickets, or mapping.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PhaseTimings {
    pub input_derive_ns: u128,
    pub matrix_ns: u128,
    pub digests_ns: u128,
    pub ticket_ns: u128,
}

impl PhaseTimings {
    pub fn total_ns(self) -> u128 {
        self.input_derive_ns + self.matrix_ns + self.digests_ns + self.ticket_ns
    }
}

/// Full CPU reference computation. Layout mirrors V2: the first n x 2048
/// values are A row-major; the next n x 2048 values are B stored column by
/// column (B^T row-major), i.e. `values[(n + col) * K + k]`, exactly as V2
/// `values[(A_ROWS + col) * K + k]`. C[row][col] = sum_k A[row][k] * B[k][col],
/// serialized row-major (`row * n + col`) as i32 little-endian. Max |C| is
/// 128 * 128 * 2048 = 33,554,432 < i32::MAX, so no overflow.
pub fn compute(shape: EvalShape, challenge: [u8; 32], nonce: u64) -> Certificate {
    compute_inner(shape, challenge, nonce, None).0
}

/// Same work as `compute` plus ticket, with per-phase Instant timings taken
/// inside one call (not three independent runs subtracted). Digests and the
/// ticket match `compute` + `ticket` exactly; Instant only observes.
pub fn compute_phased(
    shape: EvalShape,
    challenge: [u8; 32],
    nonce: u64,
) -> (Certificate, [u8; 32], PhaseTimings) {
    let mut timings = PhaseTimings::default();
    let (cert, claimed) = compute_inner(shape, challenge, nonce, Some(&mut timings));
    (cert, claimed, timings)
}

fn compute_inner(
    shape: EvalShape,
    challenge: [u8; 32],
    nonce: u64,
    mut timings: Option<&mut PhaseTimings>,
) -> (Certificate, [u8; 32]) {
    #[cfg(test)]
    COMPUTE_INVOCATIONS.with(|c| c.set(c.get().saturating_add(1)));
    let n = shape.n as usize;
    let k_dim = shape.inner_dim as usize;

    let t_input = std::time::Instant::now();
    let values = derive_input(shape, challenge, nonce);
    let mut input = Vec::with_capacity(values.len() * 4);
    for value in &values {
        input.extend_from_slice(&value.to_le_bytes());
    }
    if let Some(t) = timings.as_mut() {
        t.input_derive_ns = t_input.elapsed().as_nanos();
    }

    let t_matrix = std::time::Instant::now();
    let mut output = vec![0u8; shape.output_elements() * 4];
    for row in 0..n {
        let a = &values[row * k_dim..(row + 1) * k_dim];
        for col in 0..n {
            let b = &values[(n + col) * k_dim..(n + col + 1) * k_dim];
            let mut dot = 0i32;
            for k in 0..k_dim {
                dot += a[k] * b[k];
            }
            let index = row * n + col;
            output[index * 4..index * 4 + 4].copy_from_slice(&dot.to_le_bytes());
        }
    }
    if let Some(t) = timings.as_mut() {
        t.matrix_ns = t_matrix.elapsed().as_nanos();
    }

    let t_digests = std::time::Instant::now();
    let certificate = Certificate {
        version: VERSION,
        challenge,
        nonce,
        input_digest: sha2_256(&input),
        output_digest: sha2_256(&output),
    };
    if let Some(t) = timings.as_mut() {
        t.digests_ns = t_digests.elapsed().as_nanos();
    }

    let claimed = if timings.is_some() {
        let t_ticket = std::time::Instant::now();
        let claimed = ticket(&certificate);
        if let Some(t) = timings.as_mut() {
            t.ticket_ns = t_ticket.elapsed().as_nanos();
        }
        claimed
    } else {
        // Preserve historical `compute` behaviour: no ticket work on the
        // non-phased path. Callers that need the ticket call `ticket` themselves.
        [0u8; 32]
    };
    (certificate, claimed)
}

/// ticket = SHA256(TICKET_DOMAIN || fields || SHA256(COMMIT_DOMAIN || fields)),
/// fields = challenge || nonce_le || input_digest || output_digest.
pub fn ticket(certificate: &Certificate) -> [u8; 32] {
    let mut fields = Vec::with_capacity(104);
    fields.extend_from_slice(&certificate.challenge);
    fields.extend_from_slice(&certificate.nonce.to_le_bytes());
    fields.extend_from_slice(&certificate.input_digest);
    fields.extend_from_slice(&certificate.output_digest);
    let mut commitment_data = Vec::with_capacity(COMMIT_DOMAIN.len() + 104);
    commitment_data.extend_from_slice(COMMIT_DOMAIN);
    commitment_data.extend_from_slice(&fields);
    let commitment = sha2_256(&commitment_data);
    let mut ticket_data = Vec::with_capacity(TICKET_DOMAIN.len() + 136);
    ticket_data.extend_from_slice(TICKET_DOMAIN);
    ticket_data.extend_from_slice(&fields);
    ticket_data.extend_from_slice(&commitment);
    sha2_256(&ticket_data)
}

/// Mandatory T2 order: (1) decode + version + trusted challenge context;
/// (2) ticket from the claimed fields compared with the trusted target
/// (32-byte big-endian unsigned, inclusive) and rejected as `AboveTarget`
/// before any recomputation; (3) only then a full `compute()` and digest
/// check. `shape` must come from trusted runtime / evaluation constants.
pub fn verify_pre_hash(
    shape: EvalShape,
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
    let claimed = ticket(&certificate);
    if claimed > target {
        return Err(Error::AboveTarget);
    }
    let calculated = compute(shape, expected, certificate.nonce);
    if certificate.input_digest != calculated.input_digest
        || certificate.output_digest != calculated.output_digest
    {
        return Err(Error::WorkMismatch);
    }
    Ok(claimed)
}

/// Convenience wrapper which rejects sealed headers before verification.
pub fn verify(
    shape: EvalShape,
    genesis: H256,
    header: &Header,
    encoded: &[u8],
    target: [u8; 32],
) -> Result<[u8; 32], Error> {
    challenge_for_header(genesis, header)?;
    verify_pre_hash(shape, genesis, header.hash(), encoded, target)
}

/// Evaluation-only workAlgorithm digest (draft T1). NOT written to any chain
/// spec. SHA-256(WORKID_DOMAIN || 10 || CHALLENGE || INPUT || COMMIT || TICKET
/// || MAP_ID || n_le_u32 || 2048_le_u32 || 105_le_u32).
pub fn work_algorithm_preimage(shape: EvalShape) -> Vec<u8> {
    let mut m = Vec::new();
    m.extend_from_slice(WORKID_DOMAIN);
    m.push(VERSION);
    m.extend_from_slice(CHALLENGE_DOMAIN);
    m.extend_from_slice(INPUT_DOMAIN);
    m.extend_from_slice(COMMIT_DOMAIN);
    m.extend_from_slice(TICKET_DOMAIN);
    m.extend_from_slice(MAP_ID);
    m.extend_from_slice(&shape.n.to_le_bytes());
    m.extend_from_slice(&shape.inner_dim.to_le_bytes());
    m.extend_from_slice(&(CERTIFICATE_BYTES as u32).to_le_bytes());
    m
}

/// SHA-256 of `work_algorithm_preimage`.
pub fn work_algorithm_digest(shape: EvalShape) -> [u8; 32] {
    sha2_256(&work_algorithm_preimage(shape))
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_runtime::generic::Digest;

    fn shape(n: u32) -> EvalShape {
        EvalShape::new(n, INNER_DIM).expect("draft candidate n")
    }
    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }
    fn header() -> Header {
        Header::new(
            42,
            H256::repeat_byte(1),
            H256::repeat_byte(2),
            H256::repeat_byte(3),
            Digest::default(),
        )
    }
    fn just_below(ticket: [u8; 32]) -> [u8; 32] {
        let mut below = ticket;
        let i = below.iter().rposition(|x| *x != 0).expect("non-zero ticket");
        below[i] -= 1;
        for b in &mut below[i + 1..] {
            *b = 255;
        }
        below
    }
    fn fixture(n: u32) -> (EvalShape, H256, H256, Certificate, Vec<u8>, [u8; 32]) {
        let s = shape(n);
        let genesis = H256::repeat_byte(7);
        let pre_hash = header().hash();
        let cert = compute(s, challenge_from_pre_hash(genesis, pre_hash), 5);
        let bytes = cert.encode_fixed();
        let claimed = ticket(&cert);
        (s, genesis, pre_hash, cert, bytes, claimed)
    }

    // 1. A9 mapping table; prove it is not an i8 reinterpretation nor V2.
    #[test]
    fn mx10_map_table_matches_a9_not_i8_reinterpretation() {
        assert_eq!(map_byte(0), -128);
        assert_eq!(map_byte(127), -1);
        assert_eq!(map_byte(128), 0);
        assert_eq!(map_byte(255), 127);
        // i8 reinterpretation would give 0 -> 0 and 128 -> -128.
        assert_eq!(i32::from(0u8 as i8), 0);
        assert_eq!(i32::from(128u8 as i8), -128);
        assert_ne!(map_byte(0), i32::from(0u8 as i8));
        assert_ne!(map_byte(128), i32::from(128u8 as i8));
        // Bijection 0..=255 -> -128..=127, strictly increasing.
        let mut seen = [false; 256];
        for b in 0..=255u8 {
            let v = map_byte(b);
            assert!((-128..=127).contains(&v));
            assert!(!seen[(v + 128) as usize]);
            seen[(v + 128) as usize] = true;
            if b > 0 {
                assert_eq!(v, map_byte(b - 1) + 1);
            }
        }
        assert!(seen.iter().all(|x| *x));
        // Differs from the V2 rule i32::from(b % 129) - 64.
        assert_ne!(map_byte(0), i32::from(0u8 % 129) - 64);
        assert_eq!(MAP_ID, b"SVY-POW-MX10-MAP-I32-SUB-128\0");
    }

    #[test]
    fn mx10_domains_are_separated_from_v2_v3_and_offline() {
        for d in [CHALLENGE_DOMAIN, INPUT_DOMAIN, COMMIT_DOMAIN, TICKET_DOMAIN, MAP_ID, WORKID_DOMAIN] {
            assert_eq!(*d.last().unwrap(), 0u8);
            assert!(d.starts_with(b"SVY-POW-MX10-"));
            let s = std::str::from_utf8(d).unwrap();
            assert!(!s.contains("EAGW-POW-V2"));
            assert!(!s.contains("EAGW-POW-V3"));
            assert!(!s.contains("SVY-OFFLINE-SHAPE-PIPELINE"));
            assert!(!s.contains("129"));
        }
        assert_eq!(VERSION, 10);
        assert_ne!(VERSION, crate::gpu_pow::VERSION);
        assert_eq!(CERTIFICATE_BYTES, crate::gpu_pow::CERTIFICATE_BYTES);
    }

    #[test]
    fn mx10_shape_rejects_non_candidates() {
        for n in [0u32, 1, 8, 15, 17, 48, 96, 256] {
            assert_eq!(EvalShape::new(n, INNER_DIM), Err(Error::UnsupportedShape), "n={n}");
        }
        for k in [0u32, 1024, 2047, 2049, 4096] {
            assert_eq!(EvalShape::new(32, k), Err(Error::UnsupportedShape), "k={k}");
        }
    }

    // 2. Wrong version rejected before recompute.
    #[test]
    fn mx10_rejects_wrong_version_before_recompute() {
        let (s, genesis, pre_hash, _cert, bytes, _) = fixture(16);
        for v in [0u8, 1, 2, 3, 9, 11, 255] {
            let mut b = bytes.clone();
            b[0] = v;
            assert_eq!(Certificate::decode_fixed(&b), Err(Error::UnsupportedVersion), "v={v}");
            let before = compute_invocations();
            assert_eq!(
                verify_pre_hash(s, genesis, pre_hash, &b, [255; 32]),
                Err(Error::UnsupportedVersion),
                "v={v}"
            );
            assert_eq!(compute_invocations(), before, "v={v} must reject before recompute");
        }
    }

    // 3. A real V2 certificate (version 2) is never accepted by MX10.
    #[test]
    fn mx10_rejects_v2_certificate() {
        let genesis = H256::repeat_byte(7);
        let pre_hash = header().hash();
        let v2_challenge = crate::gpu_pow::challenge_from_pre_hash(genesis, pre_hash);
        let v2 = crate::gpu_pow::compute(v2_challenge, 5);
        let v2_bytes = v2.encode_fixed();
        assert_eq!(v2_bytes.len(), CERTIFICATE_BYTES);
        assert_eq!(v2_bytes[0], 2);
        assert_eq!(Certificate::decode_fixed(&v2_bytes), Err(Error::UnsupportedVersion));
        for n in N_CANDIDATES {
            let before = compute_invocations();
            assert_eq!(
                verify_pre_hash(shape(n), genesis, pre_hash, &v2_bytes, [255; 32]),
                Err(Error::UnsupportedVersion)
            );
            assert_eq!(compute_invocations(), before);
        }
        // Relabelling a V2 certificate as version 10 still fails: the V2
        // challenge domain does not match the MX10 challenge.
        let mut relabelled = v2_bytes.clone();
        relabelled[0] = VERSION;
        assert_ne!(v2_challenge, challenge_from_pre_hash(genesis, pre_hash));
        assert_eq!(
            verify_pre_hash(shape(16), genesis, pre_hash, &relabelled, [255; 32]),
            Err(Error::ChallengeMismatch)
        );
        // Even with an MX10 challenge, V2-style digests do not match MX10 work.
        let mx_challenge = challenge_from_pre_hash(genesis, pre_hash);
        let v2_work = crate::gpu_pow::compute(mx_challenge, 5);
        let forged = Certificate {
            version: VERSION,
            challenge: mx_challenge,
            nonce: 5,
            input_digest: v2_work.input_digest,
            output_digest: v2_work.output_digest,
        };
        assert_eq!(
            verify_pre_hash(shape(16), genesis, pre_hash, &forged.encode_fixed(), [255; 32]),
            Err(Error::WorkMismatch)
        );
    }

    // 4. n = 16/32/64/128 parameterised: element count, compute -> verify.
    #[test]
    fn mx10_parameterised_n_compute_then_verify() {
        let genesis = H256::repeat_byte(7);
        let h = header();
        let pre_hash = h.hash();
        let challenge = challenge_from_pre_hash(genesis, pre_hash);
        let mut input_digests = Vec::new();
        for n in N_CANDIDATES {
            let s = shape(n);
            assert_eq!(s.n(), n);
            assert_eq!(s.inner_dim(), 2048);
            assert_eq!(s.input_elements(), 2 * n as usize * 2048);
            assert_eq!(s.input_elements() % 32, 0);
            assert_eq!(s.output_elements(), (n * n) as usize);
            let values = derive_input(s, challenge, 5);
            assert_eq!(values.len(), 2 * n as usize * 2048);
            assert!(values.iter().all(|v| (-128..=127).contains(v)));
            let cert = compute(s, challenge, 5);
            let bytes = cert.encode_fixed();
            assert_eq!(bytes.len(), CERTIFICATE_BYTES);
            assert_eq!(bytes[0], VERSION);
            assert_eq!(Certificate::decode_fixed(&bytes), Ok(cert.clone()));
            let t = ticket(&cert);
            let before = compute_invocations();
            assert_eq!(verify_pre_hash(s, genesis, pre_hash, &bytes, t), Ok(t), "n={n}");
            assert_eq!(compute_invocations(), before + 1, "n={n} recompute exactly once");
            assert_eq!(verify(s, genesis, &h, &bytes, [255; 32]), Ok(t), "n={n}");
            input_digests.push((n, cert.input_digest, bytes));
        }
        // A certificate for one n never verifies under another caller n.
        for (i, (n_a, d_a, bytes_a)) in input_digests.iter().enumerate() {
            for (j, (n_b, d_b, _)) in input_digests.iter().enumerate() {
                if i != j {
                    assert_ne!(d_a, d_b, "n={n_a} vs n={n_b}");
                    assert_eq!(
                        verify_pre_hash(shape(*n_b), genesis, pre_hash, bytes_a, [255; 32]),
                        Err(Error::WorkMismatch),
                        "cert n={n_a} under shape n={n_b}"
                    );
                }
            }
        }
        // The n / inner-dim fields separate even the first BLAKE2s block.
        let v16 = derive_input(shape(16), challenge, 5);
        let v32 = derive_input(shape(32), challenge, 5);
        assert_ne!(v16[..32], v32[..32]);
    }

    // Fixed vectors produced independently with Python hashlib/struct/numpy
    // (challenge = [0; 32], nonce = 7). Not a T4 second implementation.
    #[test]
    fn mx10_fixed_vectors_python_hashlib() {
        let vectors: [(u32, &str, &str, &str); 4] = [
            (16, "3184c893ff6f9eb5303277b834df2e3fc0c290c29e8a148a46c5c145093961c5",
                 "30bfe89cb3fc5ce7a4c37e0fbfd47fa6cb0859272347bba6737ce9cee70a7dbf",
                 "2c483ea905c9f360bac61e5e141124293e0dee21447e88a7430a3838a62a7bc0"),
            (32, "6fc89094f501ec1afb3d2fbeefd0e0bcd5d2340e9a7018c2ae147a404359951d",
                 "47c9dec4e81f368d10adb30fe7200504bf91051a03f8c913d0c50387303c7e43",
                 "88e0c6093530b280ce34d54852f9bb52eca06bbfd74778a18ec755e580b74a59"),
            (64, "81754c5ab9b91fd7eedc0cf8a47ecf6fc72f275a036b1381d341cd66d0692b89",
                 "f857599322f3c427fbc9d2f5ea4270bfadf0363f69d760bc097fff222c811714",
                 "4e377d49ebf63129a78251407d8091bc5033ebfc6fbe243b63b35cea2697f808"),
            (128, "19b8feabd0a53fe5d0dcf7dc148df5d26b50f996807ec4f3e29ae6be571afe9f",
                  "3b8e08ed7cb542f0c2c9fa93186b14cc1ee07432e388c43e6e70bb83f9cfc10e",
                  "c9678fc1cd18c3672a9d61d0ca2d08966b6e435bb58b513979745efe71fe69db"),
        ];
        for (n, input, output, t) in vectors {
            let cert = compute(shape(n), [0; 32], 7);
            assert_eq!(hex(&cert.input_digest), input, "n={n}");
            assert_eq!(hex(&cert.output_digest), output, "n={n}");
            assert_eq!(hex(&ticket(&cert)), t, "n={n}");
        }
        assert_eq!(
            hex(&challenge_from_pre_hash(H256::repeat_byte(7), H256::repeat_byte(1))),
            "66b3d26dca3e62c429f2a263c450381e2c95f60afad767e0f1f42755c15a40e0"
        );
    }

    // 5. T2: above_target rejected before the full recomputation.
    #[test]
    fn mx10_verify_rejects_above_target_before_recompute() {
        for n in N_CANDIDATES {
            let (s, genesis, pre_hash, _cert, bytes, claimed) = fixture(n);
            for (case, target) in [("all_zero", [0u8; 32]), ("just_below", just_below(claimed))] {
                let before = compute_invocations();
                assert_eq!(
                    verify_pre_hash(s, genesis, pre_hash, &bytes, target),
                    Err(Error::AboveTarget),
                    "n={n} {case}"
                );
                assert_eq!(compute_invocations(), before, "n={n} {case} must not recompute");
            }
            // Inclusive comparison: target == ticket passes and recomputes once.
            let before = compute_invocations();
            assert_eq!(verify_pre_hash(s, genesis, pre_hash, &bytes, claimed), Ok(claimed));
            assert_eq!(compute_invocations(), before + 1);
        }
    }

    // T2 step 3: ticket within target but digest mismatch still rejected,
    // and the full recomputation is performed exactly once.
    #[test]
    fn mx10_verify_rejects_work_mismatch_after_ticket_passes() {
        for n in [32u32, 64] {
            let (s, genesis, pre_hash, _cert, bytes, _) = fixture(n);
            for offset in [41usize, 73] {
                let mut tampered = bytes.clone();
                tampered[offset] ^= 0xff;
                let decoded = Certificate::decode_fixed(&tampered).unwrap();
                let claimed = ticket(&decoded);
                let before = compute_invocations();
                assert_eq!(
                    verify_pre_hash(s, genesis, pre_hash, &tampered, claimed),
                    Err(Error::WorkMismatch),
                    "n={n} offset={offset}"
                );
                assert_eq!(compute_invocations(), before + 1, "n={n} offset={offset}");
            }
            // Nonce tamper.
            let mut cert = Certificate::decode_fixed(&bytes).unwrap();
            cert.nonce += 1;
            let before = compute_invocations();
            assert_eq!(
                verify_pre_hash(s, genesis, pre_hash, &cert.encode_fixed(), [255; 32]),
                Err(Error::WorkMismatch)
            );
            assert_eq!(compute_invocations(), before + 1);
        }
    }

    #[test]
    fn mx10_rejects_encoding_and_context_before_recompute() {
        let (s, genesis, pre_hash, _cert, bytes, _) = fixture(32);
        let h = header();
        let mut truncated = bytes.clone();
        truncated.pop();
        let mut trailing = bytes.clone();
        trailing.push(0);
        let before = compute_invocations();
        assert_eq!(
            verify_pre_hash(s, genesis, pre_hash, &truncated, [255; 32]),
            Err(Error::InvalidEncoding)
        );
        assert_eq!(
            verify_pre_hash(s, genesis, pre_hash, &trailing, [255; 32]),
            Err(Error::InvalidEncoding)
        );
        assert_eq!(
            verify_pre_hash(s, H256::repeat_byte(8), pre_hash, &bytes, [255; 32]),
            Err(Error::ChallengeMismatch)
        );
        let mut changed = h.clone();
        changed.set_parent_hash(H256::repeat_byte(9));
        assert_eq!(verify(s, genesis, &changed, &bytes, [255; 32]), Err(Error::ChallengeMismatch));
        let mut sealed = h.clone();
        sealed.digest_mut().push(DigestItem::Seal(*b"TEST", vec![]));
        assert_eq!(verify(s, genesis, &sealed, &bytes, [255; 32]), Err(Error::HeaderAlreadySealed));
        assert_eq!(compute_invocations(), before, "all must reject before recompute");
    }

    // 6. workAlgorithm digest: preimage layout and distinct per n.
    #[test]
    fn mx10_work_algorithm_digest_differs_per_n() {
        let expected_vectors = [
            (16u32, "25cdf8f5acd57e409d8324c71c15b271314e4effd3ef2ca75ff2c897d2275d17"),
            (32, "81d2bffc6d36d8ff7dceefe49244c428894a318020f185eddb60fcd35b44e26f"),
            (64, "95e694a454e230ad13a2cdbb54a85425ad95c4abd6a1cbec7f88ab941ce748ad"),
            (128, "9c1fffcd68dd2ba41425eaf85939efaa55b63437ce8ec51ab962175194c763cb"),
        ];
        let mut digests = Vec::new();
        for (n, vector) in expected_vectors {
            let s = shape(n);
            let pre = work_algorithm_preimage(s);
            let mut manual = Vec::new();
            manual.extend_from_slice(b"SVY-POW-MX10-WORKID\0");
            manual.push(10);
            manual.extend_from_slice(b"SVY-POW-MX10-CHALLENGE\0");
            manual.extend_from_slice(b"SVY-POW-MX10-INPUT\0");
            manual.extend_from_slice(b"SVY-POW-MX10-COMMIT\0");
            manual.extend_from_slice(b"SVY-POW-MX10-TICKET\0");
            manual.extend_from_slice(b"SVY-POW-MX10-MAP-I32-SUB-128\0");
            manual.extend_from_slice(&n.to_le_bytes());
            manual.extend_from_slice(&2048u32.to_le_bytes());
            manual.extend_from_slice(&105u32.to_le_bytes());
            assert_eq!(pre, manual, "n={n}");
            let d = work_algorithm_digest(s);
            assert_eq!(d, work_algorithm_digest(s));
            assert_eq!(hex(&d), vector, "n={n}");
            digests.push(d);
        }
        assert_ne!(work_algorithm_digest(shape(32)), work_algorithm_digest(shape(64)));
        for i in 0..digests.len() {
            for j in i + 1..digests.len() {
                assert_ne!(digests[i], digests[j]);
            }
        }
    }

    /// Timing hooks must not change digests or tickets vs plain compute+ticket.
    #[test]
    fn mx10_phased_matches_compute_and_ticket() {
        for n in N_CANDIDATES {
            let s = shape(n);
            let challenge = [7u8; 32];
            let plain = compute(s, challenge, 9);
            let (phased, claimed, timings) = compute_phased(s, challenge, 9);
            assert_eq!(plain, phased, "n={n}");
            assert_eq!(claimed, ticket(&plain), "n={n}");
            assert!(timings.total_ns() > 0, "n={n}");
            assert!(timings.input_derive_ns > 0, "n={n}");
            assert!(timings.matrix_ns > 0, "n={n}");
            assert!(timings.digests_ns > 0, "n={n}");
            assert!(timings.ticket_ns > 0, "n={n}");
        }
    }

    /// Ignored measurement harness. Env:
    ///   MX10_NMEASURE_OUT, MX10_NMEASURE_MODE=all|memory, MX10_NMEASURE_N
    #[test]
    #[ignore]
    fn mx10_nmeasure_harness() {
        use std::fs;
        use std::io::Write;
        use std::path::{Path, PathBuf};
        use std::time::{Duration, Instant};

        let out = PathBuf::from(std::env::var("MX10_NMEASURE_OUT").expect("MX10_NMEASURE_OUT"));
        let mode = std::env::var("MX10_NMEASURE_MODE").unwrap_or_else(|_| "all".into());
        let only_n: Option<u32> = std::env::var("MX10_NMEASURE_N").ok().map(|s| s.parse().expect("u32"));

        fn percentile(sorted: &[u128], kind: &str) -> u128 {
            let n = sorted.len();
            assert!(n > 0);
            match kind {
                "p50" => sorted[(n - 1) / 2],
                "p99" => {
                    let idx = ((0.99f64 * n as f64).ceil() as usize).saturating_sub(1);
                    sorted[idx.min(n - 1)]
                }
                "max" => sorted[n - 1],
                _ => panic!("unknown percentile"),
            }
        }

        fn write_jsonl(path: &Path, lines: &[String]) {
            if let Some(parent) = path.parent() {
                fs::create_dir_all(parent).unwrap();
            }
            let mut f = fs::File::create(path).unwrap();
            for line in lines {
                writeln!(f, "{line}").unwrap();
            }
        }

        fn vmhwm_kb() -> u64 {
            let status = fs::read_to_string("/proc/self/status").unwrap_or_default();
            for key in ["VmHWM:", "VmRSS:"] {
                for line in status.lines() {
                    if let Some(rest) = line.strip_prefix(key) {
                        if let Ok(v) = rest.split_whitespace().next().unwrap_or("0").parse::<u64>() {
                            return v;
                        }
                    }
                }
            }
            0
        }

        let genesis = H256::repeat_byte(7);
        let pre_hash = header().hash();
        let challenge = challenge_from_pre_hash(genesis, pre_hash);
        let ns: Vec<u32> = match only_n {
            Some(n) => vec![n],
            None => N_CANDIDATES.to_vec(),
        };

        if mode == "memory" {
            let n = only_n.expect("memory mode needs MX10_NMEASURE_N");
            let s = shape(n);
            let cert = compute(s, challenge, 11);
            let bytes = cert.encode_fixed();
            let claimed = ticket(&cert);
            let before = vmhwm_kb();
            assert_eq!(verify_pre_hash(s, genesis, pre_hash, &bytes, claimed), Ok(claimed));
            let after = vmhwm_kb();
            let dir = out.join(format!("n{n}"));
            fs::create_dir_all(&dir).unwrap();
            let body = format!(
                "{{\n  \"n\": {n},\n  \"function_under_test\": \"gpu_pow_mx10::verify_pre_hash\",\n  \"VmHWM_kB_before_verify\": {before},\n  \"VmHWM_kB_after_verify\": {after},\n  \"VmHWM_kB\": {after},\n  \"ru_maxrss_unit\": \"KB (Linux /proc/self/status VmHWM)\",\n  \"method\": \"standalone process per n; single legal verify_pre_hash after one compute\",\n  \"note\": \"Peak is process high-water including test harness; compare across n with same binary.\"\n}}\n"
            );
            fs::write(dir.join("memory.json"), body).unwrap();
            println!("memory n={n} VmHWM_kB={after}");
            return;
        }

        for n in ns {
            let s = shape(n);
            let dir = out.join(format!("n{n}"));
            fs::create_dir_all(&dir).unwrap();

            let verify_reps: usize = match n {
                32 | 64 => 300,
                _ => 120,
            };
            let reject_reps: usize = 1000;
            let phase_reps: usize = match n {
                32 | 64 => 200,
                _ => 100,
            };
            let warmup: usize = 8;

            let cert0 = compute(s, challenge, 5);
            let bytes0 = cert0.encode_fixed();
            let claimed0 = ticket(&cert0);
            for _ in 0..warmup {
                let _ = verify_pre_hash(s, genesis, pre_hash, &bytes0, claimed0);
            }

            let mut verify_lines = Vec::with_capacity(verify_reps);
            let mut verify_ns_samples = Vec::with_capacity(verify_reps);
            for i in 0..verify_reps {
                let cert = compute(s, challenge, 5 + i as u64);
                let bytes = cert.encode_fixed();
                let claimed = ticket(&cert);
                let t0 = Instant::now();
                let got = verify_pre_hash(s, genesis, pre_hash, &bytes, claimed);
                let elapsed = t0.elapsed().as_nanos();
                assert_eq!(got, Ok(claimed), "verify n={n} i={i}");
                verify_ns_samples.push(elapsed);
                verify_lines.push(format!(
                    "{{\"kind\":\"T_verify\",\"n\":{n},\"i\":{i},\"elapsed_ns\":{elapsed},\"accept\":true,\"function\":\"gpu_pow_mx10::verify_pre_hash\"}}"
                ));
            }
            write_jsonl(&dir.join("samples_verify.jsonl"), &verify_lines);
            write_jsonl(&dir.join("samples.jsonl"), &verify_lines);
            let mut sorted = verify_ns_samples.clone();
            sorted.sort_unstable();
            let p50 = percentile(&sorted, "p50");
            let p99 = percentile(&sorted, "p99");
            let mx = percentile(&sorted, "max");
            let p50_ms = p50 as f64 / 1e6;
            let p99_ms = p99 as f64 / 1e6;
            let max_ms = mx as f64 / 1e6;
            let verify_summary = format!(
                "{{\n  \"source\": \"{src}\",\n  \"recomputed_from\": \"samples_verify.jsonl / samples.jsonl elapsed_ns only\",\n  \"kind\": \"T_verify\",\n  \"function_under_test\": \"gpu_pow_mx10::verify_pre_hash\",\n  \"n\": {n},\n  \"sample_count\": {cnt},\n  \"warmup\": {warmup},\n  \"p50_ns\": {p50},\n  \"p99_ns\": {p99},\n  \"max_ns\": {mx},\n  \"p50_ms\": {p50_ms:.6},\n  \"p99_ms\": {p99_ms:.6},\n  \"max_ms\": {max_ms:.6},\n  \"budget_ms\": 100.0,\n  \"half_budget_ms\": 50.0,\n  \"exceeds_100ms_p50\": {e100p50},\n  \"exceeds_100ms_p99\": {e100p99},\n  \"exceeds_100ms_max\": {e100max},\n  \"exceeds_50ms_p50\": {e50p50},\n  \"exceeds_50ms_p99\": {e50p99},\n  \"exceeds_50ms_max\": {e50max},\n  \"percentile_rule\": \"p50=sorted[(N-1)//2]; p99=sorted[ceil(0.99*N)-1]; max=sorted[N-1]\"\n}}\n",
                src = dir.join("samples_verify.jsonl").display(),
                cnt = sorted.len(),
                e100p50 = p50_ms > 100.0,
                e100p99 = p99_ms > 100.0,
                e100max = max_ms > 100.0,
                e50p50 = p50_ms > 50.0,
                e50p99 = p99_ms > 50.0,
                e50max = max_ms > 50.0,
            );
            fs::write(dir.join("summary_verify.json"), &verify_summary).unwrap();

            // T_reject above_target
            let mut lines = Vec::new();
            let mut samples = Vec::new();
            let mut bumps = 0u64;
            for i in 0..reject_reps {
                let before = compute_invocations();
                let t0 = Instant::now();
                let got = verify_pre_hash(s, genesis, pre_hash, &bytes0, [0u8; 32]);
                let elapsed = t0.elapsed().as_nanos();
                assert_eq!(got, Err(Error::AboveTarget));
                let after = compute_invocations();
                let invoked = after != before;
                if invoked {
                    bumps += 1;
                }
                samples.push(elapsed);
                lines.push(format!(
                    "{{\"kind\":\"T_reject_above_target\",\"n\":{n},\"i\":{i},\"elapsed_ns\":{elapsed},\"compute_invoked\":{invoked}}}"
                ));
            }
            assert_eq!(bumps, 0, "above_target must not recompute");
            write_jsonl(&dir.join("samples_reject_above_target.jsonl"), &lines);
            let mut sorted = samples.clone();
            sorted.sort_unstable();
            let p50 = percentile(&sorted, "p50");
            let p99 = percentile(&sorted, "p99");
            let mx = percentile(&sorted, "max");
            fs::write(
                dir.join("summary_reject_above_target.json"),
                format!(
                    "{{\n  \"source\": \"{src}\",\n  \"recomputed_from\": \"samples_reject_above_target.jsonl elapsed_ns only\",\n  \"kind\": \"T_reject_above_target\",\n  \"function_under_test\": \"gpu_pow_mx10::verify_pre_hash\",\n  \"n\": {n},\n  \"sample_count\": {cnt},\n  \"p50_ns\": {p50},\n  \"p99_ns\": {p99},\n  \"max_ns\": {mx},\n  \"p50_ms\": {p50_ms:.6},\n  \"p99_ms\": {p99_ms:.6},\n  \"max_ms\": {max_ms:.6},\n  \"compute_invocations_bumped\": {bumps},\n  \"proves_reject_before_recompute\": true,\n  \"percentile_rule\": \"p50=sorted[(N-1)//2]; p99=sorted[ceil(0.99*N)-1]; max=sorted[N-1]\"\n}}\n",
                    src = dir.join("samples_reject_above_target.jsonl").display(),
                    cnt = sorted.len(),
                    p50_ms = p50 as f64 / 1e6,
                    p99_ms = p99 as f64 / 1e6,
                    max_ms = mx as f64 / 1e6,
                ),
            )
            .unwrap();

            // T_reject wrong version
            let mut bad = bytes0.clone();
            bad[0] = 2;
            let mut lines = Vec::new();
            let mut samples = Vec::new();
            let mut bumps = 0u64;
            for i in 0..reject_reps {
                let before = compute_invocations();
                let t0 = Instant::now();
                let got = verify_pre_hash(s, genesis, pre_hash, &bad, [255u8; 32]);
                let elapsed = t0.elapsed().as_nanos();
                assert_eq!(got, Err(Error::UnsupportedVersion));
                let after = compute_invocations();
                let invoked = after != before;
                if invoked {
                    bumps += 1;
                }
                samples.push(elapsed);
                lines.push(format!(
                    "{{\"kind\":\"T_reject_wrong_version\",\"n\":{n},\"i\":{i},\"elapsed_ns\":{elapsed},\"version\":2,\"compute_invoked\":{invoked}}}"
                ));
            }
            assert_eq!(bumps, 0);
            write_jsonl(&dir.join("samples_reject_wrong_version.jsonl"), &lines);
            let mut sorted = samples.clone();
            sorted.sort_unstable();
            let p50 = percentile(&sorted, "p50");
            let p99 = percentile(&sorted, "p99");
            let mx = percentile(&sorted, "max");
            fs::write(
                dir.join("summary_reject_wrong_version.json"),
                format!(
                    "{{\n  \"source\": \"{src}\",\n  \"recomputed_from\": \"samples_reject_wrong_version.jsonl elapsed_ns only\",\n  \"kind\": \"T_reject_wrong_version\",\n  \"function_under_test\": \"gpu_pow_mx10::verify_pre_hash\",\n  \"n\": {n},\n  \"sample_count\": {cnt},\n  \"p50_ns\": {p50},\n  \"p99_ns\": {p99},\n  \"max_ns\": {mx},\n  \"p50_ms\": {p50_ms:.6},\n  \"p99_ms\": {p99_ms:.6},\n  \"max_ms\": {max_ms:.6},\n  \"compute_invocations_bumped\": {bumps},\n  \"proves_reject_before_recompute\": true,\n  \"percentile_rule\": \"p50=sorted[(N-1)//2]; p99=sorted[ceil(0.99*N)-1]; max=sorted[N-1]\"\n}}\n",
                    src = dir.join("samples_reject_wrong_version.jsonl").display(),
                    cnt = sorted.len(),
                    p50_ms = p50 as f64 / 1e6,
                    p99_ms = p99 as f64 / 1e6,
                    max_ms = mx as f64 / 1e6,
                ),
            )
            .unwrap();

            // Phased
            let mut lines = Vec::new();
            let mut sum_input = 0u128;
            let mut sum_matrix = 0u128;
            let mut sum_digests = 0u128;
            let mut sum_ticket = 0u128;
            for i in 0..phase_reps {
                let (_c, _t, timings) = compute_phased(s, challenge, 100 + i as u64);
                sum_input += timings.input_derive_ns;
                sum_matrix += timings.matrix_ns;
                sum_digests += timings.digests_ns;
                sum_ticket += timings.ticket_ns;
                lines.push(format!(
                    "{{\"kind\":\"phased\",\"n\":{n},\"i\":{i},\"input_derive_ns\":{},\"matrix_ns\":{},\"digests_ns\":{},\"ticket_ns\":{},\"total_ns\":{}}}",
                    timings.input_derive_ns,
                    timings.matrix_ns,
                    timings.digests_ns,
                    timings.ticket_ns,
                    timings.total_ns()
                ));
            }
            write_jsonl(&dir.join("samples_phased.jsonl"), &lines);
            let reps = phase_reps as u128;
            let mean_input = sum_input / reps;
            let mean_matrix = sum_matrix / reps;
            let mean_digests = sum_digests / reps;
            let mean_ticket = sum_ticket / reps;
            let mean_total = mean_input + mean_matrix + mean_digests + mean_ticket;
            let (dom_name, dom_val) = [
                ("input_derive", mean_input),
                ("matrix", mean_matrix),
                ("digests", mean_digests),
                ("ticket", mean_ticket),
            ]
            .into_iter()
            .max_by_key(|(_, v)| *v)
            .unwrap();
            fs::write(
                dir.join("summary_phased.json"),
                format!(
                    "{{\n  \"source\": \"{src}\",\n  \"recomputed_from\": \"samples_phased.jsonl means\",\n  \"kind\": \"phased_same_call\",\n  \"function_under_test\": \"gpu_pow_mx10::compute_phased\",\n  \"n\": {n},\n  \"sample_count\": {phase_reps},\n  \"mean_input_derive_ns\": {mean_input},\n  \"mean_matrix_ns\": {mean_matrix},\n  \"mean_digests_ns\": {mean_digests},\n  \"mean_ticket_ns\": {mean_ticket},\n  \"mean_total_ns\": {mean_total},\n  \"mean_input_derive_ms\": {mi:.6},\n  \"mean_matrix_ms\": {mm:.6},\n  \"mean_digests_ms\": {md:.6},\n  \"mean_ticket_ms\": {mt:.6},\n  \"fraction_input_derive\": {fi:.6},\n  \"fraction_matrix\": {fm:.6},\n  \"fraction_digests\": {fd:.6},\n  \"fraction_ticket\": {ft:.6},\n  \"dominant_phase\": \"{dom_name}\",\n  \"dominant_fraction\": {df:.6},\n  \"note\": \"Phases timed inside one compute_phased call; not independent runs subtracted. Report only; do not announce redesign.\"\n}}\n",
                    src = dir.join("samples_phased.jsonl").display(),
                    mi = mean_input as f64 / 1e6,
                    mm = mean_matrix as f64 / 1e6,
                    md = mean_digests as f64 / 1e6,
                    mt = mean_ticket as f64 / 1e6,
                    fi = mean_input as f64 / mean_total as f64,
                    fm = mean_matrix as f64 / mean_total as f64,
                    fd = mean_digests as f64 / mean_total as f64,
                    ft = mean_ticket as f64 / mean_total as f64,
                    df = dom_val as f64 / mean_total as f64,
                ),
            )
            .unwrap();

            let window = Duration::from_secs(if matches!(n, 32 | 64) { 12 } else { 8 });
            let t_end = Instant::now() + window;
            let mut completed = 0u64;
            let mut nonce = 10_000u64;
            let wall0 = Instant::now();
            while Instant::now() < t_end {
                let _ = compute(s, challenge, nonce);
                nonce += 1;
                completed += 1;
            }
            let wall = wall0.elapsed().as_secs_f64();
            let cps = completed as f64 / wall;
            fs::write(
                dir.join("cpu_cps.json"),
                format!(
                    "{{\n  \"n\": {n},\n  \"function_under_test\": \"gpu_pow_mx10::compute\",\n  \"window_seconds_target\": {wt:.1},\n  \"window_seconds_actual\": {wall:.6},\n  \"completed_computes\": {completed},\n  \"cpu_cs\": {cps:.6},\n  \"note\": \"Full prescribed compute (derive+matrix+digests), not multiply-only. Single-threaded.\",\n  \"gpu_cs_3090\": null,\n  \"gpu_cs_note\": \"not_measured / no MX10 GPU path\"\n}}\n",
                    wt = window.as_secs_f64(),
                ),
            )
            .unwrap();

            fs::write(
                dir.join("i32_safety.json"),
                format!(
                    "{{\n  \"n\": {n},\n  \"max_abs_cell_argument\": \"128 * 128 * 2048 = 33554432\",\n  \"max_abs_cell_value\": 33554432,\n  \"independent_of_n\": true,\n  \"accumulator\": \"let mut dot = 0i32\",\n  \"term_max_abs\": 16384,\n  \"terms\": 2048,\n  \"i32_max\": {imax},\n  \"fits_i32\": true,\n  \"arithmetic_unchanged\": true\n}}\n",
                    imax = i32::MAX,
                ),
            )
            .unwrap();

            fs::write(
                dir.join("summary.json"),
                format!(
                    "{{\n  \"n\": {n},\n  \"official_n_selected\": null,\n  \"function_under_test\": \"gpu_pow_mx10::verify_pre_hash\",\n  \"T_verify_summary\": \"summary_verify.json\",\n  \"T_verify_samples\": \"samples.jsonl\",\n  \"files\": {{\n    \"samples_verify\": \"samples_verify.jsonl\",\n    \"samples\": \"samples.jsonl\",\n    \"summary_verify\": \"summary_verify.json\",\n    \"samples_reject_above_target\": \"samples_reject_above_target.jsonl\",\n    \"summary_reject_above_target\": \"summary_reject_above_target.json\",\n    \"samples_reject_wrong_version\": \"samples_reject_wrong_version.jsonl\",\n    \"summary_reject_wrong_version\": \"summary_reject_wrong_version.json\",\n    \"samples_phased\": \"samples_phased.jsonl\",\n    \"summary_phased\": \"summary_phased.json\",\n    \"cpu_cps\": \"cpu_cps.json\",\n    \"i32_safety\": \"i32_safety.json\",\n    \"memory\": \"memory.json\"\n  }}\n}}\n"
                ),
            )
            .unwrap();

            let mut srt = verify_ns_samples.clone();
            srt.sort_unstable();
            println!(
                "done n={n} verify_p50_ms={:.3} dominant_phase={dom_name}",
                percentile(&srt, "p50") as f64 / 1e6
            );
        }
    }
}
