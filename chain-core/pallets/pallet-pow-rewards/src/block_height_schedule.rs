//! Block-height issuance arithmetic. Height one is the first rewarded block.
//!
//! The first 24 period budgets use the historical early weights and are
//! normalized separately to the approved 15% and 10% of the mining budget.
//! Thereafter period `24 + k` has budget `floor(B24 * pow(k))`, where k starts
//! at one and pow is fixed-point exponentiation by squaring. Each fixed-point
//! multiplication is rounded down. This deterministic bounded algorithm can
//! eventually reach zero; blocks must remain valid when the reward is zero.
//! A block receives a difference of two integer period prefixes. Missing
//! issuance is never accumulated or attributed to a later block.

pub const UNIT: u128 = 1_000_000_000_000;
pub const CAP: u128 = 1_000_000_000 * UNIT;
pub const TEAM_GENESIS: u128 = 50_000_000 * UNIT;
pub const PERIOD_BLOCKS: u64 = 219_000;
const SCALE: u128 = 1_000_000_000_000;
const EARLY: u128 = 966_775_705_727;
const TAIL: u128 = 990_918_932_689;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidGenesis,
    InvalidHeight,
    Overflow,
}

fn mul_div(a: u128, b: u128, d: u128) -> Result<u128, Error> {
    a.checked_mul(b).ok_or(Error::Overflow).map(|n| n / d)
}

fn budgets() -> Result<[u128; 24], Error> {
    let mut weights = [0; 24];
    weights[0] = SCALE;
    for i in 1..24 {
        weights[i] = mul_div(weights[i - 1], EARLY, SCALE)?;
    }
    let mut result = [0; 24];
    let available = CAP - TEAM_GENESIS;
    for (start, total) in [(0, available * 15 / 100), (12, available * 10 / 100)] {
        let mut sum = 0u128;
        for weight in &weights[start..start + 12] {
            sum = sum.checked_add(*weight).ok_or(Error::Overflow)?;
        }
        let mut prefix = 0u128;
        let mut previous = 0u128;
        for i in start..start + 12 {
            prefix = prefix.checked_add(weights[i]).ok_or(Error::Overflow)?;
            let target = mul_div(total, prefix, sum)?;
            result[i] = target.checked_sub(previous).ok_or(Error::Overflow)?;
            previous = target;
        }
    }
    Ok(result)
}

fn fixed_pow(mut exponent: u64) -> Result<u128, Error> {
    let mut result = SCALE;
    let mut base = TAIL;
    while exponent != 0 {
        if exponent & 1 != 0 {
            result = mul_div(result, base, SCALE)?;
        }
        exponent >>= 1;
        if exponent != 0 {
            base = mul_div(base, base, SCALE)?;
        }
    }
    Ok(result)
}

fn period_budget(period: u64) -> Result<u128, Error> {
    let early = budgets()?;
    if period < 24 {
        Ok(early[period as usize])
    } else {
        let power = fixed_pow(period - 23)?;
        mul_div(early[23], power, SCALE)
    }
}

