//! Bounded, offline MX10 block author: real runtime API building and immediate
//! PowBlockImport submission, one parent at a time. No prebuilt block input,
//! network listener, V2 miner, T5, or production-chain support.
use crate::{gpu_pow_mx10 as mx, gpu_pow_consensus::{DynamicDifficultyAlgorithm, WorkAlgorithm}};
use eagle_runtime::{opaque::Block, AccountId, Header, PowBeneficiaryV1, POW_ENGINE_ID};
use sc_consensus::{BlockImport, BlockImportParams, ImportResult};
use sc_consensus_pow::{PowAlgorithm, PowBlockImport, PowIntermediate, INTERMEDIATE_KEY};
use sc_service::{Configuration, TaskManager};
use sp_api::{ProvideRuntimeApi, Core, ApiExt};
use sp_consensus_pow::TimestampApi;
use sp_block_builder::BlockBuilder;
use sp_blockchain::HeaderBackend;
use sp_inherents::InherentDataProvider;
use sp_runtime::{generic::{Digest, DigestItem, SignedBlock}, traits::{Block as _, Header as _}};
use sp_consensus::BlockOrigin;
use std::{fs::{self, OpenOptions}, io::Write, path::PathBuf, time::{Duration,Instant,SystemTime,UNIX_EPOCH}};


type Error = sc_cli::Error;
fn error(e:impl std::fmt::Display)->Error { Error::Input(e.to_string()) }
fn now_ms()->u64 {SystemTime::now().duration_since(UNIX_EPOCH).unwrap_or_default().as_millis() as u64}
fn shape()->mx::EvalShape {mx::EvalShape::new(16,2048).expect("trusted isolated constants")}

#[derive(Clone)]
pub(crate) struct Options { pub blocks:u32, pub backend:String, pub output:PathBuf }

