//! Experimental V2 adapter for Substrate's PoW trait and strict import header
//! rules. The opt-in service import queue uses this code, but authoring and
//! live PoW/finality validation are still incomplete. The V2 workload is
//! CPU-computable and does not prove that a GPU executed it.
//!
//! The caller must use a verified chain-specific genesis hash and identical
//! difficulty rules on every node. The standard PowBlockImport calculates pre_hash
//! from the actual unsealed header and parent from that same header; this trait
//! alone cannot establish those facts for an arbitrary external caller.

use crate::gpu_pow;
use eagle_runtime::opaque::Block;
use sc_consensus::{block_import::BlockImportParams, import_queue::Verifier};
use sc_consensus_pow::{Error, PowAlgorithm, PowAux};
use sp_api::ProvideRuntimeApi;
use sp_consensus_pow::TimestampApi as _;
use sp_core::{H256, U256};
use sp_runtime::{generic::BlockId, traits::Header as HeaderT, DigestItem};
use std::{future::Future, pin::Pin, sync::Arc};

/// Trusted dispatch configuration, never decoded from certificate bytes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum WorkAlgorithm { V2, Mx10N16(crate::mx10_context::Mx10ChainContext) }

impl WorkAlgorithm {
    #[cfg(feature = "mainnet-release")]
    pub(crate) fn from_spec(_: &dyn sc_service::ChainSpec) -> Result<Self, String> {
        Ok(Self::V2)
    }

    #[cfg(not(feature = "mainnet-release"))]
    pub(crate) fn from_spec(spec: &dyn sc_service::ChainSpec) -> Result<Self, String> {
        let properties = spec.properties();
        let work = properties.get("workAlgorithm").and_then(|v| v.as_str());
        if crate::chain_spec::is_mx10_isolated_id(spec.id()) {
            return crate::mx10_context::Mx10ChainContext::isolated(spec).map(Self::Mx10N16);
        }
        if work == Some(crate::chain_spec::MX10_WORK_ALGORITHM) {
            return Err("MX10 workAlgorithm requires its new isolated chain identity".into());
        }
        if spec.id() == crate::chain_spec::POW_MATRIX_BLOCK_REWARDS_LOCAL_ID && work != Some("integer-matrix-2x2048x16") {
            return Err("V2 matrix chain requires its unchanged workAlgorithm".into());
        }
        Ok(Self::V2)
    }

    pub(crate) fn verify(self, genesis: H256, pre_hash: H256, seal: &[u8], target: [u8; 32]) -> bool {
        match self {
            Self::V2 => gpu_pow::verify_pre_hash(genesis, pre_hash, seal, target).is_ok(),
            #[cfg(feature = "mainnet-release")]
            Self::Mx10N16(_) => false,
            #[cfg(not(feature = "mainnet-release"))]
            Self::Mx10N16(context) => {
                if context.ensure_genesis(genesis).is_err() { return false; }
                let start = std::time::Instant::now();
                let result = context.verify(pre_hash, seal, target);
                let elapsed_ns = start.elapsed().as_nanos();
                if std::env::var_os("MX10_T3_TRACE").as_deref() == Some(std::ffi::OsStr::new("1")) {
                    eprintln!("MX10_VERIFY_PRE_HASH genesis={:?} pre_hash={:?} elapsed_ns={} result={:?}", genesis, pre_hash, elapsed_ns, result);
                }
                result.is_ok()
            }
        }
    }
}

const POW_ENGINE_ID: [u8; 4] = *b"pow_";
const POW_SEALED_DIGEST_ITEMS: usize = eagle_runtime::POW_UNSEALED_DIGEST_ITEMS + 1;

