//! S4 mock tests: V1 vector, aligned S2 cases, T-B TB01-TB07.
use crate::{mock::*, pallet::*};
use codec::Encode;
use frame_support::{assert_noop, assert_ok};
use sp_runtime::DispatchError;

const REQ: [u8; 32] = [0x55; 32];
const RESULT: [u8; 32] = [0x77; 32];
const EVIDENCE: [u8; 32] = [0x88; 32];
const RECEIPT: [u8; 32] = [0x66; 32];

fn genesis() -> [u8; 32] {
    AiReserve::genesis_hash()
}

fn rate_id() -> [u8; 32] {
    v1_rate().rate_id()
}

fn terms_with(req: [u8; 32], worker: u64, expires_at: u64) -> TermsOf<Test> {
    AiJobTerms {
        genesis: genesis(),
        request_commitment: req,
        worker,
        beneficiary: BENEF,
        rate_id: rate_id(),
        expires_at,
    }
}

fn terms() -> TermsOf<Test> {
    terms_with(REQ, WORKER, 110)
}

fn receipt_for(t: &TermsOf<Test>, id: [u8; 32], input: u32, cached: u32, output: u32) -> UsageReceipt {
    UsageReceipt {
        receipt_id: id,
        terms_digest: AiReserve::terms_digest(t),
        result_digest: RESULT,
        execution_evidence_digest: EVIDENCE,
        input_tokens: input,
        cached_tokens: cached,
        output_tokens: output,
    }
}

fn receipt(input: u32, cached: u32, output: u32) -> UsageReceipt {
    receipt_for(&terms(), RECEIPT, input, cached, output)
}

fn signed(who: u64) -> RuntimeOrigin {
    RuntimeOrigin::signed(who)
}

fn reserve_default() -> u64 {
    let id = NextAiJobId::<Test>::get();
    assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms()));
    id
}

fn commit(job: u64) {
    assert_ok!(AiReserve::commit_ai_result(signed(WORKER), job, RESULT));
}

fn reserve_free() -> u128 {
    Balances::free_balance(RESERVE)
}

fn reserve_reserved() -> u128 {
    Balances::reserved_balance(RESERVE)
}

fn state(job: u64) -> AiJobState {
    AiJobs::<Test>::get(job).unwrap().state
}

fn totals() -> (u128, u128, u128) {
    let t = AiReserveTotals::<Test>::get();
    (t.reserved, t.paid, t.refunded)
}

/// Reserve funds (free + reserved) plus everything paid out equals the starting reserve.
fn assert_conserved() {
    let (_, paid, _) = totals();
    assert_eq!(reserve_free() + reserve_reserved() + paid, RESERVE_START);
    assert_eq!(Balances::free_balance(BENEF), BENEF_START + paid);
    assert_eq!(Balances::total_issuance(), RESERVE_START + BENEF_START);
}

fn ai_events() -> Vec<Event<Test>> {
    System::events()
        .into_iter()
        .filter_map(|r| match r.event {
            RuntimeEvent::AiReserve(e) => Some(e),
            _ => None,
        })
        .collect()
}

// ---------------------------------------------------------------- quote / rate id

fn q(r: &TokenRate, input: u32, cached: u32, output: u32) -> Result<u128, DispatchError> {
    AiReserve::quote(r, input, cached, output).map_err(Into::into)
}

#[test]
fn quote_v1_vectors_300_1408_and_rejections() {
    let r = v1_rate();
    assert_eq!(q(&r, 132, 0, 84), Ok(300));
    assert_eq!(q(&r, 1024, 0, 192), Ok(1408));
    assert_eq!(1408 - 300, 1108);
    assert_eq!(q(&r, 0, 0, 0), Ok(0));
    assert_eq!(q(&r, 1, 2, 0), Err(Error::<Test>::CachedExceedsInput.into()));
    assert_eq!(q(&r, 10, 1, 0), Err(Error::<Test>::CachedNotAllowed.into()));
    assert_eq!(q(&r, 1025, 0, 0), Err(Error::<Test>::UsageAboveCap.into()));
    assert_eq!(q(&r, 0, 0, 193), Err(Error::<Test>::UsageAboveCap.into()));
    let mut cached_rate = v1_rate();
    cached_rate.cached_per_million = 1;
    assert_eq!(q(&cached_rate, 10, 0, 0), Err(Error::<Test>::CachedNotAllowed.into()));
    let mut huge = v1_rate();
    huge.input_per_million = u128::MAX;
    assert_eq!(q(&huge, 2, 0, 0), Err(Error::<Test>::Overflow.into()));
    // floor once per job: 1 input token at 1.5 atoms/token plus 1 output token at 1.5 = 3 (not 1+1)
    let mut half = v1_rate();
    half.input_per_million = 1_500_000;
    half.output_per_million = 1_500_000;
    assert_eq!(q(&half, 1, 0, 1), Ok(3));
}

