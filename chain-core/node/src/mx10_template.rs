//! Read-only proposal RPC for the pinned, unfrozen MX10 isolated chain.
//! Registered only by new_full with its live client and explicit IPv4 loopback RPC.
//! No nonce search, GPU ownership, seal, import, transaction submission or persistence.
//! `mx10_getMiningTemplate` is unchanged (fixed AccountId([11;32]), timestamp-only body).
//! `mx10_getPaymentTemplate` is a separate isolated-only proposal for the
//! post-finality disbursement demo: beneficiary is the public dev-keyring
//! //Alice sr25519 account (a signable isolated test treasury; [11;32] has no
//! private key and can never sign a payment), and after the timestamp inherent
//! the body may carry ready, runtime-applied signed extrinsics from the node's
//! own transaction pool. It still never searches nonces, seals or imports.
use crate::mx10_context::Mx10ChainContext;
use eagle_runtime::{AccountId, Header, PowBeneficiaryV1, POW_ENGINE_ID};
use jsonrpsee::{types::ErrorObjectOwned, RpcModule};
use sp_api::{ApiExt, Core, ProvideRuntimeApi};
use sp_block_builder::BlockBuilder;
use sp_blockchain::HeaderBackend;
use sp_consensus_pow::TimestampApi;
use sp_inherents::InherentDataProvider;
use sp_runtime::{codec::Encode, generic::{Digest, DigestItem}, traits::Header as _};
use std::{io::Write, sync::{Arc, atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering}}, time::{Duration, Instant, SystemTime, UNIX_EPOCH}};
use sc_transaction_pool_api::{InPoolTransaction, TransactionPool};

/// The node's own transaction pool handle (same type new_partial builds).
pub(crate) type Pool = sc_transaction_pool::TransactionPoolHandle<eagle_runtime::opaque::Block, crate::service::FullClient>;
/// Upper bound of pool extrinsics considered for one isolated payment proposal.
const MAX_POOL_EXTRINSICS: usize = 16;

/// Isolated test treasury for the payment proposal: public dev key //Alice.
fn payment_treasury() -> AccountId { sp_keyring::Sr25519Keyring::Alice.to_account_id() }

/// Build immediately on the earliest runtime-valid timestamp.  The import
/// path still enforces pallet-timestamp's MinimumPeriod and maximum future
/// drift; this only removes idle time before miners may start searching.
fn next_template_timestamp(now: u64, parent: u64) -> u64 {
    now.max(parent.saturating_add(eagle_runtime::SLOT_DURATION / 2))
}

fn hex(bytes: &[u8]) -> String { bytes.iter().map(|b| format!("{b:02x}")).collect() }
fn error(message: impl std::fmt::Display) -> ErrorObjectOwned {
    ErrorObjectOwned::owned(-32080, message.to_string(), None::<()>)
}
struct BusyGuard(Arc<AtomicBool>);
impl Drop for BusyGuard { fn drop(&mut self) { self.0.store(false, Ordering::Release); } }

// Diagnostic candidate only. At most 64 phase lines and one terminal line per
// request. These records contain only fixed labels, IDs and clocks, never RPC
// parameters, Runtime state or error payloads. Logging failure must not affect
// proposal results. Direct stderr avoids the SDK's asynchronous debug buffer.
static NEXT_TRACE_REQUEST: AtomicU64 = AtomicU64::new(1);
const MAX_TRACE_PHASE_LINES: usize = 64;
struct ProposalTrace {
    request: Option<u64>,
    method: &'static str,
    started: Instant,
    lines: AtomicUsize,
    finished: bool,
    #[cfg(test)]
    records: Option<Arc<std::sync::Mutex<Vec<String>>>>,
}
impl ProposalTrace {
    fn new(method: &'static str) -> Self {
        let trace = Self {
            // Stop assigning IDs at exhaustion rather than wrap/reuse an ID.
            request: NEXT_TRACE_REQUEST.fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1)).ok(),
            method, started: Instant::now(), lines: AtomicUsize::new(0), finished: false,
            #[cfg(test)] records: None,
        };
        trace.emit("entry", "begin", false);
        trace
    }
    fn emit(&self, phase: &'static str, status: &'static str, terminal: bool) {
        let Some(request) = self.request else { return; };
        if !terminal && self.lines.fetch_update(Ordering::Relaxed, Ordering::Relaxed,
            |n| (n < MAX_TRACE_PHASE_LINES).then_some(n + 1)).is_err() { return; }
        let elapsed_ns = self.started.elapsed().as_nanos();
        let unix_ns = SystemTime::now().duration_since(UNIX_EPOCH).map(|t| t.as_nanos()).unwrap_or(0);
        let line = format!("MX10_RPC_PHASE pid={} request={request} method={} thread={:?} phase={phase} status={status} elapsed_ns={elapsed_ns} unix_ns={unix_ns}",
            std::process::id(), self.method, std::thread::current().id());
        #[cfg(test)]
        if let Some(records) = &self.records { records.lock().unwrap().push(line); return; }
        let _ = writeln!(std::io::stderr().lock(), "{line}");
    }
    fn measure<T, E>(&self, phase: &'static str, call: impl FnOnce() -> Result<T, E>) -> Result<T, E> {
        self.emit(phase, "begin", false);
        let result = call();
        self.emit(phase, if result.is_ok() { "ok" } else { "error" }, false);
        result
    }
    fn finish(&mut self, success: bool) {
        self.finished = true;
        self.emit("exit", if success { "ok" } else { "error" }, true);
    }
}
impl Drop for ProposalTrace {
    fn drop(&mut self) {
        if !self.finished { self.emit("exit", "cancelled_or_unwind", true); }
    }
}