fn validate_sealed_pow_digest(logs: &[DigestItem]) -> Result<(), String> {
    if logs.len() != POW_SEALED_DIGEST_ITEMS {
        return Err("gpu-pow-v2 requires exactly one beneficiary and one final seal".into());
    }
    match &logs[0] {
        DigestItem::PreRuntime(id, bytes) if *id == POW_ENGINE_ID => {
            eagle_runtime::PowBeneficiaryV1::decode_payload(bytes).map_err(str::to_owned)?;
        }
        _ => return Err("gpu-pow-v2 requires the beneficiary as the only pre-seal digest".into()),
    }
    match &logs[1] {
        DigestItem::Seal(id, _) if *id == POW_ENGINE_ID => Ok(()),
        _ => Err("gpu-pow-v2 requires one final PoW seal".into()),
    }
}

/// Constant difficulty used for each block of this fixed-target experiment.
/// Since all valid blocks use the same configured target they have equal
/// nominal weight. Dynamic adjustment is not enabled yet.
pub const EXPERIMENTAL_DIFFICULTY: u128 = 4;

/// Experimental fixed 256-bit big-endian target: 0x3f followed by 31 0xff bytes.
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
const MAINNET_GENESIS: &str =
    "0x678107bc4d21a4e7f9b12ee817af0c93f074cf04fbd7ee716976af42ca92f4c8";
const MAINNET_ROLLING_RETARGET_BLOCKS: u32 = 10;
// First block using the new rule, set in the coordinated network release.
const MAINNET_ROLLING_RETARGET_ACTIVATION: Option<u32> = Some(24_121);
const _: () = {
    if let Some(height) = MAINNET_ROLLING_RETARGET_ACTIVATION {
        assert!(height > MAINNET_ROLLING_RETARGET_BLOCKS);
    }
};
/// Closed-testnet activation for the verified-share pool.  The existing chain
/// reached difficulty one while authoring only one small GPU batch per block,
/// so ordinary interval retargeting cannot make a continuously searching GPU
/// busy.  At this explicit height all upgraded nodes deterministically seed the
/// difficulty from the measured MX10 throughput; subsequent windows use the
/// normal timestamp DAA again.
const MX10_SHARE_POOL_ACTIVATION_HEIGHT: u32 = 13_001;
// Measured on the closed-testnet RTX 3090 at 322,466.98 MX10 proofs/s with
// consensus-identical n=16 continuous 4,096-proof batches. 3,225,000 work
// units targets roughly ten seconds
// for the first share-pool block; the normal 60-block DAA takes over after it.
const MX10_SHARE_POOL_BOOTSTRAP_DIFFICULTY: u128 = 3_225_000;
/// T5 continuous retarget window: the window ending at parent height 60k
/// covers all 60 intervals back to height 60(k-1), including the former
/// 60(k-1) -> 60(k-1)+1 seam. The first window starts at the genesis block;
/// if the window start's timestamp is 0 (unset genesis `Timestamp::Now`) the
/// retarget is skipped and the parent difficulty is kept (T5-02).
const SAMPLE_INTERVALS: u32 = RETARGET_BLOCKS;
// expected = 60 x 10_000 = 600_000 ms; clamp = [300_000, 1_200_000] ms.
const _: () = assert!(SAMPLE_INTERVALS as u64 * TARGET_BLOCK_MILLIS == 600_000);
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

fn rolling_difficulty(old: u128, work: U256, elapsed_millis: u64) -> u128 {
    let target = work * U256::from(TARGET_BLOCK_MILLIS) / U256::from(elapsed_millis);
    let lower = (old * 2 / 3).max(1);
    let upper = (old * 3).div_ceil(2).min(MAX_EXPERIMENTAL_DIFFICULTY);
    target.clamp(U256::from(lower), U256::from(upper)).as_u128()
}

