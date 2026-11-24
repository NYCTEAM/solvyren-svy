//! Test-only helpers for injecting a forged EAGW block into a real import queue.
//!
//! These helpers do not start nodes and are never exposed as RPC. A future
//! isolated-node fixture must supply the real B queue, a valid executable
//! candidate/body with the correct Aura pre-digest/slot, the slot-author key,
//! and a stable client snapshot. Queue completion must be bounded by the caller.

use crate::gpu_work_binding::{candidate_subject, recompute_cpu_work, seal_candidate};
use eagle_runtime::{opaque::Block, Header};
use futures::future::poll_fn;
use sc_consensus::import_queue::{
    BlockImportError, BlockImportStatus, ImportQueue, IncomingBlock, Link,
};
use sp_consensus::BlockOrigin;
use sp_core::{sr25519, Pair, H256};
use sp_runtime::{codec::Encode, generic::DigestItem, traits::Header as HeaderT, OpaqueExtrinsic};
use std::sync::Mutex;

/// Capture both best and finalised chain state. Fixtures must quiesce other
/// imports while this test runs, or concurrent legitimate changes can race it.
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct ChainSnapshot {
    pub best_hash: H256,
    pub best_number: u32,
    pub finalized_hash: H256,
    pub finalized_number: u32,
}

/// The provided candidate must already have the right slot, parent, state root,
/// body root and inherents for B's chain; the helper adds no consensus slot.
pub(crate) fn invalid_work_signed_by_aura(
    candidate: &Header,
    body: Vec<OpaqueExtrinsic>,
    aura_author: &sr25519::Pair,
) -> Result<IncomingBlock<Block>, String> {
    let subject = candidate_subject(candidate).map_err(|e| format!("candidate: {e:?}"))?;
    let mut wrong = recompute_cpu_work(subject).commitment;
    wrong[0] ^= 1;
    let mut header = seal_candidate(candidate, wrong).map_err(|e| format!("seal: {e:?}"))?;
    // Aura signs the header containing EAGW, before the Aura seal is appended.
    let signature = aura_author.sign(header.hash().as_ref());
    header.digest_mut().push(DigestItem::Seal(
        sp_consensus_aura::AURA_ENGINE_ID,
        signature.encode(),
    ));
    Ok(IncomingBlock {
        hash: header.hash(),
        header: Some(header),
        body: Some(body),
        indexed_body: None,
        justifications: None,
        origin: None,
        allow_missing_state: false,
        skip_execution: false,
        import_existing: false,
        state: None,
    })
}

struct RejectionLink {
    expected_hash: H256,
    result: Mutex<Option<Result<BlockImportStatus<u32>, BlockImportError>>>,
}

impl Link<Block> for RejectionLink {
    fn blocks_processed(
        &self,
        _imported: usize,
        _count: usize,
        results: Vec<(Result<BlockImportStatus<u32>, BlockImportError>, H256)>,
    ) {
        let mut slot = self.result.lock().expect("test callback poisoned");
        for (result, hash) in results {
            if hash == self.expected_hash && slot.is_none() {
                *slot = Some(result);
            }
        }
    }
}

/// Inject one block through the supplied actual queue and verify the cause and
/// chain snapshot. The caller must enforce an external timeout; an idle or
/// closed queue does not complete this future on its own.
pub(crate) async fn assert_queue_rejects_invalid_work<Q, S>(
    queue: &mut Q,
    incoming: IncomingBlock<Block>,
    snapshot: S,
) -> Result<(), String>
where
    Q: ImportQueue<Block>,
    S: Fn() -> Result<ChainSnapshot, String>,
{
    let before = snapshot()?;
    let link = RejectionLink {
        expected_hash: incoming.hash,
        result: Mutex::new(None),
    };
    queue
        .service_ref()
        .import_blocks(BlockOrigin::NetworkBroadcast, vec![incoming]);
    let result = poll_fn(|cx| {
        queue.poll_actions(cx, &link);
        match link.result.lock().expect("test callback poisoned").take() {
            Some(result) => std::task::Poll::Ready(result),
            None => std::task::Poll::Pending,
        }
    })
    .await;
    match result {
        Err(BlockImportError::VerificationFailed(_, reason))
            if reason.contains("invalid EAGW work: WorkCommitmentMismatch") => {}
        other => return Err(format!("expected EAGW work rejection, got {other:?}")),
    }
    let after = snapshot()?;
    if before != after {
        return Err(format!(
            "chain changed during isolated negative test: before={before:?} after={after:?}"
        ));
    }
    Ok(())
}

