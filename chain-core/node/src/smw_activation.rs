//! The mainnet fork is part of the chain's block-validation history.
use sp_core::H256;
use crate::{gpu_pow, gpu_pow_mx10 as mx};

const MAINNET: [u8; 32] = [0x67,0x81,0x07,0xbc,0x4d,0x21,0xa4,0xe7,0xf9,0xb1,0x2e,0xe8,0x17,0xaf,0x0c,0x93,0xf0,0x74,0xcf,0x04,0xfb,0xd7,0xee,0x71,0x69,0x76,0xaf,0x42,0xca,0x92,0xf4,0xc8];

pub fn scheduled(genesis: H256) -> Option<u64> {
    (genesis.as_bytes() == MAINNET).then_some(10_000)
}

pub fn active(first: Option<u64>, parent_height: u64) -> bool {
    first.is_some_and(|height| parent_height >= height - 1)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn smw_mainnet_boundary() {
        let mainnet = scheduled(H256::from(MAINNET));
        assert!(!active(mainnet, 9_998));
        assert!(active(mainnet, 9_999));
        assert!(active(mainnet, 10_000));
        let testnet = crate::mx10_context::PUBLICDEV_GENESIS.parse().unwrap();
        assert_eq!(scheduled(testnet), None);
    }
}

fn shape() -> mx::EvalShape { mx::EvalShape::new(16, 2048).expect("fixed SMW profile") }

pub fn verify(genesis: H256, pre_hash: H256, seal: &[u8], target: [u8; 32]) -> bool {
    mx::verify_pre_hash(shape(), genesis, pre_hash, seal, target).is_ok()
}

pub fn candidate(smw: bool, challenge: [u8; 32], nonce: u64, target: [u8; 32]) -> Option<Vec<u8>> {
    if smw {
        let proof = mx::compute(shape(), challenge, nonce);
        (mx::ticket(&proof) <= target).then(|| proof.encode_fixed())
    } else {
        let proof = gpu_pow::compute(challenge, nonce);
        (gpu_pow::ticket(&proof) <= target).then(|| proof.encode_fixed().to_vec())
    }
}