#[test]
fn rate_id_is_blake2_of_scale_rate_without_version() {
    ExtBuilder::default().build().execute_with(|| {
        let r = v1_rate();
        assert_eq!(r.encode().len(), 32 * 3 + 16 * 3 + 4 * 2);
        assert_eq!(r.rate_id(), sp_io::hashing::blake2_256(&r.encode()));
        let entry = AiRates::<Test>::get(r.rate_id()).expect("registered at genesis");
        assert_eq!(entry.version, 1);
        // A different registry version would not change the key: version is not hashed.
        let other = RateEntry { rate: r.clone(), version: 7 };
        assert_eq!(other.rate.rate_id(), r.rate_id());
    });
}

// ---------------------------------------------------------------- V1 happy path

#[test]
fn v1_132_0_84_pays_300_reserves_1408_refunds_1108() {
    ExtBuilder::default().build().execute_with(|| {
        let issuance = Balances::total_issuance();
        let job = reserve_default();
        assert_eq!(job, 0);
        assert_eq!(NextAiJobId::<Test>::get(), 1);
        assert_eq!(reserve_reserved(), 1408);
        assert_eq!(reserve_free(), RESERVE_START - 1408);
        assert_eq!(totals(), (1408, 0, 0));
        assert_eq!(OpenJobOf::<Test>::get(WORKER), Some(0));
        assert_eq!(AiRequestJobs::<Test>::get(REQ), Some(0));
        commit(job);
        assert_eq!(state(job), AiJobState::Committed);
        System::set_block_number(105);
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)));
        let j = AiJobs::<Test>::get(job).unwrap();
        assert_eq!((j.state, j.paid, j.max_amount), (AiJobState::Settled, 300, 1408));
        assert_eq!(Balances::free_balance(BENEF), BENEF_START + 300);
        assert_eq!(reserve_reserved(), 0);
        assert_eq!(reserve_free(), RESERVE_START - 300);
        assert_eq!(totals(), (0, 300, 1108));
        assert_eq!(Balances::total_issuance(), issuance);
        assert_eq!(UsedReceipts::<Test>::get(RECEIPT), Some(job));
        assert_eq!(OpenJobOf::<Test>::get(WORKER), None);
        assert_conserved();
        assert_eq!(
            ai_events(),
            vec![
                Event::AiJobReserved {
                    job_id: 0,
                    worker: WORKER,
                    beneficiary: BENEF,
                    rate_id: rate_id(),
                    max_amount: 1408
                },
                Event::AiResultCommitted { job_id: 0, result_digest: RESULT },
                Event::AiJobSettled {
                    job_id: 0,
                    beneficiary: BENEF,
                    paid: 300,
                    refunded: 1108,
                    receipt_id: RECEIPT
                },
            ]
        );
    });
}

#[test]
fn zero_usage_is_terminal_and_consumes_receipt() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        commit(job);
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job, receipt(0, 0, 0)));
        let j = AiJobs::<Test>::get(job).unwrap();
        assert_eq!((j.state, j.paid), (AiJobState::Settled, 0));
        assert_eq!(UsedReceipts::<Test>::get(RECEIPT), Some(job));
        assert_eq!(totals(), (0, 0, 1408));
        assert_eq!(reserve_free(), RESERVE_START);
        assert_eq!(Balances::free_balance(BENEF), BENEF_START);
        assert_eq!(OpenJobOf::<Test>::get(WORKER), None);
        assert_conserved();
    });
}

// ---------------------------------------------------------------- replay / conflicts

