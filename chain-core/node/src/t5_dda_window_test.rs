//! T5 continuous-window tests. Every `dda_*` test below calls the real
//! `PowAlgorithm::difficulty` of `DynamicDifficultyAlgorithm` (the type that
//! service.rs constructs), backed by an in-memory `DifficultyChain` instead of
//! a database. No FixedTargetAlgorithm is used here.
use super::*;
use eagle_runtime::Header;
use sp_runtime::generic::Digest;
use std::collections::HashMap;

struct MockChain {
    headers: HashMap<H256, Header>,
    timestamps: HashMap<H256, u64>,
    aux: HashMap<H256, (u128, u128)>,
}

impl DifficultyChain for MockChain {
    fn pow_header(&self, hash: H256) -> Result<Header, Error<Block>> {
        self.headers
            .get(&hash)
            .cloned()
            .ok_or_else(|| Error::Other(format!("mock missing header {hash:?}")))
    }
    fn pow_timestamp(&self, hash: H256) -> Result<u64, Error<Block>> {
        self.timestamps
            .get(&hash)
            .copied()
            .ok_or_else(|| Error::Other(format!("mock missing timestamp {hash:?}")))
    }
    fn pow_aux(&self, hash: H256) -> Result<Option<PowAux<u128>>, Error<Block>> {
        Ok(self.aux.get(&hash).map(|(difficulty, total_difficulty)| PowAux {
            difficulty: *difficulty,
            total_difficulty: *total_difficulty,
        }))
    }
}

/// `ts[0]` is the genesis timestamp, `ts[i]` the timestamp of block `i`.
/// Every non-genesis block carries PoW aux difficulty `difficulty`.
fn chain(ts: &[u64], difficulty: u128) -> (DynamicDifficultyAlgorithm<MockChain>, Vec<H256>) {
    chain_with_difficulties(ts, |_| difficulty)
}

fn chain_with_difficulties(
    ts: &[u64],
    difficulty_at: impl Fn(usize) -> u128,
) -> (DynamicDifficultyAlgorithm<MockChain>, Vec<H256>) {
    let mut headers = HashMap::new();
    let mut timestamps = HashMap::new();
    let mut aux = HashMap::new();
    let mut hashes = Vec::new();
    let mut parent = H256::zero();
    let mut total_difficulty = 0u128;
    for (number, timestamp) in ts.iter().enumerate() {
        let header = Header::new(
            number as u32,
            H256::from_low_u64_be(number as u64 + 1),
            H256::repeat_byte(0x5a),
            parent,
            Digest::default(),
        );
        let hash = header.hash();
        headers.insert(hash, header);
        timestamps.insert(hash, *timestamp);
        if number > 0 {
            let difficulty = difficulty_at(number);
            total_difficulty += difficulty;
            aux.insert(hash, (difficulty, total_difficulty));
        }
        hashes.push(hash);
        parent = hash;
    }
    let mock = MockChain { headers, timestamps, aux };
    (DynamicDifficultyAlgorithm::new(hashes[0], Arc::new(mock)), hashes)
}

/// Builds timestamps from a genesis value and per-block intervals:
/// `interval(i)` is ts(i) - ts(i-1).
fn spaced(genesis: u64, last: u32, interval: impl Fn(u32) -> u64) -> Vec<u64> {
    let mut ts = vec![genesis];
    for i in 1..=last {
        let previous = *ts.last().unwrap();
        ts.push(previous + interval(i));
    }
    ts
}

fn err_text(result: Result<u128, Error<Block>>) -> String {
    match result {
        Ok(value) => panic!("expected rejection, got difficulty {value}"),
        Err(error) => format!("{error}"),
    }
}

/// Non-zero genesis timestamp used by the generic window tests.
const G: u64 = 1_000_000;
/// A realistic block-1 timestamp for chains whose genesis timestamp is 0.
const T1: u64 = 1_794_000_000_000;

/// Genesis timestamp 0 (as on pow-mx10-n16-local, where genesis storage has no
/// `Timestamp::Now`), block 1 at `T1`, then `interval(i)` for i >= 2.
fn genesis_zero(last: u32, interval: impl Fn(u32) -> u64) -> Vec<u64> {
    let mut ts = vec![0, T1];
    for i in 2..=last {
        let previous = *ts.last().unwrap();
        ts.push(previous + interval(i));
    }
    ts
}

