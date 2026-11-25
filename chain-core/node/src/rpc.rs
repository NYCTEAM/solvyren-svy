//! A collection of node-specific RPC methods.
//! Substrate provides the `sc-rpc` crate, which defines the core RPC layer
//! used by Substrate nodes. This file extends those RPC definitions with
//! capabilities that are specific to this project's runtime configuration.

#![warn(missing_docs)]

use std::sync::Arc;

use eagle_runtime::{opaque::Block, AccountId, Balance, Nonce};
use jsonrpsee::RpcModule;
use sc_transaction_pool_api::TransactionPool;
use sp_api::ProvideRuntimeApi;
use sp_block_builder::BlockBuilder;
use sp_blockchain::{Error as BlockChainError, HeaderBackend, HeaderMetadata};

/// Full client dependencies.
pub struct FullDeps<C, P> {
    /// The client instance to use.
    pub client: Arc<C>,
    /// Transaction pool instance.
    pub pool: Arc<P>,
}

/// Instantiate all full RPC extensions.
pub fn create_full<C, P>(
    deps: FullDeps<C, P>,
) -> Result<RpcModule<()>, Box<dyn std::error::Error + Send + Sync>>
where
    C: ProvideRuntimeApi<Block>,
    C: HeaderBackend<Block> + HeaderMetadata<Block, Error = BlockChainError> + 'static,
    C: Send + Sync + 'static,
    C::Api: substrate_frame_rpc_system::AccountNonceApi<Block, AccountId, Nonce>,
    C::Api: pallet_transaction_payment_rpc::TransactionPaymentRuntimeApi<Block, Balance>,
    C::Api: BlockBuilder<Block>,
    P: TransactionPool + 'static,
{
    use pallet_transaction_payment_rpc::{TransactionPayment, TransactionPaymentApiServer};
    use substrate_frame_rpc_system::{System, SystemApiServer};

    let mut module = RpcModule::new(());
    let FullDeps { client, pool } = deps;

    module.merge(System::new(client.clone(), pool).into_rpc())?;
    module.merge(TransactionPayment::new(client).into_rpc())?;

    // Extend this RPC with a custom API by using the following syntax.
    // `YourRpcStruct` should have a reference to a client, which is needed
    // to call into the runtime.
    // `module.merge(YourRpcTrait::into_rpc(YourRpcStruct::new(ReferenceToClient, ...)))?;`

    // You probably want to enable the `rpc v2 chainSpec` API as well
    //
    // let chain_name = chain_spec.name().to_string();
    // let genesis_hash = client.block_hash(0).ok().flatten().expect("Genesis block exists; qed");
    // let properties = chain_spec.properties();
    // module.merge(ChainSpec::new(chain_name, genesis_hash, properties).into_rpc())?;

    Ok(module)
}

// Only new_full registers this method with its actual live client.
#[cfg(all(feature = "gpu-pow-matrix-block-rewards-dev", not(feature = "mainnet-release")))]
#[path = "mx10_template.rs"]
mod mx10_template;

#[cfg(all(feature = "gpu-pow-matrix-block-rewards-dev", not(feature = "mainnet-release")))]
pub(crate) fn register_mx10_template(
    module: &mut RpcModule<()>, client: Arc<crate::service::FullClient>,
    pool: Arc<sc_transaction_pool::TransactionPoolHandle<Block, crate::service::FullClient>>,
    context: crate::mx10_context::Mx10ChainContext,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    mx10_template::register(module, client, pool, context)
}

#[cfg(all(feature = "gpu-pow-matrix-block-rewards-dev", not(feature = "mainnet-release")))]
#[path = "mx10_import.rs"]
mod mx10_import;

#[cfg(all(feature = "gpu-pow-matrix-block-rewards-dev", not(feature = "mainnet-release")))]
pub(crate) fn register_mx10_import(
 module: &mut RpcModule<()>, client: Arc<crate::service::FullClient>,
 queue: Arc<std::sync::Mutex<Box<dyn sc_consensus::import_queue::ImportQueueService<Block>>>>,
 context: crate::mx10_context::Mx10ChainContext,
) -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
 mx10_import::register(module, client, queue, context)
}