/// Chain view read by [`DynamicDifficultyAlgorithm`]. Production uses the
/// node's `FullClient`, exactly as constructed in `service.rs`; tests may
/// supply an in-memory chain to drive the very same `PowAlgorithm::difficulty`.
pub(crate) trait DifficultyChain: Send + Sync {
    fn pow_header(&self, hash: H256) -> Result<eagle_runtime::Header, Error<Block>>;
    fn pow_timestamp(&self, hash: H256) -> Result<u64, Error<Block>>;
    fn pow_aux(&self, hash: H256) -> Result<Option<PowAux<u128>>, Error<Block>>;
    fn pow_asert_anchor(&self, _hash: H256) -> Result<Option<(u64,u128)>, Error<Block>> { Ok(None) }
    fn pow_store_asert_anchor(&self, _hash: H256, _value: (u64,u128)) -> Result<(), Error<Block>> { Ok(()) }
}

impl DifficultyChain for crate::service::FullClient {
    fn pow_header(&self, hash: H256) -> Result<eagle_runtime::Header, Error<Block>> {
        self.header(hash)
            .map_err(Error::Client)?
            .ok_or_else(|| Error::Other(format!("missing PoW header {hash:?}")))
    }

    fn pow_timestamp(&self, hash: H256) -> Result<u64, Error<Block>> {
        self.runtime_api()
            .timestamp(hash)
            .map_err(|error| Error::Runtime(format!("timestamp API at {hash:?}: {error}")))
    }

    fn pow_aux(&self, hash: H256) -> Result<Option<PowAux<u128>>, Error<Block>> {
        PowAux::<u128>::read_optional::<_, Block>(self, &hash)
    }
    fn pow_asert_anchor(&self, hash: H256) -> Result<Option<(u64,u128)>, Error<Block>> {
        use sc_client_api::AuxStore;
        use sp_runtime::codec::{Encode, Decode};
        let key=(b"solvyren/asert-10s-300s/v1",crate::adaptive_difficulty::ACTIVATION_HEIGHT,hash).encode();
        self.get_aux(&key).map_err(Error::Client)?.map(|value| {
            let mut bytes=&value[..];
            let decoded=<(u64,u128)>::decode(&mut bytes).map_err(|_|Error::Other("invalid persisted ASERT anchor".into()))?;
            if !bytes.is_empty() { return Err(Error::Other("trailing ASERT anchor bytes".into())); }
            Ok(decoded)
        }).transpose()
    }
    fn pow_store_asert_anchor(&self, hash: H256, value:(u64,u128)) -> Result<(),Error<Block>> {
        use sc_client_api::AuxStore;
        use sp_runtime::codec::Encode;
        let key=(b"solvyren/asert-10s-300s/v1",crate::adaptive_difficulty::ACTIVATION_HEIGHT,hash).encode();
        let data=value.encode();
        self.insert_aux(&[(&key[..],&data[..])],&[]).map_err(Error::Client)
    }
}

/// Branch-local retargeting with a continuous window (T5).
/// Under the original rule, when the parent height is 60k (k >= 1), difficulty is recomputed
/// from elapsed = ts(60k) - ts(60(k-1)) over all 60 intervals; the first
/// window starts at the genesis timestamp. If the window start's timestamp is
/// 0 (genesis without `Timestamp::Now`), the retarget is skipped and the
/// parent difficulty is returned unchanged; all window checks still run first.
/// Every adjacent pair in the window,
/// including the former 60(k-1) -> 60(k-1)+1 seam, must be strictly
/// increasing. elapsed is clamped to [300_000, 1_200_000] ms around an
/// expected 600_000 ms. Other blocks inherit their parent's value.
/// After mainnet activation, each block uses the cumulative work and elapsed
/// time of the previous 10 intervals. The factor-of-1.5 bounds limit reactions
/// to atypical timestamps and short-lived changes in hash rate.
/// The future-drift limit and the adjacent MinimumPeriod are deliberately not
/// duplicated here: they stay enforced by the inherited pallet-timestamp
/// 42.0.0 check_inherent (MAX_TIMESTAMP_DRIFT_MILLIS, TooFarInFuture) and
/// Runtime MinimumPeriod rules on the same import path.
pub(crate) struct DynamicDifficultyAlgorithm<C = crate::service::FullClient> {
    genesis: H256,
    client: Arc<C>,
    work_algorithm: WorkAlgorithm,
}

