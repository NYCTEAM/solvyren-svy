//! Height-based geometric budgets in native atoms. Distribute rounding across blocks.
pub const UNIT: u128 = 1_000_000_000_000;
pub const CAP: u128 = 210_000_000 * UNIT;
pub const TEAM_GENESIS: u128 = 10_000_000 * UNIT;
pub const PERIOD_BLOCKS: u64 = 1_576_800;
const REWARD_BUDGET: u128 = CAP - TEAM_GENESIS;

// Carry the remaining budget. Rounding down leaves no permanently stranded atoms.
fn remaining_after_period(remaining: u128) -> u128 {
    remaining * 88_174 / 100_000
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error { InvalidGenesis, InvalidHeight }

pub fn quote(height: u64, genesis: u128, scheduled: u128) -> Result<u128, Error> {
    if genesis != TEAM_GENESIS { return Err(Error::InvalidGenesis); }
    let index = height.checked_sub(1).ok_or(Error::InvalidHeight)?;
    let mut remaining = REWARD_BUDGET;
    for _ in 0..index / PERIOD_BLOCKS {
        remaining = remaining_after_period(remaining);
        if remaining == 0 { return Ok(0); }
    }
    let budget = remaining - remaining_after_period(remaining);
    let offset = u128::from(index % PERIOD_BLOCKS);
    let blocks = u128::from(PERIOD_BLOCKS);
    let reward = budget * (offset + 1) / blocks - budget * offset / blocks;
    Ok(reward.min(REWARD_BUDGET.saturating_sub(scheduled)))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn allocation_and_period_boundaries() {
        assert_eq!(quote(0, TEAM_GENESIS, 0), Err(Error::InvalidHeight));
        assert_eq!(quote(1, 50_000_000 * UNIT, 0), Err(Error::InvalidGenesis));
        assert_eq!(quote(1, TEAM_GENESIS, 0), Ok(15 * UNIT));
        assert_eq!(quote(PERIOD_BLOCKS, TEAM_GENESIS, 0), Ok(15 * UNIT));
        assert_eq!(quote(PERIOD_BLOCKS + 1, TEAM_GENESIS, 0), Ok(13_226_100_000_000));
        assert_eq!(quote(1, TEAM_GENESIS, REWARD_BUDGET - 1), Ok(1));
        assert_eq!(quote(u64::MAX, TEAM_GENESIS, 0), Ok(0));
        let mut remaining = REWARD_BUDGET;
        let mut total = 0;
        while remaining > 0 {
            let next = remaining_after_period(remaining);
            total += remaining - next;
            remaining = next;
        }
        assert_eq!(total, 200_000_000 * UNIT);
    }
}
