#![no_std]
//! Pure planning arithmetic. Not linked into the runtime; never mints or transfers.
//! Callers must include genesis allocations in cumulative already_issued, without
//! subtracting burns. Current circulation is not an authoritative issuance counter.
//! Schedule lookup is
//! bounded but not benchmarked for runtime hooks; no consensus policy is installed.

pub mod accounting;

pub const UNIT: u128 = 1_000_000_000_000;
pub const CAP: u128 = 1_000_000_000 * UNIT;
pub const BLOCKS_PER_PERIOD: u128 = 219_000;
pub const INITIAL_REWARD: u128 = 68_269_097_821_096;
const SCALE: u128 = 1_000_000_000_000;
const EARLY: u128 = 966_775_705_727;
const LATE: u128 = 990_918_932_689;

pub(crate) fn next_reward(reward: u128, previous_period: u32) -> u128 {
    reward * if previous_period < 23 { EARLY } else { LATE } / SCALE
}

/// Constant-work transition for a cache held in trusted Runtime storage.
/// This validates bounds, NOT authenticity of an imported reward/cache. Restore
/// untrusted checkpoints with PlanningLedger::from_checkpoint instead.
pub fn advance_stored_reward(previous_reward: u128, previous_period: u32) -> Result<u128, Error> {
    if previous_period >= 9999 || previous_reward > INITIAL_REWARD {
        return Err(Error::InvalidPeriod);
    }
    Ok(next_reward(previous_reward, previous_period))
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Error {
    InvalidPeriod,
    SupplyAlreadyExceeded,
    UnexpectedHeight,
    HeightOverflow,
    AccountingOverflow,
    InvalidCheckpoint,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Split {
    pub issued: u128,
    pub base: u128,
    pub ai: u128,
}

/// Zero-based period, matching the existing JS planning curve for periods 1..10000.
pub fn period_reward(period: u32) -> Result<u128, Error> {
    if period >= 10_000 {
        return Err(Error::InvalidPeriod);
    }
    let mut reward = INITIAL_REWARD;
    for previous in 0..period {
        reward = next_reward(reward, previous);
        if reward == 0 {
            break;
        }
    }
    Ok(reward)
}

/// Clip before multiplication: even u128::MAX requested reward cannot overflow.
/// AI receives floor(30%); the base share receives the integer remainder.
pub fn quote_block(requested: u128, already_issued: u128) -> Result<Split, Error> {
    let remaining = CAP
        .checked_sub(already_issued)
        .ok_or(Error::SupplyAlreadyExceeded)?;
    let issued = requested.min(remaining);
    let ai = issued * 30 / 100;
    Ok(Split {
        issued,
        base: issued - ai,
        ai,
    })
}

/// Bulk quote preserves per-block rounding, including one final clipped block.
pub fn quote_period(period: u32, already_issued: u128) -> Result<Split, Error> {
    quote_blocks_in_period(period, BLOCKS_PER_PERIOD as u32, already_issued)
}

/// Prefix of a single period, useful for validating a planning checkpoint.
pub fn quote_blocks_in_period(
    period: u32,
    blocks: u32,
    already_issued: u128,
) -> Result<Split, Error> {
    if u128::from(blocks) > BLOCKS_PER_PERIOD {
        return Err(Error::InvalidPeriod);
    }
    let reward = period_reward(period)?;
    quote_blocks_with_reward(reward, blocks, already_issued)
}

/// Internal streaming-prefix helper. Reward is derived from the bounded schedule.
pub(crate) fn quote_blocks_with_reward(
    reward: u128,
    blocks: u32,
    already_issued: u128,
) -> Result<Split, Error> {
    if reward > INITIAL_REWARD || u128::from(blocks) > BLOCKS_PER_PERIOD {
        return Err(Error::InvalidPeriod);
    }
    let remaining = CAP
        .checked_sub(already_issued)
        .ok_or(Error::SupplyAlreadyExceeded)?;
    if reward == 0 {
        return Ok(Split {
            issued: 0,
            base: 0,
            ai: 0,
        });
    }
    let full = (remaining / reward).min(u128::from(blocks));
    let partial = if full < u128::from(blocks) {
        remaining % reward
    } else {
        0
    };
    let issued = full * reward + partial;
    let ai = full * (reward * 30 / 100) + partial * 30 / 100;
    Ok(Split {
        issued,
        base: issued - ai,
        ai,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn cap_includes_existing_supply_and_never_overflows() {
        assert_eq!(
            quote_block(u128::MAX, CAP - 5),
            Ok(Split {
                issued: 5,
                base: 4,
                ai: 1
            })
        );
        assert_eq!(
            quote_block(u128::MAX, CAP),
            Ok(Split {
                issued: 0,
                base: 0,
                ai: 0
            })
        );
        assert_eq!(quote_block(0, CAP + 1), Err(Error::SupplyAlreadyExceeded));
        assert_eq!(quote_period(0, CAP + 1), Err(Error::SupplyAlreadyExceeded));
    }
    #[test]
    fn rounding_conserves_even_the_smallest_rewards() {
        for reward in 0..1000 {
            let s = quote_block(reward, 0).unwrap();
            assert_eq!(s.issued, s.base + s.ai);
            assert_eq!(s.ai, reward * 3 / 10);
        }
    }
    #[test]
    fn period_clipping_matches_individual_final_blocks() {
        let reward = period_reward(0).unwrap();
        for remaining in [1, reward - 1, reward, reward + 1, reward * 2 + 7] {
            let period = quote_period(0, CAP - remaining).unwrap();
            let mut issued = CAP - remaining;
            let mut base = 0;
            let mut ai = 0;
            while issued < CAP {
                let s = quote_block(reward, issued).unwrap();
                issued += s.issued;
                base += s.base;
                ai += s.ai;
            }
            assert_eq!(
                period,
                Split {
                    issued: remaining,
                    base,
                    ai
                }
            );
        }
    }
    #[test]
    fn transition_and_terminal_dust_are_explicit() {
        assert_eq!(period_reward(0), Ok(INITIAL_REWARD));
        assert_eq!(
            period_reward(23).unwrap(),
            period_reward(22).unwrap() * EARLY / SCALE
        );
        assert_eq!(
            period_reward(24).unwrap(),
            period_reward(23).unwrap() * LATE / SCALE
        );
        assert!(period_reward(2977).unwrap() > 0);
        assert_eq!(period_reward(2978), Ok(0));
        assert_eq!(period_reward(10000), Err(Error::InvalidPeriod));
    }
    #[test]
    fn long_run_conserves_and_matches_the_agreed_approximate_anchors() {
        let mut total = 0;
        let mut base = 0;
        let mut ai = 0;
        for period in 0..10000 {
            let split = quote_period(period, total).unwrap();
            total += split.issued;
            base += split.base;
            ai += split.ai;
            assert_eq!(total, base + ai);
            assert!(total <= CAP);
            if period == 11 {
                assert!(total.abs_diff(150_000_000 * UNIT) < UNIT / 10);
            }
            if period == 23 {
                assert!(total.abs_diff(250_000_000 * UNIT) < UNIT / 10);
            }
        }
        assert_eq!(CAP - total, 3_069_166_000);
    }
}