pub(super) fn register(module: &mut RpcModule<()>, client: Arc<crate::service::FullClient>, pool: Arc<Pool>, context: Mx10ChainContext)
    -> Result<(), Box<dyn std::error::Error + Send + Sync>> {
    context.ensure_genesis(client.hash(0)?.ok_or("missing MX10 genesis")?)?;
    let busy = Arc::new(AtomicBool::new(false));
    let pay_client = client.clone(); let pay_busy = busy.clone();
    // Safe RPC: only transient RuntimeApi overlay execution; no state commit/import.
    module.register_async_method("mx10_getMiningTemplate", move |params, _, _| {
        let client = client.clone(); let busy = busy.clone();
        async move {
            let mut trace = ProposalTrace::new("mining");
            let result = async {
            let args: Vec<serde_json::Value> = params.parse()?;
            if !args.is_empty() { return Err(error("mx10_getMiningTemplate takes no parameters")); }
            if busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
                return Err(error("MX10 proposal already in progress"));
            }
            let _guard = BusyGuard(busy);
            trace.emit("busy_acquired", "ok", false);
            build(client, None, context, &trace).await
            }.await;
            trace.finish(result.is_ok());
            result
        }
    })?;
    module.register_async_method("mx10_getPaymentTemplate", move |params, _, _| {
        let client = pay_client.clone(); let busy = pay_busy.clone(); let pool = pool.clone();
        async move {
            let mut trace = ProposalTrace::new("payment");
            let result = async {
            let args: Vec<serde_json::Value> = params.parse()?;
            if !args.is_empty() { return Err(error("mx10_getPaymentTemplate takes no parameters")); }
            if busy.compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire).is_err() {
                return Err(error("MX10 proposal already in progress"));
            }
            let _guard = BusyGuard(busy);
            trace.emit("busy_acquired", "ok", false);
            build(client, Some(pool), context, &trace).await
            }.await;
            trace.finish(result.is_ok());
            result
        }
    })?;
    Ok(())
}

