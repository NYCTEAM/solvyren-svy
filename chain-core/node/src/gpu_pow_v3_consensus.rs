//! Experimental V3 adapter for Substrate's PoW trait and strict import header
//! rules. The opt-in service import queue uses this code, but authoring and
//! live PoW/finality validation are still incomplete. The V3 workload is
//! CPU-computable and does not prove that a GPU executed it.
//!
//! The caller must use a verified chain-specific genesis hash and identical
//! difficulty rules on every node. The standard PowBlockImport calculates pre_hash
//! from the actual unsealed header and parent from that same header; this trait
//! alone cannot establish those facts for an arbitrary external caller.

use crate::gpu_pow_v3;
use eagle_runtime::opaque::Block;
use sc_consensus::{block_import::BlockImportParams, import_queue::Verifier};
use sc_consensus_pow::{Error, PowAlgorithm, PowAux};
use sp_api::ProvideRuntimeApi;
use sp_consensus_pow::TimestampApi as _;
use sp_core::{H256, U256};
use sp_runtime::{generic::BlockId, traits::Header as HeaderT, DigestItem};
use std::{future::Future, pin::Pin, sync::Arc};

const POW_ENGINE_ID: [u8; 4] = *b"pow_";
const POW_SEALED_DIGEST_ITEMS: usize = eagle_runtime::POW_UNSEALED_DIGEST_ITEMS + 1;

fn validate_sealed_pow_digest(logs: &[DigestItem]) -> Result<(), String> {
    if logs.len() != POW_SEALED_DIGEST_ITEMS {
        return Err("gpu-pow-v3 requires exactly one beneficiary and one final seal".into());
    }
    match &logs[0] {
        DigestItem::PreRuntime(id, bytes) if *id == POW_ENGINE_ID => {
            eagle_runtime::PowBeneficiaryV1::decode_payload(bytes).map_err(str::to_owned)?;
        }
        _ => return Err("gpu-pow-v3 requires the beneficiary as the only pre-seal digest".into()),
    }
    match &logs[1] {
        DigestItem::Seal(id, bytes) if *id == POW_ENGINE_ID => {
            gpu_pow_v3::Seal::decode_fixed(bytes)
                .map_err(|_| "gpu-pow-v3 requires one exact 41-byte V3 seal".to_owned())?;
            Ok(())
        }
        _ => Err("gpu-pow-v3 requires one final PoW seal".into()),
    }
}

/// Placeholder initial difficulty for the isolated V3 experiment. This must
/// be calibrated against measured GPU and CPU search rates before a sustained
/// mining test; it is not a production network difficulty.
pub const EXPERIMENTAL_DIFFICULTY: u128 = 4;

/// Measured RTX3090 batch throughput motivates a harder V3 genesis than the
/// V2 four-attempt demonstration. This remains a development-chain starting
/// point, subject to independent two-GPU/CPU calibration and retarget review.
pub const V3_INITIAL_DIFFICULTY: u128 = 1u128 << 32;

/// Fixed-target test adapter's 256-bit big-endian target.
pub const EXPERIMENTAL_TARGET: [u8; 32] = {
    let mut target = [0xff; 32];
    target[0] = 0x3f;
    target
};

/// Canonical difficulty-to-target mapping used by authoring and verification.
/// A larger difficulty always produces a smaller target.
pub(crate) fn target_for_difficulty(difficulty: u128) -> Result<[u8; 32], &'static str> {
    if difficulty == 0 {
        return Err("PoW difficulty must be non-zero");
    }
    Ok((U256::MAX / U256::from(difficulty)).to_big_endian())
}

const TARGET_BLOCK_MILLIS: u64 = 10_000;
const RETARGET_BLOCKS: u32 = 60;
const SAMPLE_INTERVALS: u32 = RETARGET_BLOCKS - 1;
const MAX_EXPERIMENTAL_DIFFICULTY: u128 = 1u128 << 120;

fn retarget_difficulty(old: u128, elapsed_millis: u64) -> Result<u128, &'static str> {
    if old == 0 || old > MAX_EXPERIMENTAL_DIFFICULTY {
        return Err("parent PoW difficulty is outside the experimental range");
    }
    if elapsed_millis == 0 {
        return Err("PoW retarget elapsed time must be non-zero");
    }
    let expected = u64::from(SAMPLE_INTERVALS)
        .checked_mul(TARGET_BLOCK_MILLIS)
        .ok_or("PoW retarget expected time overflow")?;
    let bounded_elapsed = elapsed_millis.clamp(expected / 2, expected * 2);
    let adjusted = U256::from(old) * U256::from(expected) / U256::from(bounded_elapsed);
    Ok(adjusted
        .max(U256::one())
        .min(U256::from(MAX_EXPERIMENTAL_DIFFICULTY))
        .as_u128())
}

