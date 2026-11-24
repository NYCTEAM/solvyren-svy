//! Cumulative-work fork choice for the isolated GPU PoW chain.

use crate::service::FullBackend;
use crate::{active_pow_consensus::DynamicDifficultyAlgorithm, service::FullClient};
use eagle_runtime::opaque::Block;
use sc_client_api::backend::Backend as ClientBackend;
use sc_consensus_pow::{PowAlgorithm, PowAux};
use sp_blockchain::{Backend as BlockchainBackend, HeaderBackend};
use sp_consensus::{Error as ConsensusError, SelectChain};
use sp_runtime::traits::{Block as BlockT, Header as HeaderT, NumberFor};
use std::sync::Arc;

type Hash = <Block as BlockT>::Hash;
type Header = <Block as BlockT>::Header;

/// Selects the branch with the greatest validated cumulative PoW.
#[derive(Clone)]
pub(crate) struct PowWorkSelectChain {
    backend: Arc<FullBackend>,
    algorithm: DynamicDifficultyAlgorithm,
}

impl PowWorkSelectChain {
    pub(crate) fn new(backend: Arc<FullBackend>, client: Arc<FullClient>) -> Self {
        let genesis = backend.blockchain().info().genesis_hash;
        Self {
            backend,
            algorithm: DynamicDifficultyAlgorithm::new(genesis, client),
        }
    }

    fn lookup_error(message: impl Into<String>) -> ConsensusError {
        ConsensusError::ChainLookup(message.into())
    }

    fn header(&self, hash: Hash) -> Result<Header, ConsensusError> {
        self.backend
            .blockchain()
            .header(hash)
            .map_err(|e| Self::lookup_error(e.to_string()))?
            .ok_or_else(|| Self::lookup_error(format!("missing header {hash:?}")))
            .and_then(|header| {
                if header.hash() == hash {
                    Ok(header)
                } else {
                    Err(Self::lookup_error(format!(
                        "header hash mismatch for {hash:?}"
                    )))
                }
            })
    }

    fn aux(&self, hash: Hash, is_genesis: bool) -> Result<PowAux<u128>, ConsensusError> {
        let value = PowAux::<u128>::read_optional::<_, Block>(self.backend.as_ref(), &hash)
            .map_err(|e| Self::lookup_error(e.to_string()))?;
        match (value, is_genesis) {
            (Some(aux), _) => Ok(aux),
            (None, true) => Ok(PowAux::default()),
            (None, false) => Err(Self::lookup_error(format!(
                "missing cumulative-work record for {hash:?}"
            ))),
        }
    }

    /// Validate the complete unfinalized path and return its leaf work.
    /// Dead forks are excluded; corrupt metadata on an eligible fork fails closed.
    fn validate_leaf(
        &self,
        leaf: Hash,
        finalized_hash: Hash,
        finalized_number: NumberFor<Block>,
    ) -> Result<Option<(Hash, Header, u128)>, ConsensusError> {
        let leaf_header = self.header(leaf)?;
        if *leaf_header.number() < finalized_number {
            return Ok(None);
        }

        let mut path = Vec::new();
        let mut hash = leaf;
        let mut header = leaf_header.clone();
        while hash != finalized_hash {
            if *header.number() <= finalized_number {
                return Ok(None);
            }
            path.push((hash, header.clone()));
            hash = *header.parent_hash();
            let parent = self.header(hash)?;
            let expected_child_number = parent
                .number()
                .checked_add(1)
                .ok_or_else(|| Self::lookup_error("block number overflow"))?;
            if *header.number() != expected_child_number {
                return Err(Self::lookup_error(format!(
                    "non-decreasing or discontinuous header ancestry at {hash:?}"
                )));
            }
            header = parent;
        }

        let finalized_is_genesis = *header.number() == 0;
        let mut parent_aux = self.aux(finalized_hash, finalized_is_genesis)?;
        if finalized_is_genesis && (parent_aux.difficulty != 0 || parent_aux.total_difficulty != 0)
        {
            return Err(Self::lookup_error("genesis PoW work must be exactly zero"));
        }
        for (child_hash, child_header) in path.iter().rev() {
            let expected_number = header
                .number()
                .checked_add(1)
                .ok_or_else(|| Self::lookup_error("block number overflow"))?;
            if *child_header.number() != expected_number || *child_header.parent_hash() != hash {
                return Err(Self::lookup_error(format!(
                    "non-contiguous PoW branch at {child_hash:?}"
                )));
            }
            let child_aux = self.aux(*child_hash, false)?;
            let expected_difficulty = self
                .algorithm
                .difficulty(hash)
                .map_err(|e| Self::lookup_error(e.to_string()))?;
            if child_aux.difficulty != expected_difficulty || child_aux.difficulty == 0 {
                return Err(Self::lookup_error(format!(
                    "parent-derived PoW difficulty mismatch at {child_hash:?}"
                )));
            }
            let expected_total = parent_aux
                .total_difficulty
                .checked_add(child_aux.difficulty)
                .ok_or_else(|| Self::lookup_error("cumulative PoW overflow"))?;
            if child_aux.total_difficulty != expected_total {
                return Err(Self::lookup_error(format!(
                    "invalid cumulative PoW at {child_hash:?}"
                )));
            }
            hash = *child_hash;
            header = child_header.clone();
            parent_aux = child_aux;
        }
        Ok(Some((leaf, leaf_header, parent_aux.total_difficulty)))
    }