async fn build(client: Arc<crate::service::FullClient>, pool: Option<Arc<Pool>>, context: Mx10ChainContext, trace: &ProposalTrace) -> Result<serde_json::Value, ErrorObjectOwned> {
    let genesis = trace.measure("genesis", || client.hash(0)).map_err(error)?.ok_or_else(|| error("missing genesis"))?;
    context.ensure_genesis(genesis).map_err(error)?;
    let parent = client.info().best_hash;
    let parent_header = trace.measure("parent_header", || client.header(parent)).map_err(error)?.ok_or_else(|| error("missing best header"))?;
    let parent_time = trace.measure("timestamp", || client.runtime_api().timestamp(parent)).map_err(error)?;
    let wall_timestamp = SystemTime::now().duration_since(UNIX_EPOCH).map_err(error)?.as_millis() as u64;
    let timestamp = next_template_timestamp(wall_timestamp, parent_time);
    let number = parent_header.number().checked_add(1).ok_or_else(|| error("height overflow"))?;
    let beneficiary_account = if pool.is_some() { payment_treasury() } else { AccountId::new([11; 32]) };
    let beneficiary_bytes: [u8; 32] = beneficiary_account.clone().into();
    let beneficiary = PowBeneficiaryV1::new(beneficiary_account).encode_payload();
    let initial = Header::new(number, Default::default(), Default::default(), parent,
        Digest { logs: vec![DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary.clone())] });
    let mut inherent = sp_inherents::InherentData::new();
    trace.emit("inherent_provider", "begin", false);
    let provided = sp_timestamp::InherentDataProvider::new(timestamp.into()).provide_inherent_data(&mut inherent).await;
    trace.emit("inherent_provider", if provided.is_ok() { "ok" } else { "error" }, false);
    provided.map_err(error)?;
    // The same initialize -> timestamp inherent -> apply -> finalize sequence as
    // mx10_author::run BEFORE its nonce loop, on this FULL node's client/overlay.
    // Snapshot ready pool transactions BEFORE creating the (non-Send) runtime
    // API overlay; nothing is awaited while the overlay is alive.
    let mut ready_txs = Vec::new();
    if let Some(pool) = pool.as_ref() {
        trace.emit("pool_ready", "begin", false);
        let ready = pool.ready_at_with_timeout(parent, Duration::from_millis(1500)).await;
        for tx in ready.take(MAX_POOL_EXTRINSICS) { ready_txs.push(((**tx.data()).clone(), format!("{:?}", tx.hash()))); }
        trace.emit("pool_ready", "ok", false);
    }
    let mut api = client.runtime_api();
    api.set_call_context(sp_core::traits::CallContext::Onchain);
    trace.measure("initialize", || api.initialize_block(parent, &initial)).map_err(error)?;
    let mut body = trace.measure("inherent_extrinsics", || api.inherent_extrinsics(parent, inherent)).map_err(error)?;
    for xt in &body {
        let applied = trace.measure("apply_inherent", || api.apply_extrinsic(parent, xt.clone())).map_err(error)?;
        if !matches!(applied, Ok(Ok(()))) { return Err(error(format!("MX10 timestamp apply failed: {applied:?}"))); }
    }
    let inherent_count = body.len();
    let mut pool_included = Vec::new(); let mut pool_skipped = Vec::new();
    {
        // Same rule as sc-basic-authorship: a validity error leaves the overlay
        // untouched and is skipped; any Ok(_) result (dispatch success or a
        // dispatch error that still paid fees) mutated state and must be included.
        for (xt, hash) in ready_txs {
            match trace.measure("apply_pool", || api.apply_extrinsic(parent, xt.clone())).map_err(error)? {
                Ok(outcome) => {
                    pool_included.push(serde_json::json!({"hash": hash, "dispatch": format!("{outcome:?}"), "dispatch_ok": outcome.is_ok()}));
                    body.push(xt);
                },
                Err(validity) => pool_skipped.push(serde_json::json!({"hash": hash, "validity_error": format!("{validity:?}")})),
            }
        }
    }
    let header = trace.measure("finalize", || api.finalize_block(parent)).map_err(error)?;
    trace.emit("overlay_drop", "begin", false);
    drop(api); // discard overlay, do not commit or import the proposal
    trace.emit("overlay_drop", "ok", false);
    if header.parent_hash() != &parent || *header.number() != number ||
       header.digest().logs != vec![DigestItem::PreRuntime(POW_ENGINE_ID, beneficiary)] {
        return Err(error("runtime returned unexpected unsealed MX10 header"));
    }
    let difficulty = trace.measure("difficulty", || context.difficulty(client.clone(), parent)).map_err(error)?;
    let target = crate::gpu_pow_consensus::target_for_difficulty(difficulty).map_err(error)?;
    let pre_hash = header.hash();
    let challenge = crate::gpu_pow_mx10::challenge_from_pre_hash(genesis, pre_hash);
    if client.info().best_hash != parent { return Err(error("best parent changed during proposal; retry")); }
    eprintln!("MX10_FRESH_TEMPLATE full_client=true height={number} parent={parent:?} pre_hash={pre_hash:?} runtime=initialize/inherent_extrinsics/apply/finalize nonce_search=false import=false payment_template={} pool_included={} pool_skipped={}", pool.is_some(), pool_included.len(), pool_skipped.len());
    trace.emit("json_value", "begin", false);
    let value = serde_json::json!({
        "schema": "SVY-MX10-FRESH-TEMPLATE-1", "genesis": format!("{genesis:?}"),
        "workAlgorithm": context.work_algorithm(),
        "parent": format!("{parent:?}"), "parent_hash": hex(parent.as_bytes()), "height": number,
        "pre_hash": hex(pre_hash.as_bytes()), "challenge": hex(&challenge),
        "difficulty": difficulty, "target": format!("0x{}", hex(&target)), "chain_target": hex(&target),
        "version": context.version(), "n": context.shape().n(), "inner": context.shape().inner_dim(), "certificate_bytes": context.certificate_bytes(),
        "timestamp_ms": timestamp, "header_scale": format!("0x{}", hex(&header.encode())),
        "unsealed_header": header, "extrinsics": body.iter().map(|xt| format!("0x{}", hex(&xt.encode()))).collect::<Vec<_>>(),
        "beneficiary_account_hex": hex(&beneficiary_bytes), "payment_template": pool.is_some(),
        "inherent_extrinsics": inherent_count, "pool_included": pool_included, "pool_skipped": pool_skipped, "sealed": false, "imported": false, "nonce_search": false,
        "pool_mapping": "genesis/workAlgorithm/parent_hash/pre_hash/challenge/chain_target map directly; coordinator adds issued/expires/workers and selects only exact Mx10TemplateShares.issue fields"
    });
    trace.emit("json_value", "ok", false);
    Ok(value)
}

