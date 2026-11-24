//! Metadata-only identity fixtures: no chain-spec generator/genesis/keys/node.
use super::*;
use crate::gpu_pow_consensus::{WorkAlgorithm, EXPERIMENTAL_TARGET};
pub(crate) fn spec(id: &str, work: &str) -> crate::chain_spec::ChainSpec {
    let key = format!("0x{}", b"SVY-MX10-ISOLATED-WORKID\0".iter().map(|b|format!("{b:02x}")).collect::<String>());
    let value = serde_json::json!({"name":"metadata-only-test","id":id,"chainType":"Local","bootNodes":[],
        "properties":{"workAlgorithm":work},"genesis":{"raw":{"top":{(key):format!("0x{}",chain_spec::MX10_WORK_ALGORITHM)},"childrenDefault":{}}}});
    crate::chain_spec::ChainSpec::from_json_bytes(serde_json::to_vec(&value).unwrap()).unwrap()
}
pub(crate) fn existing_spec(mx10: bool) -> crate::chain_spec::ChainSpec {
    let file=if mx10 {"mx10-chain-spec-raw.json"}else{"v2-chain-spec-raw.json"};
    let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    crate::chain_spec::ChainSpec::from_json_bytes(std::fs::read(root.join(".local/svy-mx10-wire-t3-20261106-01").join(file)).unwrap()).unwrap()
}
pub(crate) fn dispatch() -> WorkAlgorithm {
    WorkAlgorithm::from_spec(&spec(chain_spec::POW_MX10_LOCAL_ID,chain_spec::MX10_WORK_ALGORITHM)).unwrap()
}
pub(crate) fn context() -> Mx10ChainContext {
    match dispatch() { WorkAlgorithm::Mx10N16(c) => c, _ => panic!("MX10 fixture") }
}
#[test]
fn production_identity_has_no_successful_constructor() { assert!(Mx10ChainContext::production().is_err()); }
#[test]
fn production_or_feature_alone_cannot_select_mx10() {
    assert!(WorkAlgorithm::from_spec(&spec("production-forgery",chain_spec::MX10_WORK_ALGORITHM)).is_err());
    assert!(WorkAlgorithm::from_spec(&spec(chain_spec::SVY_MATRIX_LIVE_ID,chain_spec::MX10_WORK_ALGORITHM)).is_err());
    if !cfg!(feature="gpu-pow-matrix-block-rewards-dev") || cfg!(feature="svy-matrix-live") {
        assert!(Mx10ChainContext::isolated(&spec(chain_spec::POW_MX10_LOCAL_ID,chain_spec::MX10_WORK_ALGORITHM)).is_err());
    }
}
#[cfg(all(feature="gpu-pow-matrix-block-rewards-dev",not(feature="svy-matrix-live")))]
#[test]
fn immutable_context_rejects_genesis_profile_version_and_request_replacement() {
    use sc_service::ChainSpec as _;
    let c=context();assert_eq!(c.version(),10);assert_eq!(c.certificate_bytes(),105);
    assert_eq!(c.ensure_request(ISOLATED_GENESIS),Ok(()));
    assert!(c.ensure_request("0x0000000000000000000000000000000000000000000000000000000000000000").is_err());
    assert!(c.ensure_genesis(H256::repeat_byte(8)).is_err());
    let base=spec(chain_spec::POW_MX10_LOCAL_ID,chain_spec::MX10_WORK_ALGORITHM);
    let value:serde_json::Value=serde_json::from_str(&base.as_json(true).unwrap()).unwrap();
    for (field,bad) in [("profile",serde_json::json!("mx10-n32")),("version",serde_json::json!(2))] {
        let mut v=value.clone();v["properties"][field]=bad;
        let s=crate::chain_spec::ChainSpec::from_json_bytes(serde_json::to_vec(&v).unwrap()).unwrap();
        assert!(Mx10ChainContext::isolated(&s).is_err());
    }
    let mut v=value;v["chainType"]="Live".into();
    let s=crate::chain_spec::ChainSpec::from_json_bytes(serde_json::to_vec(&v).unwrap()).unwrap();
    assert!(Mx10ChainContext::isolated(&s).is_err());
}
#[cfg(all(feature="gpu-pow-matrix-block-rewards-dev",not(feature="svy-matrix-live")))]
#[test]
fn original_verifier_encoding_target_and_rejection_order_unchanged() {
    let c=context();let p=H256::repeat_byte(45);let challenge=mx::challenge_from_pre_hash(c.genesis(),p);
    let cert=(0..128).map(|n|mx::compute(c.shape(),challenge,n)).find(|c|mx::ticket(c)<=EXPERIMENTAL_TARGET).unwrap();
    let good=cert.encode_fixed();assert_eq!(good.len(),105);
    assert!(c.verify(p,&good,EXPERIMENTAL_TARGET).is_ok());
    let mut bad=good.clone();bad[0]=2;
    assert_eq!(c.verify(p,&bad,[0;32]),Err(mx::Error::UnsupportedVersion));
    bad=good.clone();bad[1]^=1;assert_eq!(c.verify(p,&bad,[0;32]),Err(mx::Error::ChallengeMismatch));
    assert_eq!(c.verify(p,&good,[0;32]),Err(mx::Error::AboveTarget));
    assert_eq!(c.verify(p,&good[..104],EXPERIMENTAL_TARGET),Err(mx::Error::InvalidEncoding));
    let mut mismatch=good.clone();let last=mismatch.len()-1;mismatch[last]^=1;
    assert_eq!(c.verify(p,&mismatch,[255;32]),Err(mx::Error::WorkMismatch));
    assert!(c.verify(p,&good,mx::ticket(&cert)).is_ok());
    assert!(!dispatch().verify(H256::repeat_byte(9),p,&good,EXPERIMENTAL_TARGET));
    assert_eq!(c.verify(p,&good,EXPERIMENTAL_TARGET),mx::verify_pre_hash(c.shape(),c.genesis(),p,&good,EXPERIMENTAL_TARGET));
}