    fn ranked_leaves_locked(&self) -> Result<Vec<(Hash, Header, u128)>, ConsensusError> {
        let blockchain = self.backend.blockchain();
        let info = blockchain.info();
        let finalized_header = self.header(info.finalized_hash)?;
        if *finalized_header.number() != info.finalized_number {
            return Err(Self::lookup_error("finalized hash/number mismatch"));
        }
        let mut ranked = Vec::new();
        for leaf in blockchain
            .leaves()
            .map_err(|e| Self::lookup_error(e.to_string()))?
        {
            if let Some(candidate) =
                self.validate_leaf(leaf, info.finalized_hash, info.finalized_number)?
            {
                ranked.push(candidate);
            }
        }
        if ranked.is_empty() {
            return Err(Self::lookup_error(
                "no valid cumulative-work leaf descends from finalized head",
            ));
        }
        ranked.sort_by(|a, b| b.2.cmp(&a.2).then_with(|| a.0.cmp(&b.0)));
        Ok(ranked)
    }

    fn ranked_leaves(&self) -> Result<Vec<(Hash, Header, u128)>, ConsensusError> {
        let _import_guard = self.backend.get_import_lock().read();
        self.ranked_leaves_locked()
    }
}

#[async_trait::async_trait]
impl SelectChain<Block> for PowWorkSelectChain {
    async fn leaves(&self) -> Result<Vec<Hash>, ConsensusError> {
        Ok(self
            .ranked_leaves()?
            .into_iter()
            .map(|entry| entry.0)
            .collect())
    }

    async fn best_chain(&self) -> Result<Header, ConsensusError> {
        Ok(self.ranked_leaves()?.remove(0).1)
    }

    async fn finality_target(
        &self,
        base_hash: Hash,
        maybe_max_number: Option<NumberFor<Block>>,
    ) -> Result<Hash, ConsensusError> {
        let _import_guard = self.backend.get_import_lock().read();
        let base = self.header(base_hash)?;
        let base_number = *base.number();
        let mut head = self.ranked_leaves_locked()?.remove(0).1;
        let mut target = head.hash();

        if let Some(max_number) = maybe_max_number {
            if max_number < base_number {
                return Err(Self::lookup_error("finality maximum is below its base"));
            }
            while *head.number() > max_number {
                target = *head.parent_hash();
                head = self.header(target)?;
            }
        }
        while head.hash() != base_hash {
            if *head.number() <= base_number {
                return Err(Self::lookup_error(format!(
                    "finality base {base_hash:?} is not on the selected PoW branch"
                )));
            }
            head = self.header(*head.parent_hash())?;
        }
        Ok(target)
    }
}