impl<C> Clone for DynamicDifficultyAlgorithm<C> {
    fn clone(&self) -> Self {
        Self {
            genesis: self.genesis,
            client: self.client.clone(),
            work_algorithm: self.work_algorithm,
        }
    }
}

impl<C> DynamicDifficultyAlgorithm<C> {
    pub(crate) fn new(genesis: H256, client: Arc<C>) -> Self {
        Self { genesis, client, work_algorithm: WorkAlgorithm::V2 }
    }

    pub(crate) fn with_work_algorithm(mut self, work_algorithm: WorkAlgorithm) -> Self {
        self.work_algorithm = work_algorithm;
        self
    }
}

impl<C: DifficultyChain> DynamicDifficultyAlgorithm<C> {
    fn difficulty_with_activation(
        &self,
        parent: H256,
        activation: Option<u32>,
    ) -> Result<u128, Error<Block>> {
        if let WorkAlgorithm::Mx10N16(context) = self.work_algorithm {
            context.ensure_genesis(self.genesis).map_err(Error::Other)?;
        }
        let parent_header = self.client.pow_header(parent)?;
        let parent_number = *parent_header.number();
        if parent == self.genesis {
            if parent_number != 0 {
                return Err(Error::Other(
                    "configured PoW genesis has a non-zero height".into(),
                ));
            }
            return Ok(EXPERIMENTAL_DIFFICULTY);
        }
        if parent_number == 0 {
            return Err(Error::Other(
                "height-zero parent is not the configured PoW genesis".into(),
            ));
        }

        let parent_aux = self
            .client
            .pow_aux(parent)?
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
        let next_number = parent_number
            .checked_add(1)
            .ok_or_else(|| Error::Other("PoW block number overflow".into()))?;
        // Fork choice also constructs this algorithm without work dispatch.
        // Difficulty is bound to genesis on every path, including that path.
        #[cfg(not(feature = "mainnet-release"))]
        if self.genesis == crate::mx10_context::PUBLICDEV_GENESIS.parse::<H256>().expect("pinned genesis") {
            if next_number >= crate::adaptive_difficulty::ACTIVATION_HEIGHT {
                return crate::adaptive_difficulty::difficulty(self.client.as_ref(), parent);
            }
            if next_number == MX10_SHARE_POOL_ACTIVATION_HEIGHT {
                return Ok(MX10_SHARE_POOL_BOOTSTRAP_DIFFICULTY);
            }
        }
        let rolling = activation.is_some_and(|height| {
            next_number >= height
                && self.genesis == MAINNET_GENESIS.parse::<H256>().expect("pinned mainnet genesis")
        });
        let window_blocks = if rolling {
            MAINNET_ROLLING_RETARGET_BLOCKS
        } else {
            SAMPLE_INTERVALS
        };
        if !rolling && parent_number % window_blocks != 0 {
            return Ok(old);
        }

        // Walk the selected number of consecutive intervals on this branch.
        let newest_timestamp = self.client.pow_timestamp(parent)?;
        let mut later_timestamp = newest_timestamp;
        let mut cursor = parent;
        let mut work = U256::zero();
        for _ in 0..window_blocks {
            let header = self.client.pow_header(cursor)?;
            let ancestor = *header.parent_hash();
            let earlier_timestamp = self.client.pow_timestamp(ancestor)?;
            if earlier_timestamp >= later_timestamp {
                return Err(Error::Other(
                    "PoW retarget window timestamps must be strictly increasing".into(),
                ));
            }
            if rolling {
                let difficulty = if cursor == parent {
                    old
                } else {
                    self.client
                        .pow_aux(cursor)?
                        .ok_or_else(|| Error::Other(format!("missing PoW aux in rolling window at {cursor:?}")))?
                        .difficulty
                };
                if difficulty == 0 || difficulty > MAX_EXPERIMENTAL_DIFFICULTY {
                    return Err(Error::Other("invalid PoW difficulty in rolling window".into()));
                }
                work += U256::from(difficulty);
            }
            later_timestamp = earlier_timestamp;
            cursor = ancestor;
        }
        let window_start = self.client.pow_header(cursor)?;
        if window_start.number().checked_add(window_blocks) != Some(parent_number) {
            return Err(Error::Other(
                "PoW retarget window does not start at the previous retarget height".into(),
            ));
        }
        if *window_start.number() == 0 && cursor != self.genesis {
            return Err(Error::Other(
                "first PoW retarget window must start at the configured genesis".into(),
            ));
        }
        // T5-02: a zero window-start timestamp means the genesis timestamp was
        // never set; ts(60k) - 0 is not a block-time measurement, so keep `old`
        // instead of clamping to the ceiling. Strict increase above guarantees
        // only a window starting at genesis can have a zero start timestamp.
        if later_timestamp == 0 {
            return Ok(old);
        }

        let elapsed = newest_timestamp
            .checked_sub(later_timestamp)
            .ok_or_else(|| Error::Other("PoW retarget elapsed time underflow".into()))?;
        if rolling {
            return Ok(rolling_difficulty(old, work, elapsed));
        }
        retarget_difficulty(old, elapsed).map_err(|error| Error::Other(error.into()))
    }
}