/// Reward for `height`, clipped to the remaining total issuance capacity.
/// `scheduled` is all non-genesis issuance already made; a skipped or zero
/// reward is not made up on subsequent blocks. Height zero is genesis.
pub fn quote(height: u64, genesis: u128, scheduled: u128) -> Result<u128, Error> {
    if genesis != TEAM_GENESIS {
        return Err(Error::InvalidGenesis);
    }
    if height == 0 {
        return Err(Error::InvalidHeight);
    }
    let index = height - 1;
    let period = index / PERIOD_BLOCKS;
    let offset = index % PERIOD_BLOCKS;
    let budget = period_budget(period)?;
    let current = mul_div(budget, u128::from(offset + 1), u128::from(PERIOD_BLOCKS))?;
    let previous = mul_div(budget, u128::from(offset), u128::from(PERIOD_BLOCKS))?;
    let reward = current.checked_sub(previous).ok_or(Error::Overflow)?;
    Ok(reward.min(CAP.saturating_sub(genesis).saturating_sub(scheduled)))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn genesis_and_first_24_period_anchors() {
        let b = budgets().unwrap();
        assert_eq!(TEAM_GENESIS * 20, CAP);
        assert_eq!(
            b[..12].iter().sum::<u128>(),
            (CAP - TEAM_GENESIS) * 15 / 100
        );
        assert_eq!(b.iter().sum::<u128>(), (CAP - TEAM_GENESIS) * 25 / 100);
        assert_eq!(quote(1, 0, 0), Err(Error::InvalidGenesis));
        assert_eq!(quote(0, TEAM_GENESIS, 0), Err(Error::InvalidHeight));
    }

    #[test]
    fn period_boundaries_and_tail_are_defined() {
        let b = budgets().unwrap();
        assert!(b.windows(2).all(|pair| pair[0] > pair[1]));
        assert_eq!(period_budget(24).unwrap(), b[23] * TAIL / SCALE);
        for p in [1, 12, 24, 25] {
            let boundary = p * PERIOD_BLOCKS;
            for height in [boundary - 1, boundary, boundary + 1] {
                assert!(quote(height, TEAM_GENESIS, 0).is_ok());
            }
        }
    }

    #[test]
    fn prefix_differences_conserve_each_selected_period() {
        for period in [0, 11, 23, 24] {
            let budget = period_budget(period).unwrap();
            let mut sum = 0u128;
            // Calculate the same prefixes independently at all boundaries.
            for offset in 0..PERIOD_BLOCKS {
                let a = budget * u128::from(offset) / u128::from(PERIOD_BLOCKS);
                let b = budget * u128::from(offset + 1) / u128::from(PERIOD_BLOCKS);
                sum += b - a;
                if offset == 0 || offset == PERIOD_BLOCKS / 2 || offset == PERIOD_BLOCKS - 1 {
                    let height = period * PERIOD_BLOCKS + offset + 1;
                    assert_eq!(quote(height, TEAM_GENESIS, 0).unwrap(), b - a);
                }
            }
            assert_eq!(sum, budget);
        }
    }

    #[test]
    fn no_catch_up_and_cap_are_safe() {
        let h = PERIOD_BLOCKS * 25 + 7;
        let q = quote(h, TEAM_GENESIS, 0).unwrap();
        assert_eq!(q, quote(h, TEAM_GENESIS, 123).unwrap());
        assert_eq!(
            quote(h, TEAM_GENESIS, CAP - TEAM_GENESIS - 1).unwrap(),
            q.min(1)
        );
        assert_eq!(quote(h, TEAM_GENESIS, CAP - TEAM_GENESIS).unwrap(), 0);
        assert_eq!(quote(h, TEAM_GENESIS, u128::MAX).unwrap(), 0);
        assert_eq!(quote(u64::MAX, TEAM_GENESIS, 0).unwrap(), 0);
        assert_eq!(quote(u64::MAX - 1, TEAM_GENESIS, 0).unwrap(), 0);
    }

    #[test]
    fn every_representable_full_period_is_monotone_and_within_cap() {
        let max_full_periods = u64::from(u32::MAX) / PERIOD_BLOCKS;
        let mut total = 0u128;
        let mut previous = u128::MAX;
        let mut first_zero = None;
        for period in 0..max_full_periods {
            let amount = period_budget(period).unwrap();
            assert!(amount <= previous, "reward increased in period {period}");
            if amount == 0 && first_zero.is_none() {
                first_zero = Some(period + 1);
            }
            previous = amount;
            total += amount.min(CAP - TEAM_GENESIS - total);
            assert!(TEAM_GENESIS + total <= CAP);
        }
        assert_eq!(first_zero, Some(3053));
        assert_eq!(CAP - TEAM_GENESIS - total, 23_411_296_000);
        assert_eq!(quote(u64::from(u32::MAX), TEAM_GENESIS, total).unwrap(), 0);
    }
}
