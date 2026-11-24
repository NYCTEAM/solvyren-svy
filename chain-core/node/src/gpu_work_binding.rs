//! Deterministic candidate-header binding and bounded work-seal parsing.
//!
//! This module can recompute the experimental fixed-size workload on CPU. It does
//! not prove GPU execution, useful AI work, difficulty, eligibility or finality.
//! V1 is wired as a mandatory GPU-assisted Aura validity check; Aura still
//! selects the author and the ordinary longest-chain rule still selects forks.

use blake2::{Blake2s256, Digest as BlakeDigest};
use eagle_runtime::Header;
use sp_core::hashing::{blake2_256, sha2_256};
use sp_runtime::{
    codec::{Decode, Encode},
    generic::DigestItem,
    traits::Header as HeaderT,
    ConsensusEngineId,
};

/// Consensus engine identifier reserved for this experimental format.
pub const ENGINE_ID: ConsensusEngineId = *b"EAGW";
/// Current fixed-format certificate version.
pub const CERTIFICATE_VERSION: u8 = 1;
/// SCALE encoding length: one version byte and two 32-byte fields.
pub const CERTIFICATE_LEN: usize = 65;
const SUBJECT_DOMAIN: &[u8] = b"eagle-ai/gpu-work-subject/v1";
const INPUT_DOMAIN: &[u8] = b"EAGW-INPUT-V1\0";
const COMMIT_DOMAIN: &[u8] = b"EAGW-COMMIT-V1\0";
const K: usize = 2048;
const ROWS: usize = 2;
const COLS: usize = 16;
const ELEMENTS: usize = (ROWS + COLS) * K;
/// The exact length of the fixed V1 GPU output byte sequence.
pub const WORK_OUTPUT_BYTES: usize = ROWS * COLS * 4;

/// Bounded, fixed-size binding record; not itself a proof of computation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WorkCertificate {
    /// Must be [`CERTIFICATE_VERSION`].
    pub version: u8,
    /// Hash of the candidate header before its EAGW seal is appended.
    pub subject: [u8; 32],
    /// Opaque commitment to GPU output; no correctness claim is made here.
    pub work_commitment: [u8; 32],
}

impl WorkCertificate {
    fn encode_fixed(&self) -> Vec<u8> {
        (self.version, self.subject, self.work_commitment).encode()
    }
}

/// Failure while constructing or parsing a candidate's work binding.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BindingError {
    /// A candidate contains an EAGW digest before the final seal.
    UnexpectedEngineDigest,
    /// There is no final EAGW seal.
    MissingFinalSeal,
    /// The payload is malformed, truncated, oversized or has trailing bytes.
    InvalidCertificate,
    /// This module cannot interpret the certificate version.
    UnsupportedVersion,
    /// Certificate subject differs from the canonical candidate subject.
    SubjectMismatch,
    /// CPU-recomputed V1 workload commitment does not match the seal.
    WorkCommitmentMismatch,
}

/// CPU reference computation for the fixed V1 workload.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CpuWorkResult {
    /// SHA-256 of the signed little-endian i32 inputs.
    pub input_digest: [u8; 32],
    /// SHA-256 of the 32 signed little-endian i32 dot products.
    pub output_digest: [u8; 32],
    /// The dot products in row-major order.
    pub output: [i32; ROWS * COLS],
    /// SHA-256 commitment binding subject and both digests.
    pub commitment: [u8; 32],
}

/// Recompute the bounded V1 matrix workload using only deterministic CPU code.
///
/// BLAKE2s blocks hash `EAGW-INPUT-V1\0 || subject || counter_u32_le`.
/// Bytes map to `(byte % 129) - 64`; A has two rows, B has sixteen rows.
/// This reference calculation is inexpensive and does not establish GPU use.
pub fn recompute_cpu_work(subject: [u8; 32]) -> CpuWorkResult {
    let mut values = Vec::with_capacity(ELEMENTS);
    let mut input_bytes = Vec::with_capacity(ELEMENTS * 4);
    for counter in 0..(ELEMENTS / 32) as u32 {
        let mut hasher = Blake2s256::new();
        hasher.update(INPUT_DOMAIN);
        hasher.update(subject);
        hasher.update(counter.to_le_bytes());
        for byte in hasher.finalize() {
            let value = i32::from(byte % 129) - 64;
            values.push(value);
            input_bytes.extend_from_slice(&value.to_le_bytes());
        }
    }
    let mut output = [0i32; ROWS * COLS];
    let mut output_bytes = [0u8; WORK_OUTPUT_BYTES];
    for row in 0..ROWS {
        for col in 0..COLS {
            let mut dot = 0i32;
            for k in 0..K {
                dot += values[row * K + k] * values[(ROWS + col) * K + k];
            }
            let index = row * COLS + col;
            output[index] = dot;
            output_bytes[index * 4..index * 4 + 4].copy_from_slice(&dot.to_le_bytes());
        }
    }
    let input_digest = sha2_256(&input_bytes);
    let output_digest = sha2_256(&output_bytes);
    let mut message = Vec::with_capacity(COMMIT_DOMAIN.len() + 96);
    message.extend_from_slice(COMMIT_DOMAIN);
    message.extend_from_slice(&subject);
    message.extend_from_slice(&input_digest);
    message.extend_from_slice(&output_digest);
    CpuWorkResult {
        input_digest,
        output_digest,
        output,
        commitment: sha2_256(&message),
    }
}