#[test]
fn dda_t5_constants_match_spec() {
    assert_eq!(RETARGET_BLOCKS, 60);
    assert_eq!(SAMPLE_INTERVALS, 60);
    assert_eq!(u64::from(SAMPLE_INTERVALS) * TARGET_BLOCK_MILLIS, 600_000);
    assert_eq!(TARGET_BLOCK_MILLIS, 10_000);
    assert_eq!(retarget_difficulty(100, 600_000).unwrap(), 100);
    assert_eq!(retarget_difficulty(100, 300_000).unwrap(), 200);
    assert_eq!(retarget_difficulty(100, 1_200_000).unwrap(), 50);
    // Experimental difficulty and fixed target are unchanged.
    assert_eq!(EXPERIMENTAL_DIFFICULTY, 4);
    assert_eq!(EXPERIMENTAL_TARGET[0], 0x3f);
    assert!(EXPERIMENTAL_TARGET[1..].iter().all(|b| *b == 0xff));
    assert_eq!(target_for_difficulty(EXPERIMENTAL_DIFFICULTY).unwrap(), EXPERIMENTAL_TARGET);
    assert_eq!(MX10_SHARE_POOL_ACTIVATION_HEIGHT, 13_001);
    assert_eq!(MX10_SHARE_POOL_BOOTSTRAP_DIFFICULTY, 3_225_000);
}

#[test]
fn dda_share_pool_activation_seeds_measured_throughput_once() {
    let ts = spaced(G, MX10_SHARE_POOL_ACTIVATION_HEIGHT, |_| 10_000);
    let (mut algorithm, hashes) = chain_with_difficulties(&ts, |height| {
        if height >= MX10_SHARE_POOL_ACTIVATION_HEIGHT as usize {
            MX10_SHARE_POOL_BOOTSTRAP_DIFFICULTY
        } else {
            1
        }
    });
    let activation_parent = (MX10_SHARE_POOL_ACTIVATION_HEIGHT - 1) as usize;
    algorithm.genesis = "0x678107bc4d21a4e7f9b12ee817af0c93f074cf04fbd7ee716976af42ca92f4c8".parse().unwrap();
    assert_eq!(algorithm.difficulty(hashes[activation_parent]).unwrap(), 1);
    algorithm.genesis = crate::mx10_context::PUBLICDEV_GENESIS.parse().unwrap();
    assert_eq!(algorithm.difficulty(hashes[activation_parent - 1]).unwrap(), 1);
    assert_eq!(algorithm.difficulty(hashes[activation_parent]).unwrap(), 3_225_000);
    assert_eq!(algorithm.difficulty(hashes[activation_parent + 1]).unwrap(), 3_225_000);
}

#[test]
fn dda_genesis_parent_returns_initial_difficulty_without_window() {
    let (algorithm, hashes) = chain(&[G], 100);
    assert_eq!(algorithm.difficulty(hashes[0]).unwrap(), EXPERIMENTAL_DIFFICULTY);
    assert_eq!(algorithm.difficulty(hashes[0]).unwrap(), 4);
}

#[test]
fn dda_retarget_triggers_when_parent_height_multiple_of_60() {
    // 1_000 ms intervals: any window recompute clamps to the 300_000 floor.
    let (algorithm, hashes) = chain(&spaced(G, 121, |_| 1_000), 100);
    assert_eq!(algorithm.difficulty(hashes[59]).unwrap(), 100);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 200);
    assert_eq!(algorithm.difficulty(hashes[61]).unwrap(), 100);
    assert_eq!(algorithm.difficulty(hashes[119]).unwrap(), 100);
    assert_eq!(algorithm.difficulty(hashes[120]).unwrap(), 200);
    assert_eq!(algorithm.difficulty(hashes[121]).unwrap(), 100);
}

#[test]
fn dda_proposed_first_window_elapsed_is_ts60_minus_genesis() {
    // Non-zero genesis timestamp G (T5-02: only a zero start timestamp skips).
    // genesis -> 1 is 150_000 ms, then 59 x 5_000 ms.
    // ts(60) - ts(genesis) = 445_000 -> 100 * 600_000 / 445_000 = 134.
    // The former rule (ts(60) - ts(1) = 295_000 over 59 intervals) would give 200.
    let ts = spaced(G, 60, |i| if i == 1 { 150_000 } else { 5_000 });
    assert_eq!(ts[60] - ts[0], 445_000);
    let (algorithm, hashes) = chain(&ts, 100);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 134);
}