#[cfg(all(feature="gpu-pow-matrix-block-rewards-dev",not(feature="svy-matrix-live")))]
mod real_path {
    use super::*;
    use crate::gpu_pow_consensus::{DifficultyChain, DynamicDifficultyAlgorithm, StrictPowVerifier};
    use eagle_runtime::{opaque::Block, Header, POW_ENGINE_ID};
    use sc_consensus::{BlockImport, BlockCheckParams, BlockImportParams, ImportResult};
    use sc_consensus::import_queue::{BasicQueue, ImportQueue, IncomingBlock, Link, BlockImportStatus, BlockImportError};
    use sc_consensus_pow::{PowAlgorithm, PowAux};
    use sp_runtime::{codec::{Encode, DecodeAll}, generic::{Digest,DigestItem,SignedBlock}, traits::Header as _, OpaqueExtrinsic, StateVersion};
    use sp_core::traits::{CodeExecutor, RuntimeCode, WrappedRuntimeCode, CallContext, SpawnEssentialNamed};
    use std::sync::{Arc,Mutex};

    struct ExistingParent(Header);
    impl DifficultyChain for ExistingParent {
        fn pow_header(&self,hash:H256)->Result<Header,sc_consensus_pow::Error<Block>> {
            if hash!=self.0.hash(){return Err(sc_consensus_pow::Error::Other("unknown fixture parent".into()));}
            Ok(self.0.clone())
        }
        fn pow_timestamp(&self,_:H256)->Result<u64,sc_consensus_pow::Error<Block>> {Ok(0)}
        fn pow_aux(&self,_:H256)->Result<Option<PowAux<u128>>,sc_consensus_pow::Error<Block>> {Ok(None)}
    }
    fn unhex(s:&str)->Vec<u8> {
        let s=s.trim_start_matches("0x");(0..s.len()).step_by(2).map(|i|u8::from_str_radix(&s[i..i+2],16).unwrap()).collect()
    }
    fn runtime_proposal(c:Mx10ChainContext)->(Header,Vec<OpaqueExtrinsic>,Header) {
        use sc_executor::{WasmExecutor,WasmExecutionMethod,WasmtimeInstantiationStrategy};
        use sp_inherents::InherentDataProvider;
        let root=std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
        // Read existing, preserved isolated artifacts. Never call a spec builder,
        // generate a genesis, open a chain database or launch a node process.
        let directory=root.join(".local/svy-mx10-wire-t3-20261106-01");
        let genesis:SignedBlock<Block>=serde_json::from_slice(&std::fs::read(directory.join("mx10-genesis.json")).unwrap()).unwrap();
        let parent=genesis.block.header.clone();c.ensure_genesis(parent.hash()).unwrap();
        let doc:serde_json::Value=serde_json::from_slice(&std::fs::read(directory.join("mx10-chain-spec-raw.json")).unwrap()).unwrap();
        let mut storage=sp_core::storage::Storage::default();
        for(k,v)in doc["genesis"]["raw"]["top"].as_object().unwrap(){storage.top.insert(unhex(k),unhex(v.as_str().unwrap()));}
        let code=storage.top.remove(b":code".as_slice()).unwrap();
        let mut ext=sp_io::TestExternalities::new_with_code_and_state(&code,storage,StateVersion::V1);
        let wasm=WasmExecutor::<sp_io::SubstrateHostFunctions>::builder().with_execution_method(WasmExecutionMethod::Compiled{instantiation_strategy:WasmtimeInstantiationStrategy::RecreateInstance}).with_max_runtime_instances(1).with_runtime_cache_size(1).build();
        let call=|method:&str,input:&[u8]| {
            let fetch=WrappedRuntimeCode(std::borrow::Cow::Borrowed(&code));
            let runtime=RuntimeCode{code_fetcher:&fetch,heap_pages:None,hash:sp_core::blake2_256(&code).to_vec()};
            let(result,native)=sp_externalities::with_externalities(|ext|wasm.call(ext,&runtime,method,input,CallContext::Onchain)).unwrap();
            assert!(!native);result.unwrap()
        };
        let beneficiary=eagle_runtime::PowBeneficiaryV1::new(eagle_runtime::AccountId::new([11;32])).encode_payload();
        let initial=Header::new(1,Default::default(),Default::default(),c.genesis(),Digest{logs:vec![DigestItem::PreRuntime(POW_ENGINE_ID,beneficiary)]});
        let mut inherent=sp_inherents::InherentData::new();
        futures::executor::block_on(sp_timestamp::InherentDataProvider::new(1_790_000_006_000u64.into()).provide_inherent_data(&mut inherent)).unwrap();
        let(header,body)=ext.execute_with(|| {
            let init=call("Core_initialize_block",&initial.encode());
            let _=sp_runtime::ExtrinsicInclusionMode::decode_all(&mut &init[..]).unwrap();
            let encoded=call("BlockBuilder_inherent_extrinsics",&inherent.encode());
            let body=Vec::<OpaqueExtrinsic>::decode_all(&mut &encoded[..]).unwrap();assert!(!body.is_empty());
            for xt in &body {
                let result=call("BlockBuilder_apply_extrinsic",&xt.encode());
                let result=sp_runtime::ApplyExtrinsicResult::decode_all(&mut &result[..]).unwrap();assert!(matches!(result,Ok(Ok(()))));
            }
            let result=call("BlockBuilder_finalize_block",&[]);
            (Header::decode_all(&mut &result[..]).unwrap(),body)
        });
        assert_eq!(header.parent_hash(),&c.genesis());
        (header,body,parent)
    }
    #[derive(Clone)]
    struct ThreadSpawner;
    impl SpawnEssentialNamed for ThreadSpawner {
        fn spawn_essential_blocking(&self,_:&'static str,_:Option<&'static str>,future:futures::future::BoxFuture<'static,()>){
            std::thread::spawn(move||futures::executor::block_on(future));
        }
        fn spawn_essential(&self,n:&'static str,g:Option<&'static str>,f:futures::future::BoxFuture<'static,()>){self.spawn_essential_blocking(n,g,f)}
    }
    struct CheckedSink { algorithm:DynamicDifficultyAlgorithm<ExistingParent>, accepted:Arc<Mutex<Vec<H256>>> }
    #[async_trait::async_trait]
    impl BlockImport<Block> for CheckedSink {
        type Error=sp_consensus::Error;
        async fn check_block(&self,_:BlockCheckParams<Block>)->Result<ImportResult,Self::Error>{Ok(ImportResult::imported(false))}
        async fn import_block(&self,block:BlockImportParams<Block>)->Result<ImportResult,Self::Error>{
            // BasicQueue's StrictPowVerifier strips the final seal. The actual
            // service PowBlockImport then calls this SAME difficulty/verify API.
            // This controlled terminal sink records admission only, no DB commit.
            let seal=match &block.post_digests[0]{DigestItem::Seal(_,s)=>s,_=>panic!("seal")};
            let beneficiary=match &block.header.digest().logs[0]{DigestItem::PreRuntime(_,b)=>b,_=>panic!("beneficiary")};
            let parent=*block.header.parent_hash();let difficulty=self.algorithm.difficulty(parent).unwrap();
            if !self.algorithm.verify(&sp_runtime::generic::BlockId::hash(parent),&block.header.hash(),Some(beneficiary),seal,difficulty).unwrap(){
                return Err(sp_consensus::Error::ClientImport("invalid controlled fixture seal".into()));
            }
            assert!(!block.state_action.skip_execution_checks());
            self.accepted.lock().unwrap().push(block.header.hash());Ok(ImportResult::imported(false))
        }
    }
    struct Results(Arc<Mutex<Vec<bool>>>);
    impl Link<Block> for Results {
        fn blocks_processed(&self,_:usize,_:usize,results:Vec<(Result<BlockImportStatus<u32>,BlockImportError>,H256)>){
            self.0.lock().unwrap().extend(results.into_iter().map(|(r,_)|r.is_ok()));
        }
    }
    #[test]
    fn runtime_api_proposal_context_target_and_real_basic_queue_admission() {
        let c=context();let(header,body,parent)=runtime_proposal(c);
        let chain=Arc::new(ExistingParent(parent));let target=c.target(chain.clone(),c.genesis()).unwrap();
        assert_eq!(target,EXPERIMENTAL_TARGET);
        let replaced=DynamicDifficultyAlgorithm::new(H256::repeat_byte(99),chain.clone()).with_work_algorithm(WorkAlgorithm::Mx10N16(c));
        assert!(replaced.difficulty(c.genesis()).is_err());
        let algorithm=DynamicDifficultyAlgorithm::new(c.genesis(),chain).with_work_algorithm(WorkAlgorithm::Mx10N16(c));
        let pre=header.hash();let challenge=mx::challenge_from_pre_hash(c.genesis(),pre);
        let cert=(0..128).map(|n|mx::compute(c.shape(),challenge,n)).find(|v|mx::ticket(v)<=target).unwrap();
        let seal=cert.encode_fixed();assert!(c.verify(pre,&seal,target).is_ok());
        let accepted=Arc::new(Mutex::new(Vec::new()));let results=Arc::new(Mutex::new(Vec::new()));
        let mut queue=BasicQueue::new(StrictPowVerifier::new(algorithm.clone()),Box::new(CheckedSink{algorithm,accepted:accepted.clone()}),None,&ThreadSpawner,None);
        let mut incoming=Vec::new();
        for valid in [true,false] {
            let mut h=header.clone();let mut s=seal.clone();if !valid{s[0]=2;}
            h.digest_mut().push(DigestItem::Seal(POW_ENGINE_ID,s));
            incoming.push(IncomingBlock::<Block>{hash:h.hash(),header:Some(h),body:Some(body.clone()),indexed_body:None,justifications:None,origin:None,allow_missing_state:false,skip_execution:false,import_existing:false,state:None});
        }
        queue.service().import_blocks(sp_consensus::BlockOrigin::Own,incoming);
        let link=Results(results.clone());let waker=futures::task::noop_waker();let mut cx=std::task::Context::from_waker(&waker);
        let deadline=std::time::Instant::now()+std::time::Duration::from_secs(15);
        while results.lock().unwrap().len()<2 {
            queue.poll_actions(&mut cx,&link);
            assert!(std::time::Instant::now()<deadline,"BasicQueue completion deadline");
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert_eq!(*results.lock().unwrap(),vec![true,false]);assert_eq!(*accepted.lock().unwrap(),vec![pre]);
    }
}