#[cfg(test)]
mod trace_tests {
    use super::*;
    fn captured() -> (ProposalTrace, Arc<std::sync::Mutex<Vec<String>>>) {
        let records = Arc::new(std::sync::Mutex::new(Vec::new()));
        let trace = ProposalTrace { request: Some(42), method: "mining", started: Instant::now(),
            lines: AtomicUsize::new(0), finished: false, records: Some(records.clone()) };
        (trace, records)
    }
    #[test]
    fn next_template_is_immediate_and_runtime_minimum_period_valid() {
        let minimum = eagle_runtime::SLOT_DURATION / 2;
        assert_eq!(next_template_timestamp(20_000, 10_000), 20_000);
        assert_eq!(next_template_timestamp(12_000, 10_000), 10_000 + minimum);
        assert_eq!(next_template_timestamp(0, u64::MAX), u64::MAX);
    }
    #[test]
    fn calls_once_and_preserves_success_and_opaque_error() {
        let (mut trace, records) = captured();
        let mut calls = 0;
        let value = trace.measure("initialize", || { calls += 1; Ok::<_, ()>(vec![1, 2, 3]) });
        assert_eq!(value, Ok(vec![1, 2, 3]));
        struct OpaqueError(&'static str); // deliberately has no Display/Debug
        let failure = trace.measure("timestamp", || { calls += 1; Err::<(), _>(OpaqueError("secret-must-not-be-formatted")) });
        assert_eq!(failure.err().unwrap().0, "secret-must-not-be-formatted");
        assert_eq!(calls, 2);
        trace.finish(false);
        drop(trace);
        let lines = records.lock().unwrap();
        assert_eq!(lines.len(), 5);
        assert!(lines.iter().any(|s| s.contains("phase=timestamp status=error")));
        assert_eq!(lines.iter().filter(|s| s.contains("phase=exit")).count(), 1);
        assert!(lines.iter().all(|s| !s.contains("secret-must-not-be-formatted")));
    }
    #[test]
    fn line_cap_keeps_terminal_and_clocks_are_monotonic() {
        let (mut trace, records) = captured();
        for _ in 0..100 { trace.emit("apply_inherent", "ok", false); }
        trace.finish(true);
        drop(trace);
        let lines = records.lock().unwrap();
        assert_eq!(lines.len(), MAX_TRACE_PHASE_LINES + 1);
        let mut last = 0;
        for line in lines.iter() {
            assert!(line.contains("pid=") && line.contains("request=42") && line.contains("thread=ThreadId("));
            let elapsed: u128 = line.split("elapsed_ns=").nth(1).unwrap().split_whitespace().next().unwrap().parse().unwrap();
            assert!(elapsed >= last); last = elapsed;
        }
        assert!(lines.last().unwrap().contains("phase=exit status=ok"));
    }
    #[test]
    fn unfinished_scope_and_unwind_emit_terminal() {
        let (trace, records) = captured();
        drop(trace);
        assert!(records.lock().unwrap()[0].contains("status=cancelled_or_unwind"));
        let (trace, records) = captured();
        let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(move || {
            let _owned = trace; panic!("test-only unwind");
        }));
        assert!(result.is_err());
        assert_eq!(records.lock().unwrap().len(), 1);
    }
    #[test]
    fn live_ids_are_unique_and_do_not_control_busy_state() {
        let one = ProposalTrace::new("mining");
        let two = ProposalTrace::new("payment");
        assert_ne!(one.request, two.request);
        let busy = Arc::new(AtomicBool::new(true));
        drop(one); drop(two);
        assert!(busy.load(Ordering::Acquire));
        drop(BusyGuard(busy.clone()));
        assert!(!busy.load(Ordering::Acquire));
    }
}