#[test]
fn dda_proposed_second_window_includes_seam_60_to_61() {
    // On-target first window, then a 300_000 ms stall on the 60 -> 61 seam.
    // ts(120) - ts(60) = 300_000 + 59 x 10_000 = 890_000 -> 600_000 * 100 / 890_000 = 67.
    // Excluding the seam (ts(120) - ts(61) = 590_000) would give 101 (or 100 under the old rule).
    let ts = spaced(G, 120, |i| if i == 61 { 300_000 } else { 10_000 });
    assert_eq!(ts[120] - ts[60], 890_000);
    let (algorithm, hashes) = chain(&ts, 100);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 100);
    assert_eq!(algorithm.difficulty(hashes[120]).unwrap(), 67);
}

#[test]
fn dda_proposed_on_target_600000ms() {
    let ts = spaced(G, 120, |_| 10_000);
    assert_eq!(ts[60] - ts[0], 600_000);
    assert_eq!(ts[120] - ts[60], 600_000);
    let (algorithm, hashes) = chain(&ts, 100);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 100);
    assert_eq!(algorithm.difficulty(hashes[120]).unwrap(), 100);
}

#[test]
fn dda_proposed_clamp_floor_300000ms() {
    for (ts, want) in [
        (spaced(G, 60, |_| 1_000), 200),                            // 60_000 -> floor
        (spaced(G, 60, |i| if i == 7 { 4_999 } else { 5_000 }), 200), // 299_999 -> floor
        (spaced(G, 60, |_| 5_000), 200),                            // exactly 300_000
        (spaced(G, 60, |i| if i == 7 { 5_001 } else { 5_000 }), 199), // 300_001 unclamped
    ] {
        let (algorithm, hashes) = chain(&ts, 100);
        assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), want, "elapsed {}", ts[60] - ts[0]);
    }
}

#[test]
fn dda_proposed_clamp_ceil_1200000ms() {
    for (ts, want) in [
        (spaced(G, 60, |_| 30_000), 500),                              // 1_800_000 -> ceiling (unclamped 333)
        (spaced(G, 60, |_| 20_000), 500),                              // exactly 1_200_000
        (spaced(G, 60, |i| if i == 7 { 19_999 } else { 20_000 }), 500), // 1_199_999 unclamped -> 500
        (spaced(G, 60, |i| if i == 7 { 1 } else { 18_000 }), 564),      // 1_062_001 unclamped
    ] {
        let (algorithm, hashes) = chain(&ts, 1_000);
        assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), want, "elapsed {}", ts[60] - ts[0]);
    }
}

#[test]
fn dda_reject_window_timestamp_regression() {
    let mut ts = spaced(G, 60, |_| 10_000);
    ts[30] = ts[29] - 1;
    let (algorithm, hashes) = chain(&ts, 100);
    assert!(err_text(algorithm.difficulty(hashes[60])).contains("strictly increasing"));
}

#[test]
fn dda_reject_window_timestamp_equal() {
    let mut ts = spaced(G, 60, |_| 10_000);
    ts[30] = ts[29];
    let (algorithm, hashes) = chain(&ts, 100);
    assert!(err_text(algorithm.difficulty(hashes[60])).contains("strictly increasing"));
}

#[test]
fn dda_proposed_seam_60_to_61_must_be_strictly_increasing() {
    for delta in [0i64, -1] {
        let mut ts = spaced(G, 120, |_| 10_000);
        ts[61] = (ts[60] as i64 + delta) as u64;
        let (algorithm, hashes) = chain(&ts, 100);
        // The seam is not part of the first window...
        assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 100);
        // ...but the second window now walks 120 -> 60 and must see it.
        assert!(err_text(algorithm.difficulty(hashes[120])).contains("strictly increasing"));
    }
}

#[test]
fn dda_proposed_genesis_to_block1_pair_must_be_strictly_increasing() {
    let mut ts = spaced(G, 60, |_| 10_000);
    ts[1] = ts[0];
    let (algorithm, hashes) = chain(&ts, 100);
    assert!(err_text(algorithm.difficulty(hashes[60])).contains("strictly increasing"));
}