#[test]
fn replays_are_recovered_without_new_terminal_state_or_events() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        commit(job);
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)));
        let events = System::events().len();
        let snapshot = (reserve_free(), reserve_reserved(), Balances::free_balance(BENEF), totals());
        let stored = AiJobs::<Test>::get(job);
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms()));
        assert_ok!(AiReserve::commit_ai_result(signed(WORKER), job, RESULT));
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)));
        assert_eq!(System::events().len(), events);
        assert_eq!(NextAiJobId::<Test>::get(), 1);
        assert_eq!(AiJobs::<Test>::get(job), stored);
        assert_eq!(
            (reserve_free(), reserve_reserved(), Balances::free_balance(BENEF), totals()),
            snapshot
        );
        // Settled job with a different receipt is a conflict, not a recovery.
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 85)),
            Error::<Test>::AlreadyTerminal
        );
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt_for(&terms(), [0x67; 32], 132, 0, 84)),
            Error::<Test>::AlreadyTerminal
        );
    });
}

#[test]
fn request_already_bound_is_permanent() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with(REQ, WORKER, 111)),
            Error::<Test>::RequestAlreadyBound
        );
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with(REQ, WORKER2, 110)),
            Error::<Test>::RequestAlreadyBound
        );
        assert_ok!(AiReserve::cancel_uncommitted_ai_job(signed(SCHED), job));
        // still bound after the job is terminal: a redispatch needs a new commitment
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with(REQ, WORKER, 111)),
            Error::<Test>::RequestAlreadyBound
        );
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms_with([0x56; 32], WORKER, 111)));
        assert_eq!(NextAiJobId::<Test>::get(), 2);
    });
}

#[test]
fn receipt_already_used_on_another_job() {
    ExtBuilder::default().build().execute_with(|| {
        let job0 = reserve_default();
        commit(job0);
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job0, receipt(132, 0, 84)));
        let t2 = terms_with([0xaa; 32], WORKER, 140);
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), t2.clone()));
        assert_ok!(AiReserve::commit_ai_result(signed(WORKER), 1, RESULT));
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), 1, receipt_for(&t2, RECEIPT, 132, 0, 84)),
            Error::<Test>::ReceiptAlreadyUsed
        );
        assert_eq!(state(1), AiJobState::Committed);
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), 1, receipt_for(&t2, [0x99; 32], 1, 0, 1)));
        assert_eq!(AiJobs::<Test>::get(1).unwrap().paid, 3);
    });
}

#[test]
fn worker_busy_until_terminal() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with([0xab; 32], WORKER, 120)),
            Error::<Test>::WorkerBusy
        );
        commit(job);
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with([0xab; 32], WORKER, 120)),
            Error::<Test>::WorkerBusy
        );
        // a different worker is fine
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms_with([0xac; 32], WORKER2, 120)));
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)));
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms_with([0xab; 32], WORKER, 120)));
    });
}

#[test]
fn unknown_rate() {
    ExtBuilder::default().build().execute_with(|| {
        let mut t = terms();
        t.rate_id = [0x99; 32];
        assert_noop!(AiReserve::reserve_ai_job(signed(SCHED), t), Error::<Test>::UnknownRate);
        // a version-tagged hash of the same rate is not a valid id
        let mut t = terms();
        t.rate_id = sp_io::hashing::blake2_256(&(v1_rate(), 1u16).encode());
        assert_noop!(AiReserve::reserve_ai_job(signed(SCHED), t), Error::<Test>::UnknownRate);
    });
}

#[test]
fn cached_not_allowed_and_cached_exceeds_input_at_settle() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        commit(job);
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 1, 84)),
            Error::<Test>::CachedNotAllowed
        );
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(1, 2, 84)),
            Error::<Test>::CachedExceedsInput
        );
        assert_eq!(state(job), AiJobState::Committed);
        assert!(UsedReceipts::<Test>::get(RECEIPT).is_none());
    });
}

#[test]
#[should_panic(expected = "v1 forbids cached billing")]
fn genesis_rejects_cached_rate() {
    let mut r = v1_rate();
    r.cached_per_million = 1;
    ExtBuilder { rates: vec![RateEntry { rate: r, version: 1 }], ..Default::default() }.storage();
}

