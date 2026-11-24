// Same-host planning-only timing; not a Runtime weight or consensus benchmark.
use eagle_emission_plan::{
    accounting::{PlanningCheckpoint, PlanningLedger},
    quote_period, BLOCKS_PER_PERIOD,
};
use std::{hint::black_box, time::Instant};

fn measure(period: u32) -> (u128, u128) {
    let mut total = 0;
    let mut base = 0;
    let mut ai = 0;
    for p in 0..period {
        let split = quote_period(p, total).unwrap();
        total += split.issued;
        base += split.base;
        ai += split.ai;
    }
    let height = u64::from(period) * BLOCKS_PER_PERIOD as u64;
    let checkpoint = PlanningCheckpoint {
        version: 1,
        last_height: height,
        genesis_issued: 0,
        cumulative_issued: total,
        base_issued: base,
        ai_issued: ai,
    };
    let mut ledger = PlanningLedger::from_checkpoint(checkpoint, height, 0).unwrap();
    let started = Instant::now();
    for offset in 1..=50000 {
        black_box(ledger.apply_next(black_box(height + offset)).unwrap());
    }
    let elapsed = started.elapsed().as_nanos();
    (elapsed, black_box(ledger.cumulative_issued()))
}
fn main() {
    let early = measure(0);
    let late = measure(2000);
    println!("{{\"scope\":\"planning-only same-host sequential timing\",\"blocksPerCase\":50000,\"earlyPeriod\":0,\"latePeriod\":2000,\"earlyNs\":{},\"lateNs\":{},\"earlyTotal\":\"{}\",\"lateTotal\":\"{}\",\"runtimeWeight\":false}}",early.0,late.0,early.1,late.1);
}