#[test]
fn dda_window_layer_accepts_positive_increment_below_minimum_period() {
    // Layer separation: a 1 ms (positive) step passes the window's strict
    // increase check; rejecting it is pallet-timestamp's MinimumPeriod job
    // (see t5_dda_import_test import_reject_adjacent_increment_below_5000ms).
    let ts = spaced(G, 60, |i| if i == 30 { 1 } else { 10_000 });
    let (algorithm, hashes) = chain(&ts, 100);
    assert!(algorithm.difficulty(hashes[60]).is_ok());
}

#[test]
fn dda_service_constructs_dynamic_difficulty_algorithm() {
    let service = include_str!("service.rs");
    assert_eq!(
        service
            .matches("crate::active_pow_consensus::DynamicDifficultyAlgorithm::new(genesis, client.clone())")
            .count(),
        2,
        "import queue and authoring paths must both construct DynamicDifficultyAlgorithm"
    );
    assert!(service.contains("crate::active_pow_consensus::StrictPowVerifier::new(algorithm)"));
    assert!(!service.contains("FixedTargetAlgorithm"));
    let main = include_str!("main.rs");
    assert!(main.contains(
        "#[cfg(any(not(feature = \"gpu-pow-v3-rewards-dev\"), feature = \"gpu-pow-matrix-block-rewards-dev\"))]\nuse gpu_pow_consensus as active_pow_consensus;"
    ));
    // Context extraction retains the SAME dynamic implementation, reached
    // transitively by template/import rather than another target algorithm.
    let context = include_str!("mx10_context.rs");
    assert!(context.contains("DynamicDifficultyAlgorithm::new(self.genesis, client)"));
    assert!(context.contains("WorkAlgorithm::Mx10N16(self)"));
    assert!(!context.contains("FixedTargetAlgorithm"));
    let import = include_str!("mx10_import.rs");
    assert!(import.contains("trusted.target(client,parent)"));
    assert!(!import.contains("FixedTargetAlgorithm"));
    let template = include_str!("mx10_template.rs");
    assert!(template.contains("context.difficulty(client.clone(), parent)"));
    assert!(template.contains("target_for_difficulty(difficulty)"));
    assert!(!template.contains("FixedTargetAlgorithm"));
    let author = include_str!("mx10_author.rs");
    assert!(author.contains("DynamicDifficultyAlgorithm::new(genesis"));
    assert!(author.contains("WorkAlgorithm::Mx10N16(context)"));
    assert!(!author.contains("FixedTargetAlgorithm"));
    // Type-level proof: under the matrix feature the service alias is this module's
    // DynamicDifficultyAlgorithm over the production FullClient.
    #[cfg(feature = "gpu-pow-matrix-block-rewards-dev")]
    {
        let _constructor: fn(H256, Arc<crate::service::FullClient>) -> crate::active_pow_consensus::DynamicDifficultyAlgorithm =
            DynamicDifficultyAlgorithm::<crate::service::FullClient>::new;
    }
}

#[test]
fn dda_genesis_ts0_first_window_skips_retarget_height_61_stays_4() {
    // T5-02: the first window starts at genesis with timestamp 0, so
    // ts(60) - 0 is not a block-time measurement; the parent difficulty is kept
    // whatever the in-window spacing (T5-01 would have halved 4 -> 2 here).
    for interval in [1_000u64, 5_000, 10_000, 30_000] {
        let ts = genesis_zero(121, |_| interval);
        assert_eq!(ts[0], 0);
        let (algorithm, hashes) = chain(&ts, EXPERIMENTAL_DIFFICULTY);
        assert_eq!(algorithm.difficulty(hashes[0]).unwrap(), 4, "interval {interval}");
        assert_eq!(algorithm.difficulty(hashes[59]).unwrap(), 4, "interval {interval}");
        assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 4, "height 61, interval {interval}");
        assert_eq!(algorithm.difficulty(hashes[61]).unwrap(), 4, "height 62, interval {interval}");
        assert_eq!(algorithm.difficulty(hashes[119]).unwrap(), 4, "height 120, interval {interval}");
    }
}