pub(crate) fn run(config:Configuration, options:Options)
 -> Result<(impl std::future::Future<Output=Result<(),Error>>,TaskManager),Error> {
 if !(1..=8).contains(&options.blocks) {return Err(error("--mx10-author-blocks must be 1..=8"));}
 let context=crate::mx10_context::Mx10ChainContext::isolated(config.chain_spec.as_ref()).map_err(error)?;
 if !["auto","gpu","cpu"].contains(&options.backend.as_str()) {return Err(error("MX10 backend must be auto, gpu or cpu"));}
 // Never overwrite a previous session or append to its output.
 fs::create_dir(&options.output).map_err(error)?;
 let sc_service::PartialComponents {client,task_manager,select_chain,other:(inner_import,_,_),..}=super::new_partial(&config)?;
 let genesis=client.hash(0).map_err(error)?.ok_or_else(||error("missing MX10 genesis"))?;
 context.ensure_genesis(genesis).map_err(error)?;
 let algorithm=DynamicDifficultyAlgorithm::new(genesis,client.clone()).with_work_algorithm(WorkAlgorithm::Mx10N16(context));
 let import_algorithm=algorithm.clone();
 let importer=PowBlockImport::new(inner_import,client.clone(),import_algorithm,0,select_chain,move |_,()|async move{
  Ok::<_,Box<dyn std::error::Error+Send+Sync>>(sp_timestamp::InherentDataProvider::from_system_time())
 });
 let future=async move {
  let started=Instant::now();let deadline=started+Duration::from_secs(45);
  let mut fallback=None;let mut gpu=None;
  if options.backend!="cpu" {
   match crate::gpu_pow_mx10_gpu::GpuSession::open(&options.output) {
    Ok(mut session)=>{
     // One small, pinned fixed-vector check; not a C/s measurement.
     let fixed=session.batch([0;32],7,1).map_err(error)?;
     if fixed!=vec![mx::compute(shape(),[0;32],7)] {return Err(error("MX10 GPU fixed vector mismatch"));}
     gpu=Some(session);
    },
    Err(reason)=>{if options.backend=="gpu" {return Err(error(reason));} fallback=Some(reason);}
   }
  }
  let backend=if gpu.is_some(){"gpu"}else{"cpu"};
  let out=options.output.clone();let mut records=Vec::new();let mut block_stream=OpenOptions::new().write(true).create_new(true).open(out.join("authored-blocks.jsonl")).map_err(error)?;
  let mut receipts=OpenOptions::new().write(true).create_new(true).open(out.join("produced.jsonl")).map_err(error)?;
  for _ in 0..options.blocks {
   if Instant::now()>=deadline {return Err(error("bounded MX10 author session deadline"));}
   let parent=client.info().best_hash;let parent_header=client.header(parent).map_err(error)?.ok_or_else(||error("missing parent"))?;
   let parent_time=client.runtime_api().timestamp(parent).map_err(error)?;
   // Respect the existing runtime's SLOT_DURATION/2 minimum period. Wait in
   // real time rather than invent future timestamps or change the difficulty.
   let earliest=parent_time.saturating_add(eagle_runtime::SLOT_DURATION/2);
   let now=now_ms();if earliest>now {
    let wait=Duration::from_millis(earliest-now);
    if Instant::now()+wait>=deadline {return Err(error("timestamp wait exceeds MX10 session budget"));}
    futures_timer::Delay::new(wait).await;
   }
   let timestamp=now_ms();let number=parent_header.number().checked_add(1).ok_or_else(||error("height overflow"))?;
   let beneficiary=PowBeneficiaryV1::new(AccountId::new([11;32])).encode_payload();
   let initial=Header::new(number,Default::default(),Default::default(),parent,Digest{logs:vec![DigestItem::PreRuntime(POW_ENGINE_ID,beneficiary)]});
   let mut inherent=sp_inherents::InherentData::new();
   sp_timestamp::InherentDataProvider::new(timestamp.into()).provide_inherent_data(&mut inherent).await.map_err(error)?;
   // Same runtime API overlay for initialize/apply/finalize, then discard it:
   // the importer independently executes and commits the authored block.
   let mut api=client.runtime_api();api.set_call_context(sp_core::traits::CallContext::Onchain);
   api.initialize_block(parent,&initial).map_err(error)?;
   let body=api.inherent_extrinsics(parent,inherent).map_err(error)?;
   for xt in &body {let applied=api.apply_extrinsic(parent,xt.clone()).map_err(error)?;if !matches!(applied,Ok(Ok(()))) {return Err(error(format!("MX10 timestamp apply failed: {applied:?}")));}}
   let header=api.finalize_block(parent).map_err(error)?;drop(api);
   if header.parent_hash()!=&parent || *header.number()!=number {return Err(error("runtime returned wrong MX10 parent/height"));}
   let difficulty=algorithm.difficulty(parent).map_err(error)?;
   let target=crate::gpu_pow_consensus::target_for_difficulty(difficulty).map_err(error)?;
   let pre_hash=header.hash();let challenge=mx::challenge_from_pre_hash(genesis,pre_hash);
   let mut base=0u64;let mut searched=0u64;
   let cert=loop {
    if Instant::now()>=deadline {return Err(error("MX10 nonce search deadline"));}
    let batch=if let Some(ref mut session)=gpu {session.batch(challenge,base,16).map_err(error)?}else{vec![mx::compute(shape(),challenge,base)]};
    searched+=batch.len() as u64;
    if let Some(c)=batch.into_iter().find(|c|mx::ticket(c)<=target) {break c;}
    base=base.checked_add(if gpu.is_some(){16}else{1}).ok_or_else(||error("nonce overflow"))?;
   };
   let seal=cert.encode_fixed();let checked=mx::verify_pre_hash(shape(),genesis,pre_hash,&seal,target).map_err(|e|error(format!("MX10 authored seal: {e:?}")))?;
   if cert.version!=10 || seal.len()!=105 {return Err(error("MX10 encoding invariant"));}
   if client.info().best_hash!=parent {return Err(error("MX10 parent changed; stopping without stale submit"));}
   let mut params=BlockImportParams::<Block>::new(BlockOrigin::Own,header.clone());params.body=Some(body.clone());params.post_digests.push(DigestItem::Seal(POW_ENGINE_ID,seal.clone()));
   params.insert_intermediate(INTERMEDIATE_KEY,PowIntermediate::<u128>{difficulty:None});
   let sealed=seal_header(header,&seal);let hash=sealed.hash();
   let imported=importer.import_block(params).await.map_err(error)?;
   if !matches!(imported,ImportResult::Imported(_)) || client.info().best_hash!=hash {return Err(error(format!("MX10 import not observed: {imported:?}")));}
   let record=serde_json::json!({"height":number,"parent":format!("{parent:?}"),"hash":format!("{hash:?}"),"pre_hash":format!("{pre_hash:?}"),"challenge":cert.challenge,"nonce":cert.nonce,"backend":backend,"nonces_computed":searched,"difficulty":difficulty,"target":target,"version":cert.version,"certificate_bytes":seal.len(),"seal_hex":seal.iter().map(|b|format!("{b:02x}")).collect::<String>(),"verify_pre_hash":format!("Ok({checked:?})"),"timestamp_ms":timestamp,"import":format!("{imported:?}"),"parent_committed_before_next_proposal":true});
   writeln!(receipts,"{}",record).map_err(error)?;receipts.sync_data().map_err(error)?;
   writeln!(block_stream,"{}",serde_json::to_string(&SignedBlock{block:Block::new(sealed,body),justifications:None}).map_err(error)?).map_err(error)?;
   records.push(record);eprintln!("MX10_AUTHORED height={number} parent={parent:?} hash={hash:?} backend={backend} verify=OK");
  }
  block_stream.sync_all().map_err(error)?;
  let gpu_report=if let Some(session)=gpu {Some(session.finish().map_err(error)?)}else{None};
  let summary=serde_json::json!({"schema":"SVY-MX10-BOUNDED-AUTHOR-1","genesis":format!("{genesis:?}"),"backend":backend,"fallback_reason":fallback,"blocks":records,"gpu":gpu_report,"elapsed_seconds":started.elapsed().as_secs_f64(),"node_started":false,"network_or_rpc_started":false,"authoring_path":"new CLI --mx10-author -> service::mx10_author::run -> real client RuntimeApi Core_initialize_block / BlockBuilder_inherent_extrinsics / apply_extrinsic / finalize_block -> MX10 nonce search -> CPU verify_pre_hash -> PowBlockImport::import_block -> runtime execution+commit -> next committed parent","not_prebuilt_import":true,"beneficiary_note":"public test-only AccountId([11;32]), not a production wallet"});
  fs::write(out.join("summary.json"),serde_json::to_vec_pretty(&summary).map_err(error)?).map_err(error)?;Ok(())
 };
 Ok((future,task_manager))
}

// Shared byte-identical final seal assembly, exposed for live template CPU sealing.
pub(crate) fn seal_header(mut header: Header, seal: &[u8]) -> Header {
 header.digest_mut().push(DigestItem::Seal(POW_ENGINE_ID,seal.to_vec()));
 header
}