#[test]
#[should_panic(expected = "duplicate rate")]
fn genesis_rejects_duplicate_rate_even_with_new_version() {
    ExtBuilder {
        rates: vec![
            RateEntry { rate: v1_rate(), version: 1 },
            RateEntry { rate: v1_rate(), version: 2 },
        ],
        ..Default::default()
    }
    .storage();
}

#[test]
fn usage_above_cap_at_settle() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        commit(job);
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(1025, 0, 84)),
            Error::<Test>::UsageAboveCap
        );
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 193)),
            Error::<Test>::UsageAboveCap
        );
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job, receipt(1024, 0, 192)));
        assert_eq!(AiJobs::<Test>::get(job).unwrap().paid, 1408);
        assert_eq!(totals(), (0, 1408, 0));
    });
}

#[test]
fn wrong_genesis() {
    ExtBuilder::default().build().execute_with(|| {
        assert_ne!(genesis(), [0u8; 32]);
        let mut t = terms();
        t.genesis = [0x11; 32];
        assert_noop!(AiReserve::reserve_ai_job(signed(SCHED), t), Error::<Test>::WrongGenesis);
        let mut t = terms();
        t.genesis = [0u8; 32];
        assert_noop!(AiReserve::reserve_ai_job(signed(SCHED), t), Error::<Test>::WrongGenesis);
    });
}

#[test]
fn invalid_beneficiary_worker_and_role_overlap() {
    ExtBuilder::default().build().execute_with(|| {
        let mut t = terms();
        t.beneficiary = RESERVE;
        assert_noop!(AiReserve::reserve_ai_job(signed(SCHED), t), Error::<Test>::InvalidBeneficiary);
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with(REQ, RESERVE, 110)),
            Error::<Test>::InvalidWorker
        );
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with(REQ, SCHED, 110)),
            Error::<Test>::RoleOverlap
        );
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with(REQ, ATT, 110)),
            Error::<Test>::RoleOverlap
        );
    });
}

#[test]
fn genesis_roles_are_sorted_disjoint_and_exclude_reserve() {
    ExtBuilder { schedulers: vec![12, 11], attestors: vec![22, 21], ..Default::default() }
        .build()
        .execute_with(|| {
            assert_eq!(Schedulers::<Test>::get().into_inner(), vec![11, 12]);
            assert_eq!(Attestors::<Test>::get().into_inner(), vec![21, 22]);
            let s: std::collections::BTreeSet<u64> = Schedulers::<Test>::get().into_iter().collect();
            let a: std::collections::BTreeSet<u64> = Attestors::<Test>::get().into_iter().collect();
            assert!(s.is_disjoint(&a));
            assert!(!s.contains(&RESERVE) && !a.contains(&RESERVE));
            assert_ok!(AiReserve::reserve_ai_job(signed(12), terms()));
        });
}

#[test]
#[should_panic(expected = "must not overlap")]
fn genesis_rejects_scheduler_attestor_overlap() {
    ExtBuilder { schedulers: vec![1, 7], attestors: vec![7], ..Default::default() }.storage();
}

#[test]
#[should_panic(expected = "scheduler set must not contain RewardAccount")]
fn genesis_rejects_reserve_as_scheduler() {
    ExtBuilder { schedulers: vec![RESERVE], ..Default::default() }.storage();
}

#[test]
#[should_panic(expected = "attestor set must not contain RewardAccount")]
fn genesis_rejects_reserve_as_attestor() {
    ExtBuilder { attestors: vec![RESERVE], ..Default::default() }.storage();
}

#[test]
#[should_panic(expected = "too many scheduler entries")]
fn genesis_rejects_more_than_four_schedulers() {
    ExtBuilder { schedulers: vec![11, 12, 13, 14, 15], ..Default::default() }.storage();
}

#[test]
fn origins_are_enforced() {
    ExtBuilder::default().build().execute_with(|| {
        assert_noop!(AiReserve::reserve_ai_job(signed(ATT), terms()), DispatchError::BadOrigin);
        assert_noop!(AiReserve::reserve_ai_job(signed(ANYONE), terms()), DispatchError::BadOrigin);
        assert_noop!(
            AiReserve::reserve_ai_job(RuntimeOrigin::root(), terms()),
            DispatchError::BadOrigin
        );
        let job = reserve_default();
        assert_noop!(
            AiReserve::commit_ai_result(signed(SCHED), job, RESULT),
            Error::<Test>::NotWorker
        );
        commit(job);
        assert_noop!(
            AiReserve::settle_ai_job(signed(SCHED), job, receipt(132, 0, 84)),
            DispatchError::BadOrigin
        );
        assert_noop!(
            AiReserve::settle_ai_job(signed(WORKER), job, receipt(132, 0, 84)),
            DispatchError::BadOrigin
        );
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(RuntimeOrigin::root(), job),
            DispatchError::BadOrigin
        );
    });
}