#[test]
fn dda_genesis_ts0_second_window_still_retargets_from_block_60() {
    // The second window starts at block 60 (non-zero timestamp) and is retargeted.
    // 10_000 ms spacing with a 300_000 ms seam 60 -> 61: elapsed 890_000 -> 4 * 600_000 / 890_000 = 2.
    let ts = genesis_zero(121, |i| if i == 61 { 300_000 } else { 10_000 });
    assert_eq!(ts[120] - ts[60], 890_000);
    let (algorithm, hashes) = chain(&ts, EXPERIMENTAL_DIFFICULTY);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 4);
    assert_eq!(algorithm.difficulty(hashes[120]).unwrap(), 2);
    // Without the seam (ts(120) - ts(61) = 590_000) it would have stayed 4.
    assert_eq!(retarget_difficulty(4, ts[120] - ts[61]).unwrap(), 4);
    // Fast second window clamps to the floor: 60 x 1_000 -> 300_000 -> 8.
    let fast = genesis_zero(121, |i| if i <= 60 { 10_000 } else { 1_000 });
    let (algorithm, hashes) = chain(&fast, EXPERIMENTAL_DIFFICULTY);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 4);
    assert_eq!(algorithm.difficulty(hashes[120]).unwrap(), 8);
    // Slow second window clamps to the ceiling: 60 x 30_000 -> 1_200_000 -> 2.
    let slow = genesis_zero(121, |i| if i <= 60 { 10_000 } else { 30_000 });
    let (algorithm, hashes) = chain(&slow, EXPERIMENTAL_DIFFICULTY);
    assert_eq!(algorithm.difficulty(hashes[120]).unwrap(), 2);
}

#[test]
fn dda_genesis_nonzero_first_window_uses_ts60_minus_ts0_and_clamps() {
    // A non-zero genesis timestamp keeps the T5-01 first-window rule.
    // Huge gap genesis -> 1 (3 h): ceiling 1_200_000 -> 4 / 2 = 2.
    let ts = spaced(G, 60, |i| if i == 1 { 3 * 3600 * 1000 } else { 10_000 });
    let (algorithm, hashes) = chain(&ts, EXPERIMENTAL_DIFFICULTY);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 2);
    // On target: 600_000 -> 4.
    let ts = spaced(G, 60, |_| 10_000);
    let (algorithm, hashes) = chain(&ts, EXPERIMENTAL_DIFFICULTY);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 4);
    // Fast: 60 x 1_000 -> floor 300_000 -> 8.
    let ts = spaced(G, 60, |_| 1_000);
    let (algorithm, hashes) = chain(&ts, EXPERIMENTAL_DIFFICULTY);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 8);
    // Genesis timestamp 1 (smallest non-zero) is not skipped.
    let mut ts = genesis_zero(60, |_| 10_000);
    ts[0] = 1;
    let (algorithm, hashes) = chain(&ts, EXPERIMENTAL_DIFFICULTY);
    assert_eq!(algorithm.difficulty(hashes[60]).unwrap(), 2);
}

#[test]
fn dda_genesis_ts0_skip_still_runs_window_checks() {
    // Strict increase is checked before the skip: genesis 0 and block 1 at 0 is rejected.
    let mut ts = genesis_zero(60, |_| 10_000);
    ts[1] = 0;
    let (algorithm, hashes) = chain(&ts, EXPERIMENTAL_DIFFICULTY);
    assert!(err_text(algorithm.difficulty(hashes[60])).contains("strictly increasing"));
    // A regression inside the window is still rejected even though the start is 0.
    let mut ts = genesis_zero(60, |_| 10_000);
    ts[30] = ts[29];
    let (algorithm, hashes) = chain(&ts, EXPERIMENTAL_DIFFICULTY);
    assert!(err_text(algorithm.difficulty(hashes[60])).contains("strictly increasing"));
    // The height-0 start must still be the configured genesis.
    let ts = genesis_zero(60, |_| 10_000);
    let (algorithm, hashes) = chain(&ts, EXPERIMENTAL_DIFFICULTY);
    let foreign = DynamicDifficultyAlgorithm::new(H256::repeat_byte(0x99), algorithm.client.clone());
    assert!(err_text(foreign.difficulty(hashes[60])).contains("configured genesis"));
}