/// Check the sealed candidate against the CPU reference result. No GPU proof.
pub fn validate_cpu_work(header: &Header) -> Result<CpuWorkResult, BindingError> {
    let certificate = validate_sealed_header(header)?;
    let result = recompute_cpu_work(certificate.subject);
    if result.commitment != certificate.work_commitment {
        return Err(BindingError::WorkCommitmentMismatch);
    }
    Ok(result)
}

fn is_engine_digest(item: &DigestItem) -> bool {
    match item {
        DigestItem::PreRuntime(id, _) | DigestItem::Consensus(id, _) | DigestItem::Seal(id, _) => {
            *id == ENGINE_ID
        }
        _ => false,
    }
}

/// Compute the one canonical subject of an unsealed candidate header.
///
/// The SCALE tuple contains a length-delimited domain, certificate version,
/// parent hash, block number, state root, extrinsics root and the complete
/// pre-seal header. Any EAGW digest is forbidden before the work seal.
pub fn candidate_subject(header: &Header) -> Result<[u8; 32], BindingError> {
    if header.digest().logs.iter().any(is_engine_digest) {
        return Err(BindingError::UnexpectedEngineDigest);
    }
    Ok(blake2_256(
        &(
            SUBJECT_DOMAIN,
            CERTIFICATE_VERSION,
            header.parent_hash(),
            header.number(),
            header.state_root(),
            header.extrinsics_root(),
            header,
        )
            .encode(),
    ))
}

/// Append a fixed-format final seal to a candidate with no prior EAGW digest.
pub fn seal_candidate(header: &Header, work_commitment: [u8; 32]) -> Result<Header, BindingError> {
    let certificate = WorkCertificate {
        version: CERTIFICATE_VERSION,
        subject: candidate_subject(header)?,
        work_commitment,
    };
    let mut sealed = header.clone();
    sealed
        .digest_mut()
        .push(DigestItem::Seal(ENGINE_ID, certificate.encode_fixed()));
    Ok(sealed)
}

/// Parse the unique final EAGW seal and verify that it binds this exact header.
///
/// This is a structural and subject check only, not validation of the committed GPU work.
pub fn validate_sealed_header(header: &Header) -> Result<WorkCertificate, BindingError> {
    let mut candidate = header.clone();
    let seal = candidate
        .digest_mut()
        .logs
        .pop()
        .ok_or(BindingError::MissingFinalSeal)?;
    let bytes = match seal {
        DigestItem::Seal(id, bytes) if id == ENGINE_ID => bytes,
        _ => return Err(BindingError::MissingFinalSeal),
    };
    if bytes.len() != CERTIFICATE_LEN {
        return Err(BindingError::InvalidCertificate);
    }
    let mut input = bytes.as_slice();
    let (version, subject, work_commitment) = <(u8, [u8; 32], [u8; 32])>::decode(&mut input)
        .map_err(|_| BindingError::InvalidCertificate)?;
    let certificate = WorkCertificate {
        version,
        subject,
        work_commitment,
    };
    if !input.is_empty() {
        return Err(BindingError::InvalidCertificate);
    }
    if certificate.version != CERTIFICATE_VERSION {
        return Err(BindingError::UnsupportedVersion);
    }
    let subject = candidate_subject(&candidate)?;
    if certificate.subject != subject {
        return Err(BindingError::SubjectMismatch);
    }
    Ok(certificate)
}