#[test]
fn lease_bounds_and_commit_deadline() {
    ExtBuilder::default().build().execute_with(|| {
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with(REQ, WORKER, 100)),
            Error::<Test>::Expired
        );
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms_with(REQ, WORKER, 100 + MAX_LEASE + 1)),
            Error::<Test>::LeaseTooLong
        );
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms_with(REQ, WORKER, 100 + MAX_LEASE)));
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms_with([0x57; 32], WORKER2, 110)));
        System::set_block_number(111);
        assert_noop!(AiReserve::commit_ai_result(signed(WORKER2), 1, RESULT), Error::<Test>::Expired);
        System::set_block_number(110);
        assert_ok!(AiReserve::commit_ai_result(signed(WORKER2), 1, RESULT)); // now == expires_at allowed
    });
}

#[test]
fn reserve_keeps_existential_deposit() {
    ExtBuilder { reserve_balance: 1408 + ED - 1, ..Default::default() }.build().execute_with(|| {
        assert_noop!(
            AiReserve::reserve_ai_job(signed(SCHED), terms()),
            Error::<Test>::InsufficientReserve
        );
    });
    ExtBuilder { reserve_balance: 1408 + ED, ..Default::default() }.build().execute_with(|| {
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms()));
        assert_eq!(reserve_free(), ED);
    });
}

#[test]
fn settlement_bindings_terms_result_and_state() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)),
            Error::<Test>::NotCommitted
        );
        commit(job);
        let mut r = receipt(132, 0, 84);
        r.terms_digest = [0x12; 32];
        assert_noop!(AiReserve::settle_ai_job(signed(ATT), job, r), Error::<Test>::TermsMismatch);
        let mut r = receipt(132, 0, 84);
        r.result_digest = [0x13; 32];
        assert_noop!(AiReserve::settle_ai_job(signed(ATT), job, r), Error::<Test>::ResultMismatch);
        let mut r = receipt(132, 0, 84);
        r.receipt_id = [0u8; 32];
        assert_noop!(AiReserve::settle_ai_job(signed(ATT), job, r), Error::<Test>::InvalidDigest);
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), 42, receipt(132, 0, 84)),
            Error::<Test>::UnknownJob
        );
        assert_eq!(state(job), AiJobState::Committed);
        // settle after expires_at is allowed while inside the settle window
        System::set_block_number(115);
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)));
    });
}

#[test]
fn commit_conflicts_and_zero_digest() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        assert_noop!(
            AiReserve::commit_ai_result(signed(WORKER), job, [0u8; 32]),
            Error::<Test>::InvalidDigest
        );
        commit(job);
        assert_noop!(
            AiReserve::commit_ai_result(signed(WORKER), job, [0x78; 32]),
            Error::<Test>::ResultMismatch
        );
        assert_noop!(
            AiReserve::commit_ai_result(signed(WORKER), 9, RESULT),
            Error::<Test>::UnknownJob
        );
    });
}

