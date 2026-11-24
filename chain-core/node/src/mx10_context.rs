//! Node-owned MX10 identity/capability. No production identity is approved.
//! Never deserialize this type from a spec property, certificate or RPC request.
use crate::{chain_spec, gpu_pow_mx10 as mx};
use sp_core::H256;

pub(crate) const ISOLATED_GENESIS: &str = "0x6172a0f0094cba19bfafe0b389dac397189903a6f816deb21378c3dba5200100";
pub(crate) const PUBLICDEV_GENESIS: &str = "0x2a1d109015577c2e617a15768e9623ca0f63d3319520ce4fc485a347a7faf44e";
pub(crate) const PROFILE: &str = "mx10-n16-inner2048-v10";

/// Private fields and the sole isolated factory prevent caller-selected shapes,
/// genesis identities, production activation or an RPC-selected verifier.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Mx10ChainContext {
    genesis: H256,
}

impl Mx10ChainContext {
    pub(crate) fn isolated(spec: &dyn sc_service::ChainSpec) -> Result<Self, String> {
        if !cfg!(feature = "gpu-pow-matrix-block-rewards-dev") || cfg!(feature = "svy-matrix-live") {
            return Err("MX10 isolated context unavailable in this build".into());
        }
        let properties = spec.properties();
        if !chain_spec::is_mx10_isolated_id(spec.id()) || spec.chain_type() != sc_service::ChainType::Local ||
            properties.get("workAlgorithm").and_then(|v| v.as_str()) != Some(chain_spec::MX10_WORK_ALGORITHM) {
            return Err("MX10 requires the pinned n=16 workAlgorithm and fixed isolated spec identity".into());
        }
        // Existing isolated specs need no new fields; contradictory declarations
        // cannot override the trusted profile/version constants.
        if properties.get("profile").is_some_and(|v| v.as_str() != Some(PROFILE)) ||
            properties.get("version").is_some_and(|v| v.as_u64() != Some(u64::from(mx::VERSION))) {
            return Err("MX10 profile/version declaration conflicts with fixed context".into());
        }
        let shape = mx::EvalShape::new(16, 2048).map_err(|e| format!("{e:?}"))?;
        let digest = mx::work_algorithm_digest(shape).iter().map(|b| format!("{b:02x}")).collect::<String>();
        if digest != chain_spec::MX10_WORK_ALGORITHM { return Err("MX10 work digest mismatch".into()); }
        let genesis = match spec.id() {
            chain_spec::POW_MX10_LOCAL_ID => ISOLATED_GENESIS,
            chain_spec::POW_MX10_PUBLICDEV_ID => PUBLICDEV_GENESIS,
            _ => return Err("MX10 spec identity has no pinned genesis".into()),
        };
        Ok(Self { genesis: genesis.parse().map_err(|_| "invalid isolated genesis constant")? })
    }

    /// Deliberately no successful production constructor or activation default.
    pub(crate) fn production() -> Result<Self, String> {
        Err("MX10 production mode/identity/activation have not been approved".into())
    }
    pub(crate) fn genesis(self) -> H256 { self.genesis }
    pub(crate) fn shape(self) -> mx::EvalShape { mx::EvalShape::new(16, 2048).expect("fixed MX10 context shape") }
    pub(crate) fn version(self) -> u8 { mx::VERSION }
    pub(crate) fn certificate_bytes(self) -> usize { mx::CERTIFICATE_BYTES }
    pub(crate) fn work_algorithm(self) -> &'static str { chain_spec::MX10_WORK_ALGORITHM }
    pub(crate) fn ensure_genesis(self, actual: H256) -> Result<(), String> {
        if actual != self.genesis { return Err("wrong pinned MX10 genesis/context".into()); }
        Ok(())
    }
    pub(crate) fn ensure_request(self, supplied: &str) -> Result<(), String> {
        let supplied = supplied.parse::<H256>().map_err(|_| "request genesis is invalid")?;
        if supplied != self.genesis { return Err("request genesis differs from node MX10 context".into()); }
        Ok(())
    }
    /// Original algorithm and original error order, with node-owned profile.
    pub(crate) fn verify(self, pre_hash: H256, seal: &[u8], target: [u8; 32]) -> Result<[u8; 32], mx::Error> {
        mx::verify_pre_hash(self.shape(), self.genesis, pre_hash, seal, target)
    }
    pub(crate) fn difficulty<C: crate::gpu_pow_consensus::DifficultyChain>(self, client: std::sync::Arc<C>, parent: H256) -> Result<u128, String> {
        use sc_consensus_pow::PowAlgorithm;
        let algorithm = crate::gpu_pow_consensus::DynamicDifficultyAlgorithm::new(self.genesis, client)
            .with_work_algorithm(crate::gpu_pow_consensus::WorkAlgorithm::Mx10N16(self));
        algorithm.difficulty(parent).map_err(|e| format!("{e:?}"))
    }
    pub(crate) fn target<C: crate::gpu_pow_consensus::DifficultyChain>(self, client: std::sync::Arc<C>, parent: H256) -> Result<[u8; 32], String> {
        crate::gpu_pow_consensus::target_for_difficulty(self.difficulty(client, parent)?).map_err(str::to_owned)
    }
}

#[cfg(test)]
#[path = "mx10_context_test.rs"]
pub(crate) mod tests;