/// Branch-local experimental retargeting. Every sixtieth parent recomputes the
/// next difficulty from the 59 timestamp intervals between blocks #1 and #60
/// (and equivalent later windows). Other blocks inherit their parent's value.
/// This is deterministic consensus plumbing, not calibrated production policy.
#[derive(Clone)]
pub(crate) struct DynamicDifficultyAlgorithm {
    genesis: H256,
    client: Arc<crate::service::FullClient>,
}

impl DynamicDifficultyAlgorithm {
    pub(crate) fn new(genesis: H256, client: Arc<crate::service::FullClient>) -> Self {
        Self { genesis, client }
    }

    fn header(&self, hash: H256) -> Result<eagle_runtime::Header, Error<Block>> {
        self.client
            .header(hash)
            .map_err(Error::Client)?
            .ok_or_else(|| Error::Other(format!("missing PoW header {hash:?}")))
    }

    fn timestamp(&self, hash: H256) -> Result<u64, Error<Block>> {
        self.client
            .runtime_api()
            .timestamp(hash)
            .map_err(|error| Error::Runtime(format!("timestamp API at {hash:?}: {error}")))
    }
}

impl PowAlgorithm<Block> for DynamicDifficultyAlgorithm {
    type Difficulty = u128;

    fn difficulty(&self, parent: H256) -> Result<Self::Difficulty, Error<Block>> {
        let parent_header = self.header(parent)?;
        let parent_number = *parent_header.number();
        if parent == self.genesis {
            if parent_number != 0 {
                return Err(Error::Other(
                    "configured PoW genesis has a non-zero height".into(),
                ));
            }
            return Ok(V3_INITIAL_DIFFICULTY);
        }
        if parent_number == 0 {
            return Err(Error::Other(
                "height-zero parent is not the configured PoW genesis".into(),
            ));
        }

        let parent_aux = PowAux::<u128>::read_optional::<_, Block>(self.client.as_ref(), &parent)?
            .ok_or_else(|| Error::Other(format!("missing PoW aux for parent {parent:?}")))?;
        let old = parent_aux.difficulty;
        if old == 0 || old > MAX_EXPERIMENTAL_DIFFICULTY {
            return Err(Error::Other(
                "parent PoW difficulty is outside the experimental range".into(),
            ));
        }
        if parent_aux.total_difficulty < old {
            return Err(Error::Other(
                "parent PoW cumulative difficulty is smaller than its block difficulty".into(),
            ));
        }
        if parent_number % RETARGET_BLOCKS != 0 {
            return Ok(old);
        }

        let newest_timestamp = self.timestamp(parent)?;
        let mut later_timestamp = newest_timestamp;
        let mut cursor = parent;
        for _ in 0..SAMPLE_INTERVALS {
            let header = self.header(cursor)?;
            let ancestor = *header.parent_hash();
            let earlier_timestamp = self.timestamp(ancestor)?;
            if earlier_timestamp >= later_timestamp {
                return Err(Error::Other(
                    "PoW retarget window timestamps must be strictly increasing".into(),
                ));
            }
            later_timestamp = earlier_timestamp;
            cursor = ancestor;
        }

        let elapsed = newest_timestamp
            .checked_sub(later_timestamp)
            .ok_or_else(|| Error::Other("PoW retarget elapsed time underflow".into()))?;
        retarget_difficulty(old, elapsed).map_err(|error| Error::Other(error.into()))
    }

    fn verify(
        &self,
        _parent: &BlockId<Block>,
        pre_hash: &H256,
        pre_digest: Option<&[u8]>,
        seal: &Vec<u8>,
        difficulty: Self::Difficulty,
    ) -> Result<bool, Error<Block>> {
        let Some(pre_digest) = pre_digest else {
            return Ok(false);
        };
        if eagle_runtime::PowBeneficiaryV1::decode_payload(pre_digest).is_err() {
            return Ok(false);
        }
        let target =
            target_for_difficulty(difficulty).map_err(|error| Error::Other(error.into()))?;
        Ok(gpu_pow_v3::verify_pre_hash(self.genesis, *pre_hash, seal, target).is_ok())
    }
}

/// Fixed, externally configured context. It is never decoded from a seal.
#[derive(Clone, Debug)]
pub struct FixedTargetAlgorithm {
    genesis: H256,
}

impl FixedTargetAlgorithm {
    /// Construct using consensus configuration trusted by all participants.
    pub fn new(genesis: H256) -> Self {
        Self { genesis }
    }
}

impl PowAlgorithm<Block> for FixedTargetAlgorithm {
    type Difficulty = u128;

    fn difficulty(&self, _parent: H256) -> Result<Self::Difficulty, Error<Block>> {
        Ok(EXPERIMENTAL_DIFFICULTY)
    }