#[test]
fn cancel_rules() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        assert_noop!(
            AiReserve::cancel_uncommitted_ai_job(signed(ANYONE), job),
            DispatchError::BadOrigin
        );
        assert_ok!(AiReserve::cancel_uncommitted_ai_job(signed(SCHED), job));
        assert_eq!(state(job), AiJobState::Cancelled);
        assert_eq!(totals(), (0, 0, 1408));
        assert_eq!(reserve_free(), RESERVE_START);
        let events = System::events().len();
        assert_ok!(AiReserve::cancel_uncommitted_ai_job(signed(SCHED), job)); // recovered
        assert_eq!(System::events().len(), events);
        assert_noop!(
            AiReserve::commit_ai_result(signed(WORKER), job, RESULT),
            Error::<Test>::AlreadyTerminal
        );
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)),
            Error::<Test>::AlreadyTerminal
        );
        // anyone may cancel an uncommitted job once now > expires_at (aligned S2 rule)
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms_with([0x58; 32], WORKER, 120)));
        System::set_block_number(120);
        assert_noop!(
            AiReserve::cancel_uncommitted_ai_job(signed(ANYONE), 1),
            DispatchError::BadOrigin
        );
        System::set_block_number(121);
        assert_ok!(AiReserve::cancel_uncommitted_ai_job(signed(ANYONE), 1));
        assert_eq!(state(1), AiJobState::Cancelled);
        // committed jobs cannot be cancelled
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), terms_with([0x59; 32], WORKER, 150)));
        assert_ok!(AiReserve::commit_ai_result(signed(WORKER), 2, RESULT));
        assert_noop!(
            AiReserve::cancel_uncommitted_ai_job(signed(SCHED), 2),
            Error::<Test>::NotReserved
        );
        assert_ok!(AiReserve::settle_ai_job(
            signed(ATT),
            2,
            receipt_for(&terms_with([0x59; 32], WORKER, 150), RECEIPT, 132, 0, 84)
        ));
        assert_noop!(
            AiReserve::cancel_uncommitted_ai_job(signed(SCHED), 2),
            Error::<Test>::AlreadyTerminal
        );
        assert_conserved();
    });
}

#[test]
fn payment_to_nonexistent_beneficiary_fails_atomically() {
    // Same order as pay_verified_result: repatriate_reserved(.., Free) to an account that does
    // not exist fails in pallet-balances with DeadAccount. The whole call is rolled back.
    ExtBuilder::default().build().execute_with(|| {
        const FRESH: u64 = 77;
        let mut t = terms();
        t.beneficiary = FRESH;
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), t.clone()));
        assert_ok!(AiReserve::commit_ai_result(signed(WORKER), 0, RESULT));
        for (input, output) in [(132u32, 84u32), (3, 0)] {
            assert_noop!(
                AiReserve::settle_ai_job(signed(ATT), 0, receipt_for(&t, RECEIPT, input, 0, output)),
                pallet_balances::Error::<Test>::DeadAccount
            );
        }
        assert_eq!(state(0), AiJobState::Committed);
        assert_eq!(reserve_reserved(), 1408);
        assert!(UsedReceipts::<Test>::get(RECEIPT).is_none());
        assert_eq!(Balances::free_balance(FRESH), 0);
        // zero usage needs no transfer, so it still terminates
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), 0, receipt_for(&t, RECEIPT, 0, 0, 0)));
        assert_eq!(state(0), AiJobState::Settled);
        assert_conserved();
    });
}

#[test]
fn small_payment_below_ed_to_existing_beneficiary_is_delivered() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        commit(job);
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job, receipt(3, 0, 0)));
        assert_eq!(AiJobs::<Test>::get(job).unwrap().paid, 3);
        assert_conserved();
    });
}

#[test]
fn call_indices_are_12_to_16_and_job_ids_independent() {
    ExtBuilder::default().build().execute_with(|| {
        let calls: Vec<(u8, Call<Test>)> = vec![
            (12, Call::reserve_ai_job { terms: terms() }),
            (13, Call::commit_ai_result { job_id: 0, result_digest: RESULT }),
            (14, Call::settle_ai_job { job_id: 0, receipt: receipt(1, 0, 1) }),
            (15, Call::cancel_uncommitted_ai_job { job_id: 0 }),
            (16, Call::refund_timed_out_ai_job { job_id: 0 }),
        ];
        for (index, call) in calls {
            assert_eq!(call.encode()[0], index);
        }
        assert_eq!(NextAiJobId::<Test>::get(), 0);
    });
}

// ---------------------------------------------------------------- T-B (TB01-TB07)

/// Reserve + commit at block 100; returns the commit block.
fn committed_job() -> (u64, u64) {
    let job = reserve_default();
    commit(job);
    (job, AiJobs::<Test>::get(job).unwrap().commit_block.unwrap())
}

