//! Version 3 fixed-size hash ticket for an isolated PoW chain.
//!
//! Verification recomputes one ticket, regardless of the number of nonces a
//! miner searched. A valid ticket does not attest that a GPU performed the
//! search: CPU miners can compute the same SHA-256 function.

use eagle_runtime::Header;
use sp_core::{hashing::sha2_256, H256};
use sp_runtime::{generic::DigestItem, traits::Header as HeaderT};

pub const VERSION: u8 = 3;
pub const SEAL_BYTES: usize = 1 + 32 + 8;
const CHALLENGE_DOMAIN: &[u8] = b"EAGW-POW-V3-CHALLENGE\0";
const TICKET_DOMAIN: &[u8] = b"EAGW-POW-V3-TICKET\0";

/// Exact wire format: version byte, 32-byte challenge, little-endian u64 nonce.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct Seal {
    pub challenge: [u8; 32],
    pub nonce: u64,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub enum Error {
    HeaderAlreadySealed,
    InvalidEncoding,
    UnsupportedVersion,
    ChallengeMismatch,
    AboveTarget,
}

impl Seal {
    pub fn encode_fixed(&self) -> [u8; SEAL_BYTES] {
        let mut encoded = [0u8; SEAL_BYTES];
        encoded[0] = VERSION;
        encoded[1..33].copy_from_slice(&self.challenge);
        encoded[33..41].copy_from_slice(&self.nonce.to_le_bytes());
        encoded
    }

    pub fn decode_fixed(encoded: &[u8]) -> Result<Self, Error> {
        if encoded.len() != SEAL_BYTES {
            return Err(Error::InvalidEncoding);
        }
        if encoded[0] != VERSION {
            return Err(Error::UnsupportedVersion);
        }
        let mut challenge = [0u8; 32];
        challenge.copy_from_slice(&encoded[1..33]);
        let mut nonce = [0u8; 8];
        nonce.copy_from_slice(&encoded[33..41]);
        Ok(Self {
            challenge,
            nonce: u64::from_le_bytes(nonce),
        })
    }
}

/// The caller must derive `pre_hash` from the actual unsealed header.
pub fn challenge_from_pre_hash(genesis: H256, pre_hash: H256) -> [u8; 32] {
    let mut message = Vec::with_capacity(CHALLENGE_DOMAIN.len() + 1 + 64);
    message.extend_from_slice(CHALLENGE_DOMAIN);
    message.push(VERSION);
    message.extend_from_slice(genesis.as_bytes());
    message.extend_from_slice(pre_hash.as_bytes());
    sha2_256(&message)
}

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

/// SHA-256(domain || challenge || nonce_le). The result is unsigned big-endian.
pub fn ticket(challenge: [u8; 32], nonce: u64) -> [u8; 32] {
    let mut message = Vec::with_capacity(TICKET_DOMAIN.len() + 32 + 8);
    message.extend_from_slice(TICKET_DOMAIN);
    message.extend_from_slice(&challenge);
    message.extend_from_slice(&nonce.to_le_bytes());
    sha2_256(&message)
}

/// Inclusive, unsigned big-endian target comparison.
pub fn verify_pre_hash(
    genesis: H256,
    pre_hash: H256,
    encoded: &[u8],
    target: [u8; 32],
) -> Result<[u8; 32], Error> {
    let seal = Seal::decode_fixed(encoded)?;
    let expected = challenge_from_pre_hash(genesis, pre_hash);
    if seal.challenge != expected {
        return Err(Error::ChallengeMismatch);
    }
    let score = ticket(expected, seal.nonce);
    if score > target {
        return Err(Error::AboveTarget);
    }
    Ok(score)
}

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

    fn hex(bytes: &[u8]) -> String {
        bytes.iter().map(|byte| format!("{byte:02x}")).collect()
    }

    #[test]
    fn python_vector_pins_domains_and_nonce_endianness() {
        let genesis = H256::from_slice(&(0u8..32).collect::<Vec<_>>());
        let pre_hash = H256::from_slice(&(32u8..64).collect::<Vec<_>>());
        let challenge = challenge_from_pre_hash(genesis, pre_hash);
        assert_eq!(
            hex(&challenge),
            "31415955b1b9cdc248736d1e55053d503cae6d38509d2aad81799af41ecca740"
        );
        let nonce = 0x0102_0304_0506_0708;
        let score = ticket(challenge, nonce);
        assert_eq!(
            hex(&score),
            "62903a15758f688144d47b4307dc7b6f6414ce0b151255ae9f643fce0b4f9dee"
        );
        let encoded = Seal { challenge, nonce }.encode_fixed();
        assert_eq!(&encoded[33..], &[8, 7, 6, 5, 4, 3, 2, 1]);
        assert_eq!(Seal::decode_fixed(&encoded), Ok(Seal { challenge, nonce }));
        assert_eq!(
            verify_pre_hash(genesis, pre_hash, &encoded, score),
            Ok(score)
        );
        assert_eq!(
            verify_pre_hash(genesis, pre_hash, &encoded, [0; 32]),
            Err(Error::AboveTarget)
        );
        assert_eq!(
            verify_pre_hash(H256::zero(), pre_hash, &encoded, [255; 32]),
            Err(Error::ChallengeMismatch)
        );
        assert_eq!(
            Seal::decode_fixed(&encoded[..40]),
            Err(Error::InvalidEncoding)
        );
        let mut wrong_version = encoded;
        wrong_version[0] = VERSION - 1;
        assert_eq!(
            Seal::decode_fixed(&wrong_version),
            Err(Error::UnsupportedVersion)
        );
    }
}