#[cfg(test)]
mod tests {
    use super::*;
    use sp_core::H256;
    use sp_runtime::generic::Digest;

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|b| format!("{b:02x}")).collect()
    }

    #[test]
    fn cpu_reference_matches_independent_zero_subject_vector() {
        let work = recompute_cpu_work([0; 32]);
        assert_eq!(
            hex(&work.input_digest),
            "ac840e3944d8225e59976a511bd9b5500dfe7e0d05815bb7c111ee6c0ce6fa58"
        );
        assert_eq!(
            hex(&work.output_digest),
            "a468777b3b511714124d77634ccdfd4e38698ac934f1ec38ce5df11446db10f9"
        );
        assert_eq!(
            hex(&work.commitment),
            "057388a35123ac823e6f64a8e57700d6e4c81493001fcb3429eba832f97dca33"
        );
        assert_eq!(&work.output[..4], &[48629, 102397, 83057, 113763]);
        assert_ne!(recompute_cpu_work([1; 32]).commitment, work.commitment);
    }

    #[test]
    fn cpu_check_rejects_altered_commitment_and_header() {
        let header = candidate();
        let result = recompute_cpu_work(candidate_subject(&header).unwrap());
        // Cross-checked against the bounded RTX 3090 execution artifact in
        // `.local/eagw-gpu-latest/output.bin`.
        assert_eq!(
            hex(&result.output_digest),
            "48ab4fcf64f4962c253b151320f9aab40f94921c74880645b719dcb5fb0526ba"
        );
        assert_eq!(
            hex(&result.commitment),
            "69884d757d92b3e0d71113eed30e48cfc62cdcdd2e9a1a857e95d377948dad7e"
        );
        let mut sealed = seal_candidate(&header, result.commitment).unwrap();
        assert_eq!(validate_cpu_work(&sealed), Ok(result));
        if let Some(DigestItem::Seal(_, bytes)) = sealed.digest_mut().logs.last_mut() {
            bytes[33] ^= 1;
        }
        assert_eq!(
            validate_cpu_work(&sealed),
            Err(BindingError::WorkCommitmentMismatch)
        );
        sealed.set_number(43);
        assert_eq!(
            validate_cpu_work(&sealed),
            Err(BindingError::SubjectMismatch)
        );
    }

    fn candidate() -> Header {
        Header::new(
            42,
            H256::repeat_byte(1),
            H256::repeat_byte(2),
            H256::repeat_byte(3),
            Digest::default(),
        )
    }

    #[test]
    fn roundtrip_is_deterministic_and_commitment_is_opaque() {
        let header = candidate();
        let subject = candidate_subject(&header).unwrap();
        assert_eq!(
            subject,
            [
                0x81, 0x85, 0xe4, 0xc0, 0x72, 0x87, 0x9c, 0x99, 0xa0, 0xb5, 0x90, 0x1f, 0x1e, 0x07,
                0x70, 0xed, 0x24, 0xe5, 0x8b, 0x0f, 0x68, 0x32, 0x3c, 0xc2, 0xfd, 0xfc, 0x08, 0x7e,
                0xe2, 0xa2, 0x42, 0xf6
            ]
        );
        assert_eq!(subject, candidate_subject(&header).unwrap());
        let sealed = seal_candidate(&header, [7; 32]).unwrap();
        let cert = validate_sealed_header(&sealed).unwrap();
        assert_eq!(cert.subject, subject);
        assert_eq!(cert.work_commitment, [7; 32]);
        assert_eq!(cert.encode_fixed().len(), CERTIFICATE_LEN);
        assert_eq!(header.digest().logs.len(), 0);
    }

    #[test]
    fn every_header_field_and_digest_affects_subject() {
        let original = seal_candidate(&candidate(), [0; 32]).unwrap();
        let mut variants = Vec::new();
        let mut h = original.clone();
        h.set_parent_hash(H256::repeat_byte(4));
        variants.push(h);
        let mut h = original.clone();
        h.set_number(43);
        variants.push(h);
        let mut h = original.clone();
        h.set_state_root(H256::repeat_byte(4));
        variants.push(h);
        let mut h = original.clone();
        h.set_extrinsics_root(H256::repeat_byte(4));
        variants.push(h);
        let mut h = original.clone();
        h.digest_mut().logs.insert(0, DigestItem::Other(vec![1]));
        variants.push(h);
        for h in variants {
            assert_eq!(
                validate_sealed_header(&h),
                Err(BindingError::SubjectMismatch)
            );
        }
    }

    #[test]
    fn rejects_missing_non_final_duplicate_and_other_eagw_digest() {
        let h = candidate();
        assert_eq!(
            validate_sealed_header(&h),
            Err(BindingError::MissingFinalSeal)
        );
        let mut sealed = seal_candidate(&h, [1; 32]).unwrap();
        sealed.digest_mut().push(DigestItem::Other(vec![1]));
        assert_eq!(
            validate_sealed_header(&sealed),
            Err(BindingError::MissingFinalSeal)
        );
        for digest in [
            DigestItem::Seal(ENGINE_ID, vec![]),
            DigestItem::PreRuntime(ENGINE_ID, vec![]),
            DigestItem::Consensus(ENGINE_ID, vec![]),
        ] {
            let mut bad = candidate();
            bad.digest_mut().push(digest);
            assert_eq!(
                candidate_subject(&bad),
                Err(BindingError::UnexpectedEngineDigest)
            );
            let mut duplicate = seal_candidate(&h, [0; 32]).unwrap();
            duplicate
                .digest_mut()
                .logs
                .insert(0, bad.digest().logs[0].clone());
            assert_eq!(
                validate_sealed_header(&duplicate),
                Err(BindingError::UnexpectedEngineDigest)
            );
        }
    }

    #[test]
    fn rejects_bad_length_version_and_subject() {
        let valid = seal_candidate(&candidate(), [9; 32]).unwrap();
        for change in 0..4 {
            let mut h = valid.clone();
            if let Some(DigestItem::Seal(_, bytes)) = h.digest_mut().logs.last_mut() {
                match change {
                    0 => {
                        bytes.pop();
                    }
                    1 => bytes.push(0),
                    2 => bytes[0] = 2,
                    _ => bytes[1] ^= 1,
                }
            }
            let expected = match change {
                0 | 1 => BindingError::InvalidCertificate,
                2 => BindingError::UnsupportedVersion,
                _ => BindingError::SubjectMismatch,
            };
            assert_eq!(validate_sealed_header(&h), Err(expected));
        }
    }
}
