//! Experimental authoring adapters. Not wired into the node service.
//!
//! The proposer appends EAGW before Aura signs. The local import adapter then
//! moves that same EAGW item from the runtime header to post-digests without
//! altering the final `[EAGW, Aura]` header or hash. The network verifier must
//! independently perform Aura-then-EAGW verification with the same ordering.
//! This module has not been compiled or exercised against a running node.

use crate::gpu_work_binding::{candidate_subject, seal_candidate, validate_cpu_work};
use eagle_runtime::{opaque::Block, Header};
use futures::future::BoxFuture;
use sc_consensus::{BlockCheckParams, BlockImport, BlockImportParams, ImportResult};
use sp_consensus::{BlockOrigin, Environment, Error as ConsensusError, Proposal, Proposer};
use sp_runtime::{
    generic::DigestItem,
    traits::{Block as BlockT, Header as HeaderT},
    Digest,
};
use std::{
    future::Future,
    pin::Pin,
    time::{Duration, Instant},
};

/// Asynchronous, cancellation-aware work source; implementations must respect
/// `deadline` and cancellation of the returned future, and must not block an
/// async executor. Its result is independently checked by the CPU reference.
pub(crate) trait WorkProvider: Clone + Send + Sync + 'static {
    fn produce(
        &self,
        subject: [u8; 32],
        deadline: Instant,
    ) -> BoxFuture<'static, Result<[u8; 32], String>>;
}

fn consensus_failure(message: impl Into<String>) -> ConsensusError {
    ConsensusError::Other(Box::new(std::io::Error::other(message.into())))
}

/// Wrap the existing proposer factory; no service integration is performed here.
pub(crate) struct WorkEnvironment<E, W> {
    pub(crate) inner: E,
    pub(crate) work: W,
}

/// Proposer that computes a sealed candidate before Aura signs it.
pub(crate) struct WorkProposer<P, W> {
    inner: P,
    work: W,
}

impl<E, W> Environment<Block> for WorkEnvironment<E, W>
where
    E: Environment<Block>,
    E::Error: Send + 'static,
    E::Proposer: Proposer<Block, Error = E::Error>,
    W: WorkProvider,
{
    type Proposer = WorkProposer<E::Proposer, W>;
    type Error = E::Error;
    type CreateProposer = BoxFuture<'static, Result<Self::Proposer, Self::Error>>;

    fn init(&mut self, parent_header: &Header) -> Self::CreateProposer {
        let future = self.inner.init(parent_header);
        let work = self.work.clone();
        Box::pin(async move {
            Ok(WorkProposer {
                inner: future.await?,
                work,
            })
        })
    }
}

impl<P, W> Proposer<Block> for WorkProposer<P, W>
where
    P: Proposer<Block> + Send + 'static,
    P::Error: Send + 'static,
    W: WorkProvider,
{
    type Error = P::Error;
    type Proof = P::Proof;
    type ProofRecording = P::ProofRecording;
    type Proposal = BoxFuture<'static, Result<Proposal<Block, Self::Proof>, Self::Error>>;

    fn propose(
        self,
        inherent_data: sp_inherents::InherentData,
        inherent_digests: Digest,
        max_duration: Duration,
        block_size_limit: Option<usize>,
    ) -> Self::Proposal {
        let deadline = Instant::now() + max_duration;
        let proposal = self.inner.propose(
            inherent_data,
            inherent_digests,
            max_duration,
            block_size_limit,
        );
        Box::pin(async move {
            let mut proposal = proposal.await?;
            if Instant::now() >= deadline {
                return Err(consensus_failure("EAGW proposal deadline elapsed").into());
            }
            let (header, body) = proposal.block.deconstruct();
            let subject = candidate_subject(&header)
                .map_err(|e| consensus_failure(format!("invalid EAGW candidate: {e:?}")))?;
            let commitment = self
                .work
                .produce(subject, deadline)
                .await
                .map_err(consensus_failure)?;
            if Instant::now() >= deadline {
                return Err(consensus_failure("EAGW work deadline elapsed").into());
            }
            let sealed = seal_candidate(&header, commitment)
                .map_err(|e| consensus_failure(format!("EAGW seal failed: {e:?}")))?;
            validate_cpu_work(&sealed)
                .map_err(|e| consensus_failure(format!("invalid EAGW work: {e:?}")))?;
            proposal.block = Block::new(sealed, body);
            Ok(proposal)
        })
    }
}

/// Local-only import adapter: preserve the full hash while separating the EAGW
/// post-runtime item from the runtime header. Never use for network import.
pub(crate) struct LocalWorkDigestSplit<I> {
    pub(crate) inner: I,
}

impl<I> BlockImport<Block> for LocalWorkDigestSplit<I>
where
    I: BlockImport<Block> + Send + Sync,
    I::Error: Send + 'static,
{
    type Error = ConsensusError;

    fn check_block<'life0, 'async_trait>(
        &'life0 self,
        block: BlockCheckParams<Block>,
    ) -> Pin<Box<dyn Future<Output = Result<ImportResult, Self::Error>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            self.inner
                .check_block(block)
                .await
                .map_err(|e| ConsensusError::ClientImport(e.to_string()))
        })
    }

    fn import_block<'life0, 'async_trait>(
        &'life0 self,
        mut block: BlockImportParams<Block>,
    ) -> Pin<Box<dyn Future<Output = Result<ImportResult, Self::Error>> + Send + 'async_trait>>
    where
        'life0: 'async_trait,
        Self: 'async_trait,
    {
        Box::pin(async move {
            if block.origin != BlockOrigin::Own {
                return Err(consensus_failure(
                    "EAGW local splitter rejects non-local blocks",
                ));
            }
            if !matches!(block.post_digests.as_slice(), [DigestItem::Seal(id, _)] if *id == sp_consensus_aura::AURA_ENGINE_ID)
            {
                return Err(consensus_failure("expected one final Aura seal"));
            }
            validate_cpu_work(&block.header)
                .map_err(|e| consensus_failure(format!("invalid EAGW local block: {e:?}")))?;
            let full_header = block.post_header();
            let full_hash = full_header.hash();
            if block.post_hash.is_some_and(|cached| cached != full_hash) {
                return Err(consensus_failure("inconsistent cached block hash"));
            }
            let seal = block
                .header
                .digest_mut()
                .logs
                .pop()
                .ok_or_else(|| consensus_failure("missing EAGW seal"))?;
            block.post_digests.insert(0, seal);
            if block.post_header() != full_header {
                return Err(consensus_failure("EAGW split changed full header"));
            }
            block.post_hash = Some(full_hash);
            self.inner
                .import_block(block)
                .await
                .map_err(|e| ConsensusError::ClientImport(e.to_string()))
        })
    }
}