#[test]
fn forged_work_is_aura_signed_after_the_forgery() {
    use sp_runtime::generic::Digest;
    let mut candidate = Header::new(
        42,
        H256::repeat_byte(1),
        H256::repeat_byte(2),
        H256::repeat_byte(3),
        Digest::default(),
    );
    let slot = 2u64.into();
    candidate
        .digest_mut()
        .push(sc_consensus_aura::standalone::pre_digest::<sr25519::Pair>(
            slot,
        ));
    let pair = sr25519::Pair::from_seed(&[8; 32]);
    let incoming = invalid_work_signed_by_aura(&candidate, vec![], &pair).unwrap();
    let signed = incoming.header.unwrap();
    let (header, verified_slot, _) = sc_consensus_aura::standalone::check_header_slot_and_seal::<
        Block,
        sr25519::Pair,
    >(slot, signed.clone(), &[pair.public()])
    .unwrap();
    assert_eq!(verified_slot, slot);
    let mut signed_again = signed;
    let signature = match signed_again.digest_mut().logs.pop().unwrap() {
        DigestItem::Seal(id, bytes) if id == sp_consensus_aura::AURA_ENGINE_ID => {
            use sp_runtime::codec::Decode;
            sr25519::Signature::decode(&mut bytes.as_slice()).unwrap()
        }
        _ => panic!("missing Aura seal"),
    };
    assert!(sr25519::Pair::verify(
        &signature,
        header.hash().as_ref(),
        &pair.public()
    ));
    assert_eq!(
        crate::gpu_work_binding::validate_cpu_work(&header),
        Err(crate::gpu_work_binding::BindingError::WorkCommitmentMismatch)
    );
}

/// Real BasicQueue and EAGW verifier, with an in-memory mock importer and Aura
/// adapter using the SDK's actual signature/slot check. This is not node B.
#[test]
fn basic_queue_rejects_bad_work_after_real_aura_signature_check() {
    use crate::gpu_work_import::{AuraThenWork, DeterministicCpuWorkVerifier};
    use futures::{
        executor::block_on,
        future::{select, BoxFuture, Either},
    };
    use sc_consensus::{
        import_queue::Verifier, BasicQueue, BlockCheckParams, BlockImport, BlockImportParams,
        ImportResult,
    };
    use sp_runtime::generic::Digest;
    use std::{
        future::Future,
        pin::Pin,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    #[derive(Clone)]
    struct TestSpawner;
    impl sp_core::traits::SpawnEssentialNamed for TestSpawner {
        fn spawn_essential_blocking(
            &self,
            _: &'static str,
            _: Option<&'static str>,
            f: BoxFuture<'static, ()>,
        ) {
            std::thread::spawn(move || block_on(f));
        }
        fn spawn_essential(
            &self,
            name: &'static str,
            group: Option<&'static str>,
            f: BoxFuture<'static, ()>,
        ) {
            self.spawn_essential_blocking(name, group, f);
        }
    }

    struct Importer(Arc<AtomicUsize>);
    impl BlockImport<Block> for Importer {
        type Error = sp_consensus::Error;
        fn check_block<'a, 'b>(
            &'a self,
            _: BlockCheckParams<Block>,
        ) -> Pin<Box<dyn Future<Output = Result<ImportResult, Self::Error>> + Send + 'b>>
        where
            'a: 'b,
            Self: 'b,
        {
            Box::pin(async { Ok(ImportResult::imported(false)) })
        }
        fn import_block<'a, 'b>(
            &'a self,
            _: BlockImportParams<Block>,
        ) -> Pin<Box<dyn Future<Output = Result<ImportResult, Self::Error>> + Send + 'b>>
        where
            'a: 'b,
            Self: 'b,
        {
            Box::pin(async move {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok(ImportResult::imported(true))
            })
        }
    }

    struct AuraSignatureVerifier(sr25519::Public);
    impl Verifier<Block> for AuraSignatureVerifier {
        fn verify<'a, 'b>(
            &'a self,
            mut block: BlockImportParams<Block>,
        ) -> Pin<Box<dyn Future<Output = Result<BlockImportParams<Block>, String>> + Send + 'b>>
        where
            'a: 'b,
            Self: 'b,
        {
            Box::pin(async move {
                let (header, _, seal) = sc_consensus_aura::standalone::check_header_slot_and_seal::<Block, sr25519::Pair>(2u64.into(), block.header, &[self.0]).map_err(|e| format!("Aura invalid: {e:?}"))?;
                block.header = header;
                block.post_digests.push(seal);
                Ok(block)
            })
        }
    }

    let pair = sr25519::Pair::from_seed(&[8; 32]);
    let mut candidate = Header::new(
        42,
        H256::repeat_byte(1),
        H256::repeat_byte(2),
        H256::repeat_byte(3),
        Digest::default(),
    );
    candidate
        .digest_mut()
        .push(sc_consensus_aura::standalone::pre_digest::<sr25519::Pair>(
            2u64.into(),
        ));
    let incoming = invalid_work_signed_by_aura(&candidate, vec![], &pair).unwrap();
    let count = Arc::new(AtomicUsize::new(0));
    let verifier = AuraThenWork {
        aura: AuraSignatureVerifier(pair.public()),
        work: DeterministicCpuWorkVerifier,
    };
    let mut queue = BasicQueue::new(
        verifier,
        Box::new(Importer(count.clone())),
        None,
        &TestSpawner,
        None,
    );
    let snapshot = || {
        Ok(ChainSnapshot {
            best_hash: H256::zero(),
            best_number: 0,
            finalized_hash: H256::zero(),
            finalized_number: 0,
        })
    };
    let (tx, rx) = futures::channel::oneshot::channel();
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(5));
        let _ = tx.send(());
    });
    let outcome = block_on(async {
        select(
            Box::pin(assert_queue_rejects_invalid_work(
                &mut queue, incoming, snapshot,
            )),
            rx,
        )
        .await
    });
    match outcome {
        Either::Left((result, _)) => result.unwrap(),
        Either::Right(_) => panic!("queue test timed out"),
    }
    assert_eq!(count.load(Ordering::SeqCst), 0);
}