    fn verify(
        &self,
        _parent: &BlockId<Block>,
        pre_hash: &H256,
        pre_digest: Option<&[u8]>,
        seal: &Vec<u8>,
        difficulty: Self::Difficulty,
    ) -> Result<bool, Error<Block>> {
        if difficulty != EXPERIMENTAL_DIFFICULTY {
            return Ok(false);
        }
        let Some(pre_digest) = pre_digest else {
            return Ok(false);
        };
        if eagle_runtime::PowBeneficiaryV1::decode_payload(pre_digest).is_err() {
            return Ok(false);
        }
        let target =
            target_for_difficulty(difficulty).map_err(|error| Error::Other(error.into()))?;
        Ok(gpu_pow_v3::verify_pre_hash(self.genesis, *pre_hash, seal, target).is_ok())
    }
    // No mining implementation is provided by this trait or this adapter.
    // GPU/CPU workers must construct and submit candidate seals separately.
}

/// Enforces the isolated PoW header shape before delegating to the SDK
/// verifier. In particular, an Aura pre-runtime digest cannot reactivate the
/// Aura slot semantics that the V3 runtime deliberately removed.
pub(crate) struct StrictPowVerifier<Algorithm> {
    inner: sc_consensus_pow::PowVerifier<Block, Algorithm>,
}

impl<Algorithm> StrictPowVerifier<Algorithm> {
    pub(crate) fn new(algorithm: Algorithm) -> Self {
        Self {
            inner: sc_consensus_pow::PowVerifier::new(algorithm),
        }
    }
}

impl<Algorithm> Verifier<Block> for StrictPowVerifier<Algorithm>
where
    Algorithm: PowAlgorithm<Block> + Send + Sync,
    Algorithm::Difficulty: 'static + Send,
{
    fn verify<'life0, 'async_trait>(
        &'life0 self,
        block: BlockImportParams<Block>,
    ) -> Pin<Box<dyn Future<Output = Result<BlockImportParams<Block>, String>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            if block.with_state()
                || block.state_action.skip_execution_checks()
                || !block.post_digests.is_empty()
            {
                return Err("gpu-pow-v3 requires full execution and an unprocessed header".into());
            }

            let original = block.header.clone();
            let full_hash = original.hash();
            let logs = &original.digest().logs;
            validate_sealed_pow_digest(logs)?;

            let checked = self.inner.verify(block).await?;
            let mut reconstructed = checked.header.clone();
            for item in &checked.post_digests {
                reconstructed.digest_mut().push(item.clone());
            }
            if reconstructed != original || reconstructed.hash() != full_hash {
                return Err("gpu-pow-v3 verifier changed the original header".into());
            }
            Ok(checked)
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use eagle_runtime::Header;
    use sp_runtime::{generic::Digest, traits::Header as HeaderT};

    #[test]
    fn fixed_adapter_accepts_one_v3_ticket_and_rejects_wrong_context() {
        let beneficiary =
            eagle_runtime::PowBeneficiaryV1::new(eagle_runtime::AccountId::new([11; 32]))
                .encode_payload();
        let genesis = H256::repeat_byte(7);
        let parent = H256::repeat_byte(3);
        let header = Header::new(
            42,
            H256::repeat_byte(1),
            H256::repeat_byte(2),
            parent,
            Digest {
                logs: vec![DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary.clone())],
            },
        );
        let pre_hash = header.hash();
        let challenge = gpu_pow_v3::challenge_for_header(genesis, &header).unwrap();
        let nonce = (0..64)
            .find(|nonce| gpu_pow_v3::ticket(challenge, *nonce) <= EXPERIMENTAL_TARGET)
            .expect("bounded fixed-target vector must include a winner");
        let seal = gpu_pow_v3::Seal { challenge, nonce }
            .encode_fixed()
            .to_vec();
        let algorithm = FixedTargetAlgorithm::new(genesis);
        let parent_id = BlockId::hash(parent);
        assert_eq!(target_for_difficulty(4).unwrap(), EXPERIMENTAL_TARGET);
        assert!(algorithm
            .verify(&parent_id, &pre_hash, Some(&beneficiary), &seal, 4)
            .unwrap());
        assert!(!algorithm
            .verify(&parent_id, &pre_hash, Some(&beneficiary), &seal, 2)
            .unwrap());
        assert!(!algorithm
            .verify(&parent_id, &H256::zero(), Some(&beneficiary), &seal, 4)
            .unwrap());
        assert!(!algorithm
            .verify(&parent_id, &pre_hash, None, &seal, 4)
            .unwrap());
        let mut wrong_version = seal.clone();
        wrong_version[0] = 2;
        assert!(validate_sealed_pow_digest(&[
            DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary.clone()),
            DigestItem::Seal(POW_ENGINE_ID, wrong_version),
        ])
        .is_err());
        assert!(validate_sealed_pow_digest(&[
            DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary.clone()),
            DigestItem::Seal(POW_ENGINE_ID, seal.clone()),
        ])
        .is_ok());
        for extra in [
            DigestItem::Other(vec![0]),
            DigestItem::Consensus(*b"test", vec![0]),
        ] {
            assert!(validate_sealed_pow_digest(&[
                DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary.clone()),
                extra,
                DigestItem::Seal(POW_ENGINE_ID, seal.clone()),
            ])
            .is_err());
        }
    }
}