#[test]
fn tb01_settle_after_window_is_settle_window_closed_and_job_stays_committed() {
    ExtBuilder::default().build().execute_with(|| {
        let (job, at) = committed_job();
        System::set_block_number(at + SETTLE_WINDOW);
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)),
            Error::<Test>::SettleWindowClosed
        );
        System::set_block_number(at + SETTLE_WINDOW + 30);
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)),
            Error::<Test>::SettleWindowClosed
        );
        assert_eq!(state(job), AiJobState::Committed);
        assert_eq!(reserve_reserved(), 1408);
    });
}

#[test]
fn tb01b_last_block_inside_window_still_settles() {
    ExtBuilder::default().build().execute_with(|| {
        let (job, at) = committed_job();
        System::set_block_number(at + SETTLE_WINDOW - 1);
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)));
        assert_eq!(AiJobs::<Test>::get(job).unwrap().paid, 300);
    });
}

#[test]
fn tb02_anyone_triggers_full_refund_to_reserve() {
    ExtBuilder::default().build().execute_with(|| {
        let issuance = Balances::total_issuance();
        let (job, at) = committed_job();
        System::set_block_number(at + SETTLE_WINDOW);
        assert_ok!(AiReserve::refund_timed_out_ai_job(signed(ANYONE), job));
        let j = AiJobs::<Test>::get(job).unwrap();
        assert_eq!((j.state, j.paid), (AiJobState::Refunded, 0));
        assert_eq!(reserve_free(), RESERVE_START);
        assert_eq!(reserve_reserved(), 0);
        assert_eq!(totals(), (0, 0, 1408));
        assert_eq!(Balances::free_balance(BENEF), BENEF_START);
        assert_eq!(Balances::total_issuance(), issuance);
        assert_eq!(OpenJobOf::<Test>::get(WORKER), None);
        assert_eq!(
            ai_events().last(),
            Some(&Event::AiJobRefunded { job_id: job, refunded: 1408 })
        );
        assert_conserved();
    });
}

#[test]
fn tb03_no_disputed_state_and_everything_after_refund_is_terminal() {
    ExtBuilder::default().build().execute_with(|| {
        // The state enum has exactly these variants; there is no Disputed (compile-time check).
        let all = [
            AiJobState::Reserved,
            AiJobState::Committed,
            AiJobState::Settled,
            AiJobState::Cancelled,
            AiJobState::Refunded,
        ];
        for s in all {
            match s {
                AiJobState::Reserved
                | AiJobState::Committed
                | AiJobState::Settled
                | AiJobState::Cancelled
                | AiJobState::Refunded => {}
            }
        }
        let (job, at) = committed_job();
        System::set_block_number(at + SETTLE_WINDOW);
        assert_ok!(AiReserve::refund_timed_out_ai_job(signed(ATT), job));
        assert_eq!(state(job), AiJobState::Refunded);
        assert_noop!(
            AiReserve::cancel_uncommitted_ai_job(signed(SCHED), job),
            Error::<Test>::AlreadyTerminal
        );
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(signed(WORKER), job),
            Error::<Test>::AlreadyTerminal
        );
    });
}

#[test]
fn tb04_settle_after_refund_is_already_terminal() {
    ExtBuilder::default().build().execute_with(|| {
        let (job, at) = committed_job();
        System::set_block_number(at + SETTLE_WINDOW);
        assert_ok!(AiReserve::refund_timed_out_ai_job(signed(ANYONE), job));
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(132, 0, 84)),
            Error::<Test>::AlreadyTerminal
        );
        assert_noop!(
            AiReserve::settle_ai_job(signed(ATT), job, receipt(0, 0, 0)),
            Error::<Test>::AlreadyTerminal
        );
    });
}