/// Explicit opt-in fixture generator. Only writes a new path supplied by the
/// caller and never opens a node database or starts a node/GPU process.
#[test]
#[ignore = "explicit output path and existing block-1 export required"]
fn export_bob_signed_invalid_work_block() {
    use jsonrpsee::core::__reexports::serde_json;
    use sp_runtime::{generic::SignedBlock, traits::Block as BlockT};
    use std::{fs::OpenOptions, io::Write, path::PathBuf, str::FromStr};

    let output = PathBuf::from(
        std::env::var_os("EAGW_NEGATIVE_EXPORT_PATH")
            .expect("EAGW_NEGATIVE_EXPORT_PATH is required"),
    );
    assert!(output.is_absolute(), "output path must be absolute");
    let input =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../.local/eagw-block1-latest.json");
    let original: SignedBlock<Block> =
        serde_json::from_reader(std::fs::File::open(input).expect("known export must exist"))
            .expect("standard SignedBlock JSON");
    let (original_header, body) = original.block.deconstruct();
    assert_eq!(*original_header.number(), 1);
    assert_eq!(
        *original_header.parent_hash(),
        H256::from_str("198c51667910b329ec74febcf86539a726b2c2a42eb6ea5a79fd29e3c910ed6d").unwrap()
    );
    assert_eq!(
        *original_header.state_root(),
        H256::from_str("3aeadfa976a4ace380abda091ac089b40775ff7ae4a13272ece64e065b5c0c5f").unwrap()
    );
    assert_eq!(
        *original_header.extrinsics_root(),
        H256::from_str("fb05d0a067cb38ca5146ce7a3ea4bfe0f545908b886a7e93962749c1240d35e2").unwrap()
    );
    assert_eq!(body.len(), 1);
    assert_eq!(
        serde_json::to_value(&body[0]).unwrap(),
        serde_json::json!("0x280501000b71815e99a101")
    );

    let slot = sc_consensus_aura::standalone::find_pre_digest::<Block, sr25519::Signature>(
        &original_header,
    )
    .unwrap();
    assert_eq!(
        *slot % 2,
        1,
        "the fixed local Alice/Bob ordering must select Bob"
    );
    let alice = sp_keyring::Sr25519Keyring::Alice.pair();
    let bob = sp_keyring::Sr25519Keyring::Bob.pair();
    let authorities = [alice.public(), bob.public()];
    let (mut candidate, checked_slot, _) =
        sc_consensus_aura::standalone::check_header_slot_and_seal::<Block, sr25519::Pair>(
            slot,
            original_header,
            &authorities,
        )
        .expect("original Aura signature and slot must validate");
    assert_eq!(checked_slot, slot);
    crate::gpu_work_binding::validate_cpu_work(&candidate).expect("original EAGW must be valid");
    let removed = candidate.digest_mut().logs.pop().expect("EAGW seal");
    assert!(matches!(removed, DigestItem::Seal(id, _) if id == crate::gpu_work_binding::ENGINE_ID));

    let incoming = invalid_work_signed_by_aura(&candidate, body, &bob).unwrap();
    let forged_hash = incoming.hash;
    let forged_header = incoming.header.unwrap();
    let (verified, _, _) = sc_consensus_aura::standalone::check_header_slot_and_seal::<
        Block,
        sr25519::Pair,
    >(slot, forged_header.clone(), &authorities)
    .expect("forged block must still pass Aura");
    assert_eq!(
        crate::gpu_work_binding::validate_cpu_work(&verified),
        Err(crate::gpu_work_binding::BindingError::WorkCommitmentMismatch)
    );
    let forged = SignedBlock {
        block: Block::new(forged_header, incoming.body.unwrap()),
        justifications: None,
    };
    let json = serde_json::to_vec(&forged).expect("serialize standard SignedBlock");
    let roundtrip: SignedBlock<Block> =
        serde_json::from_slice(&json).expect("roundtrip standard SignedBlock");
    assert_eq!(roundtrip.block.header().hash(), forged_hash);
    let mut file = OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(&output)
        .expect("output must not exist and parent must be writable");
    file.write_all(&json).unwrap();
    file.write_all(b"\n").unwrap();
    file.sync_all().unwrap();
    println!(
        "exported {:?} hash={forged_hash:?} slot={slot:?} (not imported)",
        output
    );
}
