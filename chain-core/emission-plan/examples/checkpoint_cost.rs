// Planning-only same-host recovery timing, excluding checkpoint preparation.
use eagle_emission_plan::{
    accounting::{PlanningCheckpoint, PlanningLedger},
    quote_period, BLOCKS_PER_PERIOD,
};
use std::{hint::black_box, time::Instant};
fn main() {
    for period in [24u32, 2000, 9999] {
        let mut total = 0;
        let mut base = 0;
        let mut ai = 0;
        for p in 0..period {
            let s = quote_period(p, total).unwrap();
            total += s.issued;
            base += s.base;
            ai += s.ai;
        }
        let height = u64::from(period) * BLOCKS_PER_PERIOD as u64;
        let cp = PlanningCheckpoint {
            version: 1,
            last_height: height,
            genesis_issued: 0,
            cumulative_issued: total,
            base_issued: base,
            ai_issued: ai,
        };
        let start = Instant::now();
        for _ in 0..3 {
            let restored =
                black_box(PlanningLedger::from_checkpoint(black_box(cp), height, 0).unwrap());
            assert_eq!(restored.checkpoint(), cp);
        }
        println!("{{\"period\":{},\"restores\":3,\"elapsedNs\":{},\"total\":\"{}\",\"runtimeWeight\":false}}",period,start.elapsed().as_nanos(),total);
    }
}
