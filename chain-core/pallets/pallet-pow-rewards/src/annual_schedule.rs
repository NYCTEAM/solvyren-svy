//! Thirty height-based annual budgets for the SVY candidate runtime.
pub const UNIT: u128 = 1_000_000_000_000;
pub const CAP: u128 = 1_000_000_000 * UNIT;
pub const TEAM_GENESIS: u128 = 50_000_000 * UNIT;
pub const PERIOD_BLOCKS: u64 = 3_153_600;
const YEARS: u64 = 30;
const WEIGHT_SUM: u128 = 2175; // 87 + 86 + ... + 58

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidGenesis,
    InvalidHeight,
}

// Sum of the first y weights is y * (175 - y) / 2. All callers bound y to 30.
fn cumulative_budget(years: u64) -> u128 {
    let y = u128::from(years);
    (CAP - TEAM_GENESIS) * y * (175 - y) / (2 * WEIGHT_SUM)
}

fn period_budget(period: u64) -> u128 {
    if period >= YEARS {
        return 0;
    }
    cumulative_budget(period + 1) - cumulative_budget(period)
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
    let budget = period_budget(period);
    let current = budget * u128::from(offset + 1) / u128::from(PERIOD_BLOCKS);
    let previous = budget * u128::from(offset) / u128::from(PERIOD_BLOCKS);
    let reward = current - previous;
    Ok(reward.min(CAP.saturating_sub(genesis).saturating_sub(scheduled)))
}


#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_qualified_block_conserves_the_full_budget() {
        let mut total = 0;
        let mut last_budget = CAP;
        for year in 0..YEARS {
            let before = total;
            for height in year * PERIOD_BLOCKS + 1..=(year + 1) * PERIOD_BLOCKS {
                total += quote(height, TEAM_GENESIS, total).unwrap();
            }
            let budget = total - before;
            assert!(budget < last_budget);
            last_budget = budget;
            if year == 0 { assert_eq!(budget, 38_000_000 * UNIT); }
        }
        assert_eq!(total, 950_000_000 * UNIT);
        assert_eq!(last_budget, 25_333_333_333_333_333_334);
        assert_eq!(quote(30 * PERIOD_BLOCKS + 1, TEAM_GENESIS, 0), Ok(0));
        assert_eq!(quote(u64::MAX, TEAM_GENESIS, 0), Ok(0));
    }

    #[test]
    fn first_reward_year_transition_and_cap() {
        assert_eq!(quote(1, TEAM_GENESIS, 0), Ok(12_049_720_953_830));
        assert_eq!(quote(PERIOD_BLOCKS, TEAM_GENESIS, 0), Ok(12_049_720_953_831));
        assert_eq!(quote(PERIOD_BLOCKS + 1, TEAM_GENESIS, 0), Ok(11_911_218_414_131));
        assert_eq!(quote(1, TEAM_GENESIS, CAP - TEAM_GENESIS - 2), Ok(2));
        assert_eq!(quote(0, TEAM_GENESIS, 0), Err(Error::InvalidHeight));
        assert_eq!(quote(1, 0, 0), Err(Error::InvalidGenesis));
    }
}
