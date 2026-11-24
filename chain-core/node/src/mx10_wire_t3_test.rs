//! Test-only CPU fixtures and unfrozen MX10 real-path T3 evidence.
use super::{WorkAlgorithm, EXPERIMENTAL_TARGET};
use crate::gpu_pow_mx10 as mx;
use eagle_runtime::{opaque::Block, AccountId, Header, PowBeneficiaryV1, RuntimeCall, TimestampCall, UncheckedExtrinsic, POW_ENGINE_ID};
use sp_runtime::{codec::{Encode, Decode, DecodeAll}, generic::{Digest, DigestItem, SignedBlock}, traits::{Header as _, Block as _, BlakeTwo256, Hash as _}, OpaqueExtrinsic, StateVersion};
use sp_core::H256;
use sc_executor::{WasmExecutor, WasmExecutionMethod};
use sc_executor::WasmtimeInstantiationStrategy;
use sp_core::traits::{CallContext, CodeExecutor, RuntimeCode, WrappedRuntimeCode};
use std::{fs, path::PathBuf, process::Command, time::Instant};

fn dir() -> PathBuf { PathBuf::from(std::env::var("MX10_T3_OUT").expect("explicit fresh evidence directory")) }
fn shape() -> mx::EvalShape { mx::EvalShape::new(16,2048).unwrap() }
fn beneficiary() -> Vec<u8> { PowBeneficiaryV1::new(AccountId::new([11;32])).encode_payload() }
fn hex(s:&str)->Vec<u8> { let s=s.trim_start_matches("0x"); (0..s.len()).step_by(2).map(|i|u8::from_str_radix(&s[i..i+2],16).unwrap()).collect() }
fn call(w:&WasmExecutor<sp_io::SubstrateHostFunctions>, code:&[u8], method:&str, input:&[u8])->Vec<u8>{
 let fetch=WrappedRuntimeCode(std::borrow::Cow::Borrowed(code));
 let c=RuntimeCode{code_fetcher:&fetch,heap_pages:None,hash:sp_core::blake2_256(code).to_vec()};
 let (result,native)=sp_externalities::with_externalities(|ext|w.call(ext,&c,method,input,CallContext::Onchain)).unwrap();
 assert!(!native);result.unwrap()
}
fn finalized(w:&WasmExecutor<sp_io::SubstrateHostFunctions>,code:&[u8],parent:H256,number:u32,millis:u64)->(Header,OpaqueExtrinsic){
 let xt=UncheckedExtrinsic::new_bare(RuntimeCall::Timestamp(TimestampCall::set{now:millis}));
 let encoded=xt.encode();let opaque=OpaqueExtrinsic::decode(&mut &encoded[..]).unwrap();
 let root=BlakeTwo256::ordered_trie_root(vec![encoded.clone()],StateVersion::V1);
 let h=Header::new(number,Default::default(),root,parent,Digest{logs:vec![DigestItem::PreRuntime(POW_ENGINE_ID,beneficiary())]});
 let init=call(w,code,"Core_initialize_block",&h.encode());
 let _:sp_runtime::ExtrinsicInclusionMode=DecodeAll::decode_all(&mut &init[..]).unwrap();
 let applied=call(w,code,"BlockBuilder_apply_extrinsic",&encoded);
 let result:sp_runtime::ApplyExtrinsicResult=DecodeAll::decode_all(&mut &applied[..]).unwrap();assert!(matches!(result,Ok(Ok(()))));
 let fin=call(w,code,"BlockBuilder_finalize_block",&[]);(Header::decode_all(&mut &fin[..]).unwrap(),opaque)
}
fn cert(genesis:H256,pre:H256,above:bool)->mx::Certificate{
 let ch=mx::challenge_from_pre_hash(genesis,pre);
 for nonce in 0..8192 {let c=mx::compute(shape(),ch,nonce);if (mx::ticket(&c)>EXPERIMENTAL_TARGET)==above{return c;}}panic!("nonce budget exhausted")
}
fn cli(label:&str,args:&[&str])->(Vec<u8>,String,u128){
 let t=Instant::now();let o=Command::new("/mnt/f/POAH/eagle-ai-network/target/release/eagle-node").args(args).env_remove("EAGW_POW_FINALITY_ONLY").env("MX10_T3_TRACE","1").output().unwrap();let ns=t.elapsed().as_nanos();
 let err=String::from_utf8_lossy(&o.stderr).into_owned();let d=dir();fs::write(d.join(format!("logs/{label}.log")),&err).unwrap();
 fs::write(d.join(format!("logs/{label}.run.json")),serde_json::to_vec_pretty(&serde_json::json!({"args":args,"exit":o.status.code(),"wall_ns":ns})).unwrap()).unwrap();if !label.starts_with("stage7-reject-") { assert!(o.status.success(),"{label}: {err}"); }(o.stdout,err,ns)
}
#[test]
fn mx10_dual_dispatch_spec_guards(){
 let v2=crate::mx10_context::tests::spec(crate::chain_spec::POW_MATRIX_BLOCK_REWARDS_LOCAL_ID,"integer-matrix-2x2048x16");
 let mx=crate::mx10_context::tests::spec(crate::chain_spec::POW_MX10_LOCAL_ID,crate::chain_spec::MX10_WORK_ALGORITHM);
 assert_eq!(WorkAlgorithm::from_spec(&v2).unwrap(),WorkAlgorithm::V2);
 assert_eq!(WorkAlgorithm::from_spec(&mx).unwrap(),crate::mx10_context::tests::dispatch());
 let g=crate::mx10_context::tests::context().genesis();let p=H256::repeat_byte(32);
 let c=cert(g,p,false).encode_fixed();assert!(crate::mx10_context::tests::dispatch().verify(g,p,&c,EXPERIMENTAL_TARGET));assert!(!WorkAlgorithm::V2.verify(g,p,&c,EXPERIMENTAL_TARGET));
 let ch=crate::gpu_pow::challenge_from_pre_hash(g,p);
 let c=(0..8192).map(|n|crate::gpu_pow::compute(ch,n)).find(|c|crate::gpu_pow::ticket(c)<=EXPERIMENTAL_TARGET).unwrap().encode_fixed();
 assert!(WorkAlgorithm::V2.verify(g,p,&c,EXPERIMENTAL_TARGET));assert!(!crate::mx10_context::tests::dispatch().verify(g,p,&c,EXPERIMENTAL_TARGET));
}
#[test]
#[ignore="explicit unfrozen MX10 T3 release evidence only; no GPU"]
fn mx10_wire_real_path_t3(){
 let d=dir();fs::create_dir_all(d.join("blocks")).unwrap();
 let chain="pow-mx10-n16-local";let base=d.join("mx10-db").to_string_lossy().into_owned();assert!(!PathBuf::from(&base).exists());
 let (spec,_,_)=cli("mx10-build-spec",&["build-spec","--chain",chain,"--raw"]);fs::write(d.join("mx10-chain-spec-raw.json"),&spec).unwrap();
 let (v2,_,_)=cli("v2-build-spec",&["build-spec","--chain","pow-matrix-block-rewards-local","--raw"]);fs::write(d.join("v2-chain-spec-raw.json"),&v2).unwrap();
 let export=d.join("mx10-genesis.json").to_string_lossy().into_owned();cli("mx10-genesis",&["export-blocks","--chain",chain,"--base-path",&base,"--from","0","--to","0",&export]);
 let genesis:SignedBlock<Block>=serde_json::from_slice(&fs::read(&export).unwrap()).unwrap();let g=genesis.block.header().hash();assert_ne!(format!("{g:?}"),"0xfd4116958eb485761cd994b7145a1382892178f980babed8b2b3a8a99d978585");
 let vbase=d.join("v2-db").to_string_lossy().into_owned();let vex=d.join("v2-genesis.json").to_string_lossy().into_owned();cli("v2-genesis",&["export-blocks","--chain","pow-matrix-block-rewards-local","--base-path",&vbase,"--from","0","--to","0",&vex]);
 let vg:SignedBlock<Block>=serde_json::from_slice(&fs::read(&vex).unwrap()).unwrap();// Preserve the actual pre-task V2 import-queue genesis, not a supplied
 // identifier that differs from the untouched reference raw chain spec.
 let prior:serde_json::Value=serde_json::from_slice(&fs::read("/mnt/f/POAH/eagle-ai-network/.local/matrix-phase3-20261106-01/phase3/release-import/import-queue/import.json").unwrap()).unwrap();
 assert_eq!(format!("{:?}",vg.block.header().hash()),prior["genesis_hash"].as_str().unwrap());
 let doc:serde_json::Value=serde_json::from_slice(&spec).unwrap();let mut storage=sp_core::storage::Storage::default();for(k,v)in doc["genesis"]["raw"]["top"].as_object().unwrap(){storage.top.insert(hex(k),hex(v.as_str().unwrap()));}
 let code=storage.top.remove(b":code".as_slice()).unwrap();let ext=sp_io::TestExternalities::new_with_code_and_state(&code,storage,StateVersion::V1);
 let(raw,root)=ext.into_raw_snapshot();let mut ext=sp_io::TestExternalities::from_raw_snapshot(raw,root,StateVersion::V1);
 assert_eq!(root,*genesis.block.header().state_root());
 let wasm=WasmExecutor::<sp_io::SubstrateHostFunctions>::builder().with_execution_method(WasmExecutionMethod::Compiled{instantiation_strategy:WasmtimeInstantiationStrategy::RecreateInstance}).with_max_runtime_instances(1).with_runtime_cache_size(2).build();
 let mut parent=g;let mut stream=Vec::new();let mut meta=Vec::new();let mut first=None;
 for number in 1..=8 {
  let(h,body)=ext.execute_with(||finalized(&wasm,&code,parent,number,1_790_000_000_000+u64::from(number)*6000));let pre=h.hash();let c=cert(g,pre,false);let seal=c.encode_fixed();assert!(crate::mx10_context::tests::dispatch().verify(g,pre,&seal,EXPERIMENTAL_TARGET));
  if number==1{first=Some((h.clone(),body.clone(),c.clone()));}
  let mut sh=h;sh.digest_mut().push(DigestItem::Seal(POW_ENGINE_ID,seal));let hash=sh.hash();meta.push(serde_json::json!({"height":number,"parent":format!("{parent:?}"),"pre_hash":format!("{pre:?}"),"hash":format!("{hash:?}"),"nonce":c.nonce}));
  stream.extend_from_slice(&serde_json::to_vec(&SignedBlock{block:Block::new(sh,vec![body]),justifications:None}).unwrap());stream.push(b'\n');parent=hash;
 }
 let legal=d.join("blocks/legal-multilevel.json");fs::write(&legal,&stream).unwrap();fs::write(d.join("fixtures.json"),serde_json::to_vec_pretty(&meta).unwrap()).unwrap();
 let(h,body,c)=first.unwrap();let pre=h.hash();
 let above=cert(g,pre,true);let mut wrong=c.clone();wrong.output_digest[0]^=1;while mx::ticket(&wrong)>EXPERIMENTAL_TARGET {wrong.nonce+=1;}
 let mut version=c.encode_fixed();version[0]=2;let mut challenge=c.encode_fixed();challenge[1]^=1;let mut trailing=c.encode_fixed();trailing.push(0);
 let cases=vec![("legal",c.encode_fixed()),("above_target",above.encode_fixed()),("work_mismatch",wrong.encode_fixed()),("length",vec![0;104]),("version",version),("challenge",challenge),("trailing",trailing)];
 let mut samples=Vec::new();for(name,seal)in &cases{for i in 0..100{let t=Instant::now();let result=mx::verify_pre_hash(shape(),g,pre,seal,EXPERIMENTAL_TARGET);let ns=t.elapsed().as_nanos();assert_eq!(result.is_ok(),*name=="legal");samples.push(serde_json::json!({"case":name,"iteration":i,"elapsed_ns":ns,"result":format!("{result:?}"),"path":"gpu_pow_mx10::verify_pre_hash","profile":"release"}));}}
 fs::write(d.join("stage3-samples.jsonl"),samples.iter().map(|v|v.to_string()+"\n").collect::<String>()).unwrap();
 let (_,log,wall)=cli("stage7-legal",&["import-blocks",legal.to_str().unwrap(),"--chain",chain,"--base-path",&base,"--detailed-log-output","-l","sync=trace,pow=trace,mx10_t3=debug"]);assert!(log.contains("Imported 8 blocks"),"{log}");assert!(!log.contains("error importing"));assert!(log.contains("MX10_VERIFY_PRE_HASH"));
 let export=d.join("mx10-export-1-8.json").to_string_lossy().into_owned();cli("mx10-export-imported",&["export-blocks","--chain",chain,"--base-path",&base,"--from","1","--to","8",&export]);
 for(name,seal)in cases.iter().skip(1){let mut sh=h.clone();sh.digest_mut().push(DigestItem::Seal(POW_ENGINE_ID,seal.clone()));let f=d.join(format!("blocks/reject-{name}.json"));fs::write(&f,serde_json::to_vec(&SignedBlock{block:Block::new(sh,vec![body.clone()]),justifications:None}).unwrap()).unwrap();let(_,log,_)=cli(&format!("stage7-reject-{name}"),&["import-blocks",f.to_str().unwrap(),"--chain",chain,"--base-path",&base,"--detailed-log-output","-l","sync=trace,pow=trace,mx10_t3=debug"]);assert!(log.contains("invalid seal"),"{name}: {log}");}
 fs::write(d.join("measurement-result.json"),serde_json::to_vec_pretty(&serde_json::json!({"mx10_genesis":format!("{g:?}"),"v2_genesis":format!("{:?}",vg.block.header().hash()),"stage7_legal_n":8,"stage7_legal_cli_wall_ns":wall,"stage7_invalid_n":6,"stage3_n_per_case":100,"chain_shape":"multi-height 1..8, each sealed parent hash links to prior block","node_started":false,"features":"gpu-pow-matrix-block-rewards-dev"})).unwrap()).unwrap();
}