impl<C: DifficultyChain> PowAlgorithm<Block> for DynamicDifficultyAlgorithm<C> {
    type Difficulty = u128;

    fn difficulty(&self, parent: H256) -> Result<Self::Difficulty, Error<Block>> {
        self.difficulty_with_activation(parent, MAINNET_ROLLING_RETARGET_ACTIVATION)
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
        if let Some(height) = crate::smw_activation::scheduled(self.genesis) {
            let BlockId::Hash(parent_hash) = _parent else { return Ok(false) };
            let header = self.client.pow_header(*parent_hash)?;
            if crate::smw_activation::active(Some(height), u64::from(*header.number())) {
                return Ok(crate::smw_activation::verify(self.genesis, *pre_hash, seal, target));
            }
        }
        Ok(self.work_algorithm.verify(self.genesis, *pre_hash, seal, target))
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
        Ok(gpu_pow::verify_pre_hash(self.genesis, *pre_hash, seal, target).is_ok())
    }
    // No mining implementation is provided by this trait or this adapter.
    // GPU/CPU workers must construct and submit candidate seals separately.
}

/// Enforces the isolated PoW header shape before delegating to the SDK
/// verifier. In particular, an Aura pre-runtime digest cannot reactivate the
/// Aura slot semantics that the V2 runtime deliberately removed.
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
                return Err("gpu-pow-v2 requires full execution and an unprocessed header".into());
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
                return Err("gpu-pow-v2 verifier changed the original header".into());
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
    fn sealed_digest_shape_is_exact_and_bounded() {
        let beneficiary =
            eagle_runtime::PowBeneficiaryV1::new(eagle_runtime::AccountId::new([11; 32]))
                .encode_payload();
        let seal = DigestItem::Seal(POW_ENGINE_ID, vec![0; 105]);
        assert!(validate_sealed_pow_digest(&[
            DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary.clone()),
            seal.clone(),
        ])
        .is_ok());
        assert!(validate_sealed_pow_digest(&[
            DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary.clone()),
            DigestItem::Other(vec![0]),
            seal.clone(),
        ])
        .is_err());
        assert!(validate_sealed_pow_digest(&[
            DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary.clone()),
            DigestItem::Consensus(*b"test", vec![0]),
            seal.clone(),
        ])
        .is_err());
        assert!(validate_sealed_pow_digest(&[
            DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary),
            seal.clone(),
            seal,
        ])
        .is_err());
    }

    #[test]
    fn trait_uses_trusted_context_and_rejects_bad_work_or_difficulty() {
        assert_eq!(EXPERIMENTAL_TARGET[0], 0x3f);
        assert!(EXPERIMENTAL_TARGET[1..].iter().all(|byte| *byte == 0xff));
        assert_eq!(target_for_difficulty(4).unwrap(), EXPERIMENTAL_TARGET);
        assert_eq!(target_for_difficulty(1).unwrap(), [0xff; 32]);
        assert!(target_for_difficulty(0).is_err());
        let genesis = H256::repeat_byte(7);
        let parent = H256::repeat_byte(3);
        let beneficiary =
            eagle_runtime::PowBeneficiaryV1::new(eagle_runtime::AccountId::new([11; 32]))
                .encode_payload();
        let header = Header::new(
            42,
            H256::repeat_byte(1),
            H256::repeat_byte(2),
            parent,
            Digest {
                logs: vec![DigestItem::PreRuntime(
                    eagle_runtime::POW_ENGINE_ID,
                    beneficiary.clone(),
                )],
            },
        );
        let pre_hash = header.hash();
        let challenge = gpu_pow::challenge_for_header(genesis, &header).unwrap();
        assert_eq!(
            challenge,
            gpu_pow::challenge_from_pre_hash(genesis, pre_hash)
        );
        let cert = (0..64)
            .map(|nonce| gpu_pow::compute(challenge, nonce))
            .find(|cert| gpu_pow::ticket(cert) <= EXPERIMENTAL_TARGET)
            .expect("bounded test vector must include work below difficulty-four target");
        let seal = cert.encode_fixed();
        let exact = gpu_pow::ticket(&cert);
        let algorithm = FixedTargetAlgorithm::new(genesis);
        let id = BlockId::hash(parent);
        assert_eq!(
            algorithm.difficulty(parent).unwrap(),
            EXPERIMENTAL_DIFFICULTY
        );
        assert!(algorithm
            .verify(
                &id,
                &pre_hash,
                Some(&beneficiary),
                &seal,
                EXPERIMENTAL_DIFFICULTY,
            )
            .unwrap());
        assert!(!algorithm
            .verify(&id, &pre_hash, Some(&beneficiary), &seal, 2)
            .unwrap());
        assert!(!algorithm
            .verify(&id, &pre_hash, None, &seal, EXPERIMENTAL_DIFFICULTY)
            .unwrap());
        assert!(!algorithm
            .verify(
                &id,
                &H256::repeat_byte(9),
                Some(&beneficiary),
                &seal,
                EXPERIMENTAL_DIFFICULTY
            )
            .unwrap());
        assert!(!FixedTargetAlgorithm::new(H256::repeat_byte(8))
            .verify(
                &id,
                &pre_hash,
                Some(&beneficiary),
                &seal,
                EXPERIMENTAL_DIFFICULTY,
            )
            .unwrap());
        let mut changed = cert.clone();
        changed.nonce += 1;
        assert!(!algorithm
            .verify(
                &id,
                &pre_hash,
                Some(&beneficiary),
                &changed.encode_fixed(),
                EXPERIMENTAL_DIFFICULTY
            )
            .unwrap());
        let mut malformed = seal.clone();
        malformed.push(0);
        assert!(!algorithm
            .verify(
                &id,
                &pre_hash,
                Some(&beneficiary),
                &malformed,
                EXPERIMENTAL_DIFFICULTY,
            )
            .unwrap());
        let mut lower = exact;
        let i = lower.iter().rposition(|b| *b != 0).unwrap();
        lower[i] -= 1;
        lower[i + 1..].fill(255);
        assert!(gpu_pow::verify_pre_hash(genesis, pre_hash, &seal, lower).is_err());
    }

    #[test]
    fn retarget_math_is_bounded_and_deterministic() {
        let expected = u64::from(SAMPLE_INTERVALS) * TARGET_BLOCK_MILLIS;
        assert_eq!(SAMPLE_INTERVALS, 60);
        assert_eq!(expected, 600_000);
        assert_eq!(retarget_difficulty(100, 300_000).unwrap(), 200);
        assert_eq!(retarget_difficulty(100, 299_999).unwrap(), 200);
        assert_eq!(retarget_difficulty(100, 1_200_000).unwrap(), 50);
        assert_eq!(retarget_difficulty(100, 1_200_001).unwrap(), 50);
        assert_eq!(retarget_difficulty(100, expected).unwrap(), 100);
        assert_eq!(retarget_difficulty(100, expected / 2).unwrap(), 200);
        assert_eq!(retarget_difficulty(100, 1).unwrap(), 200);
        assert_eq!(retarget_difficulty(100, expected * 2).unwrap(), 50);
        assert_eq!(retarget_difficulty(100, u64::MAX).unwrap(), 50);
        assert!(retarget_difficulty(0, expected).is_err());
        assert_eq!(
            retarget_difficulty(MAX_EXPERIMENTAL_DIFFICULTY, expected / 2).unwrap(),
            MAX_EXPERIMENTAL_DIFFICULTY
        );
    }
    #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
    #[test]
    fn matrix_height_active_adapter_rejects_valid_hash_only_seal() {
        let genesis = H256::repeat_byte(7);
        let parent = H256::repeat_byte(3);
        let pre_hash = H256::repeat_byte(9);
        let beneficiary =
            eagle_runtime::PowBeneficiaryV1::new(eagle_runtime::AccountId::new([11; 32]))
                .encode_payload();
        let challenge = crate::gpu_pow_v3::challenge_from_pre_hash(genesis, pre_hash);
        let sha_seal = crate::gpu_pow_v3::Seal {
            challenge,
            nonce: (0..64).find(|nonce| crate::gpu_pow_v3::ticket(challenge, *nonce) <= EXPERIMENTAL_TARGET)
                .expect("bounded SHA fixture below difficulty-four target"),
        }
        .encode_fixed()
        .to_vec();
        // Both fixtures meet the fixed adapter's real difficulty-four target.
        // Rejection must be due to algorithm mismatch, not invalid difficulty.
        assert!(
            crate::gpu_pow_v3::verify_pre_hash(genesis, pre_hash, &sha_seal, EXPERIMENTAL_TARGET).is_ok()
        );
        let active = crate::active_pow_consensus::FixedTargetAlgorithm::new(genesis);
        assert!(!active
            .verify(
                &BlockId::hash(parent),
                &pre_hash,
                Some(&beneficiary),
                &sha_seal,
                EXPERIMENTAL_DIFFICULTY
            )
            .unwrap());
        let matrix_challenge = gpu_pow::challenge_from_pre_hash(genesis, pre_hash);
        let matrix_seal = (0..64).map(|nonce| gpu_pow::compute(matrix_challenge, nonce))
            .find(|cert| gpu_pow::ticket(cert) <= EXPERIMENTAL_TARGET)
            .expect("bounded matrix fixture below difficulty-four target").encode_fixed();
        assert!(active
            .verify(
                &BlockId::hash(parent),
                &pre_hash,
                Some(&beneficiary),
                &matrix_seal,
                EXPERIMENTAL_DIFFICULTY
            )
            .unwrap());
    }
}

#[cfg(all(test, feature = "gpu-pow-matrix-block-rewards-dev", not(feature = "svy-matrix-live")))]
#[path = "mx10_wire_t3_test.rs"]
mod mx10_wire_t3_test;

#[cfg(test)]
#[path = "t5_dda_window_test.rs"]
mod t5_dda_window_test;

#[cfg(all(test, feature = "gpu-pow-matrix-block-rewards-dev"))]
#[path = "t5_dda_import_test.rs"]
mod t5_dda_import_test;