#[test]
fn tb05_one_terminal_per_job() {
    ExtBuilder::default().build().execute_with(|| {
        let (job, at) = committed_job();
        System::set_block_number(at + SETTLE_WINDOW);
        assert_ok!(AiReserve::refund_timed_out_ai_job(signed(ANYONE), job));
        let events = System::events().len();
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(signed(ANYONE), job),
            Error::<Test>::AlreadyTerminal
        );
        assert_eq!(System::events().len(), events);
        // refund after settle
        let t2 = terms_with([0x60; 32], WORKER, at + SETTLE_WINDOW + 10);
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), t2.clone()));
        assert_ok!(AiReserve::commit_ai_result(signed(WORKER), 1, RESULT));
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), 1, receipt_for(&t2, RECEIPT, 132, 0, 84)));
        System::set_block_number(at + 3 * SETTLE_WINDOW);
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(signed(ANYONE), 1),
            Error::<Test>::AlreadyTerminal
        );
        // refund after cancel
        let t3 = terms_with([0x61; 32], WORKER, at + 3 * SETTLE_WINDOW + 5);
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), t3));
        assert_ok!(AiReserve::cancel_uncommitted_ai_job(signed(SCHED), 2));
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(signed(ANYONE), 2),
            Error::<Test>::AlreadyTerminal
        );
        // in-window settlement still pays only the registered rate (300)
        assert_eq!(AiJobs::<Test>::get(1).unwrap().paid, 300);
        assert_eq!(totals(), (0, 300, 1408 + 1108 + 1408));
        assert_conserved();
    });
}

#[test]
fn tb06_timeout_refund_does_not_consume_receipt() {
    ExtBuilder::default().build().execute_with(|| {
        let (job, at) = committed_job();
        System::set_block_number(at + SETTLE_WINDOW);
        assert_ok!(AiReserve::refund_timed_out_ai_job(signed(ANYONE), job));
        assert_eq!(UsedReceipts::<Test>::iter().count(), 0);
        assert_eq!(AiJobs::<Test>::get(job).unwrap().receipt_id, None);
        // the same receipt id is still usable for another job
        let t2 = terms_with([0x62; 32], WORKER, at + SETTLE_WINDOW + 10);
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), t2.clone()));
        assert_ok!(AiReserve::commit_ai_result(signed(WORKER), 1, RESULT));
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), 1, receipt_for(&t2, RECEIPT, 132, 0, 84)));
        assert_eq!(UsedReceipts::<Test>::get(RECEIPT), Some(1));
    });
}

#[test]
fn tb07_not_timed_out_inside_window_and_not_committed_for_reserved() {
    ExtBuilder::default().build().execute_with(|| {
        let job = reserve_default();
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(signed(ANYONE), job),
            Error::<Test>::NotCommitted
        );
        System::set_block_number(200);
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(signed(ANYONE), job),
            Error::<Test>::NotCommitted
        );
        // uncommitted jobs use call 15 instead
        assert_ok!(AiReserve::cancel_uncommitted_ai_job(signed(ANYONE), job));
        System::set_block_number(201);
        let t2 = terms_with([0x63; 32], WORKER, 220);
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), t2));
        assert_ok!(AiReserve::commit_ai_result(signed(WORKER), 1, RESULT));
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(signed(ANYONE), 1),
            Error::<Test>::NotTimedOut
        );
        System::set_block_number(201 + SETTLE_WINDOW - 1);
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(signed(ANYONE), 1),
            Error::<Test>::NotTimedOut
        );
        System::set_block_number(201 + SETTLE_WINDOW);
        assert_ok!(AiReserve::refund_timed_out_ai_job(signed(ANYONE), 1));
        assert_noop!(
            AiReserve::refund_timed_out_ai_job(signed(ANYONE), 77),
            Error::<Test>::UnknownJob
        );
    });
}

#[test]
fn mixed_sequence_conserves_reserve_and_issuance() {
    ExtBuilder::default().build().execute_with(|| {
        let (j0, at) = committed_job();
        assert_ok!(AiReserve::settle_ai_job(signed(ATT), j0, receipt(132, 0, 84)));
        let t1 = terms_with([0x70; 32], WORKER, at + 40);
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), t1));
        assert_ok!(AiReserve::commit_ai_result(signed(WORKER), 1, RESULT));
        let t2 = terms_with([0x71; 32], WORKER2, at + 40);
        assert_ok!(AiReserve::reserve_ai_job(signed(SCHED), t2));
        assert_eq!(totals().0, 2 * 1408);
        assert_conserved();
        assert_ok!(AiReserve::cancel_uncommitted_ai_job(signed(SCHED), 2));
        System::set_block_number(at + SETTLE_WINDOW);
        assert_ok!(AiReserve::refund_timed_out_ai_job(signed(ANYONE), 1));
        assert_eq!(totals(), (0, 300, 1108 + 1408 + 1408));
        assert_eq!(reserve_reserved(), 0);
        assert_conserved();
    });
}
