//! Network import wrapper for the EAGW-before-Aura header ordering.
//! A certificate is only a header binding until an independent verifier accepts it.

use crate::gpu_work_binding::{validate_cpu_work, validate_sealed_header, WorkCertificate};
use eagle_runtime::opaque::Block;
use sc_consensus::{block_import::BlockImportParams, import_queue::Verifier};
use sp_runtime::traits::Header as HeaderT;
use std::{future::Future, pin::Pin};

/// An implementation must independently validate the committed work and its context.
pub(crate) trait WorkVerifier: Send + Sync {
    fn verify(
        &self,
        header: &eagle_runtime::Header,
        certificate: &WorkCertificate,
    ) -> Result<(), String>;
}

/// Recomputes the fixed V1 workload on CPU and accepts only the exact commitment.
/// This proves deterministic result equality, not that a GPU performed the work.
pub(crate) struct DeterministicCpuWorkVerifier;
impl WorkVerifier for DeterministicCpuWorkVerifier {
    fn verify(&self, header: &eagle_runtime::Header, _: &WorkCertificate) -> Result<(), String> {
        validate_cpu_work(header)
            .map(|_| ())
            .map_err(|error| format!("invalid EAGW work: {error:?}"))
    }
}

/// Verifies Aura first, then checks the EAGW seal immediately before it.
pub(crate) struct AuraThenWork<V, W> {
    pub(crate) aura: V,
    pub(crate) work: W,
}

impl<V: Verifier<Block>, W: WorkVerifier> Verifier<Block> for AuraThenWork<V, W> {
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
                return Err("EAGW requires full execution and an unprocessed header".into());
            }
            let original = block.header.clone();
            let full_hash = original.hash();
            let mut checked = self.aura.verify(block).await?;
            let certificate = validate_sealed_header(&checked.header)
                .map_err(|e| format!("invalid EAGW header: {e:?}"))?;
            self.work.verify(&checked.header, &certificate)?;
            let seal = checked
                .header
                .digest_mut()
                .logs
                .pop()
                .ok_or("missing EAGW seal")?;
            checked.post_digests.insert(0, seal);
            let mut reconstructed = checked.header.clone();
            for item in &checked.post_digests {
                reconstructed.digest_mut().push(item.clone());
            }
            if reconstructed != original || reconstructed.hash() != full_hash {
                return Err("EAGW/Aura reconstructed header mismatch".into());
            }
            checked.post_hash = Some(full_hash);
            Ok(checked)
        })
    }
}
