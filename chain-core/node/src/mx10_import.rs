//! Bounded CPU sealing and actual BasicQueue ingress, pinned isolated localhost only.
//! No GPU, proposer, V2 miner, direct database write or background mining loop.
use crate::gpu_pow_mx10 as mx;
use crate::mx10_context::Mx10ChainContext;
use eagle_runtime::{opaque::Block, POW_ENGINE_ID};
use jsonrpsee::{RpcModule, types::ErrorObjectOwned};
use sc_consensus::import_queue::{ImportQueueService, IncomingBlock};
use sc_consensus_pow::PowAlgorithm;
use sp_blockchain::HeaderBackend;
use sp_runtime::{generic::{SignedBlock, DigestItem}, traits::Header as _};
use sp_consensus::BlockOrigin;
use std::{sync::{Arc, Mutex}, time::{Duration, Instant}};

pub(crate) type Queue = Arc<Mutex<Box<dyn ImportQueueService<Block>>>>;
fn error(e: impl std::fmt::Display) -> ErrorObjectOwned { ErrorObjectOwned::owned(-32090, e.to_string(), None::<()>) }
fn hex(bytes:&[u8])->String {bytes.iter().map(|b|format!("{b:02x}")).collect()}
fn context(client:&crate::service::FullClient, supplied:&str, trusted:Mx10ChainContext) -> Result<sp_core::H256,ErrorObjectOwned> {
 let genesis=client.hash(0).map_err(error)?.ok_or_else(||error("missing genesis"))?;
 trusted.ensure_genesis(genesis).map_err(error)?;
 trusted.ensure_request(supplied).map_err(error)?;
 Ok(genesis)
}
fn parent(client:&crate::service::FullClient, header:&eagle_runtime::Header)->Result<(),ErrorObjectOwned>{
 let best=client.info();
 if header.parent_hash()!=&best.best_hash || *header.number()!=best.best_number.checked_add(1).ok_or_else(||error("height overflow"))? {return Err(error("wrong parent/height: must extend current best"));}
 Ok(())
}
fn target(client:Arc<crate::service::FullClient>,trusted:Mx10ChainContext,parent:sp_core::H256)->Result<[u8;32],ErrorObjectOwned>{
 trusted.target(client,parent).map_err(error)
}

pub(super) fn register(module:&mut RpcModule<()>,client:Arc<crate::service::FullClient>,queue:Queue,trusted:Mx10ChainContext)
 ->Result<(),Box<dyn std::error::Error+Send+Sync>> {
 trusted.ensure_genesis(client.hash(0)?.ok_or("missing MX10 genesis")?)?;
 let cpu_client=client.clone();
 // Explicit operator-triggered, bounded CPU work; never builds another proposal.
 module.register_blocking_method("mx10_sealTemplateCpu",move |params,_,_|->Result<serde_json::Value,ErrorObjectOwned>{
  let (supplied,value):(String,serde_json::Value)=params.parse()?;
  let genesis=context(cpu_client.as_ref(),&supplied,trusted)?;
  let mut block:SignedBlock<Block>=serde_json::from_value(value).map_err(error)?;
  let header=block.block.header.clone();parent(cpu_client.as_ref(),&header)?;
  let logs=&header.digest().logs;
  if logs.len()!=1 || !matches!(&logs[0],DigestItem::PreRuntime(id,_) if *id==POW_ENGINE_ID) || block.justifications.is_some() {return Err(error("requires unsealed beneficiary-only proposal"));}
  let pre_hash=header.hash();let challenge=mx::challenge_from_pre_hash(genesis,pre_hash);
  let target=target(cpu_client.clone(),trusted,*header.parent_hash())?;
  let shape=trusted.shape();
  let deadline=Instant::now()+Duration::from_secs(5);
  for nonce in 0..4096u64 {
   if Instant::now()>=deadline {return Err(error("bounded CPU seal deadline"));}
   let cert=mx::compute(shape,challenge,nonce);
   if mx::ticket(&cert)>target {continue;}
   let seal=cert.encode_fixed();
   trusted.verify(pre_hash,&seal,target).map_err(|e|error(format!("{e:?}")))?;
   parent(cpu_client.as_ref(),&header)?;
   block.block.header=crate::service::mx10_author::seal_header(header,&seal);
   return Ok(serde_json::json!({"block":block,"pre_hash":hex(pre_hash.as_bytes()),"challenge":hex(&challenge),"nonce":nonce,"ticket":hex(&mx::ticket(&cert)),"certificate_hex":hex(&seal),"sealed_hash":format!("{:?}",block.block.header.hash()),"backend":"cpu","nonce_search_limit":4096,"imported":false}));
  }
  Err(error("bounded CPU nonce range had no winner"))
 })?;
 module.register_blocking_method("mx10_importSealedBlock",move |params,_,_|->Result<serde_json::Value,ErrorObjectOwned>{
  let (supplied,value):(String,serde_json::Value)=params.parse()?;
  context(client.as_ref(),&supplied,trusted)?;
  let block:SignedBlock<Block>=serde_json::from_value(value).map_err(error)?;
  let header=block.block.header.clone();parent(client.as_ref(),&header)?;
  if block.justifications.is_some() || header.digest().logs.len()!=2 {return Err(error("requires beneficiary and final seal only"));}
  let mut unsealed=header.clone();
  let seal=match unsealed.digest_mut().logs.pop(){Some(DigestItem::Seal(id,seal)) if id==POW_ENGINE_ID=>seal,_=>return Err(error("missing final pow_ seal"))};
  let pre_hash=unsealed.hash();let target=target(client.clone(),trusted,*header.parent_hash())?;
  let checked=trusted.verify(pre_hash,&seal,target)
   .map_err(|e|error(format!("MX10 verification rejected: {e:?}")))?;
  let hash=header.hash();
  queue.lock().map_err(|_|error("import queue mutex poisoned"))?.import_blocks(BlockOrigin::Own,vec![IncomingBlock::<Block>{hash,header:Some(header),body:Some(block.block.extrinsics),indexed_body:None,justifications:None,origin:None,allow_missing_state:false,skip_execution:false,import_existing:false,state:None}]);
  eprintln!("MX10_LIVE_IMPORT hash={hash:?} pre_hash={pre_hash:?} admission_verify_pre_hash=Ok({checked:?}) queue=BasicQueue skip_execution=false");
  // Queue acceptance is NOT import success; caller must observe chain_getBlockHash/Best.
  Ok(serde_json::json!({"status":"queued","hash":format!("{hash:?}"),"pre_hash":hex(pre_hash.as_bytes()),"admission_verify_pre_hash":"Ok","import_success_requires_rpc_observation":true}))
 })?;
 Ok(())
}
