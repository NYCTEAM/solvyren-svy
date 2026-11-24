use eagle_emission_plan::{period_reward, quote_period, CAP};
fn main() {
    let mut total = 0;
    let mut base = 0;
    let mut ai = 0;
    for period in 0..10000 {
        let s = quote_period(period, total).expect("valid period and supply");
        total += s.issued;
        base += s.base;
        ai += s.ai;
        println!("{{\"period\":{},\"blockReward\":\"{}\",\"issued\":\"{}\",\"baseIssued\":\"{}\",\"aiIssued\":\"{}\",\"total\":\"{}\",\"base\":\"{}\",\"ai\":\"{}\",\"remaining\":\"{}\"}}",period+1,period_reward(period).unwrap(),s.issued,s.base,s.ai,total,base,ai,CAP-total);
    }
}
