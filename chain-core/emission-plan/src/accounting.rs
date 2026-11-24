//! In-memory planning state only. No FRAME storage or currency integration.
//! A future Runtime must persist/roll back this together with currency changes in
//! one transaction. Cloning a checkpoint here is not a blockchain reorg proof.
use crate::{
    next_reward, quote_block, quote_blocks_with_reward, Error, Split, BLOCKS_PER_PERIOD, CAP,
    INITIAL_REWARD,
};

/// Typed planning data, not a storage encoding or authenticated checkpoint.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanningCheckpoint {
    pub version: u16,
    pub last_height: u64,
    pub genesis_issued: u128,
    pub cumulative_issued: u128,
    pub base_issued: u128,
    pub ai_issued: u128,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct PlanningLedger {
    last_height: u64,
    genesis_issued: u128,
    cumulative_issued: u128,
    base_issued: u128,
    ai_issued: u128,
    reward_period: u32,
    reward_per_block: u128,
}

impl PlanningLedger {
    pub fn checkpoint(&self) -> PlanningCheckpoint {
        PlanningCheckpoint {
            version: 1,
            last_height: self.last_height,
            genesis_issued: self.genesis_issued,
            cumulative_issued: self.cumulative_issued,
            base_issued: self.base_issued,
            ai_issued: self.ai_issued,
        }
    }

    /// Caller pins trusted height/genesis independently. Recompute the complete
    /// deterministic prefix, not merely the supplied counter conservation sum.
    /// Bounded planning-only recovery; not benchmarked and never a per-block hook.
    pub fn from_checkpoint(
        checkpoint: PlanningCheckpoint,
        expected_height: u64,
        expected_genesis: u128,
    ) -> Result<Self, Error> {
        if checkpoint.version != 1
            || checkpoint.last_height != expected_height
            || checkpoint.genesis_issued != expected_genesis
            || u128::from(expected_height) > 10000 * BLOCKS_PER_PERIOD
        {
            return Err(Error::InvalidCheckpoint);
        }
        let mut canonical = Self::new(expected_genesis)?;
        let full = u128::from(expected_height) / BLOCKS_PER_PERIOD;
        let remainder = (u128::from(expected_height) % BLOCKS_PER_PERIOD) as u32;
        let mut reward = INITIAL_REWARD;
        let mut last_reward = INITIAL_REWARD;
        for period in 0..full as u32 {
            let s = quote_blocks_with_reward(
                reward,
                BLOCKS_PER_PERIOD as u32,
                canonical.cumulative_issued,
            )?;
            canonical.cumulative_issued += s.issued;
            canonical.base_issued += s.base;
            canonical.ai_issued += s.ai;
            last_reward = reward;
            reward = next_reward(reward, period);
        }
        if remainder > 0 {
            let s = quote_blocks_with_reward(reward, remainder, canonical.cumulative_issued)?;
            canonical.cumulative_issued += s.issued;
            canonical.base_issued += s.base;
            canonical.ai_issued += s.ai;
        }
        canonical.last_height = expected_height;
        if canonical.checkpoint() != checkpoint {
            return Err(Error::InvalidCheckpoint);
        }
        // Derive the private cache from validated height; never trust cached
        // amounts supplied by a caller or add them to the checkpoint format.
        if expected_height > 0 {
            canonical.reward_period =
                ((u128::from(expected_height) - 1) / BLOCKS_PER_PERIOD) as u32;
            canonical.reward_per_block = if remainder > 0 { reward } else { last_reward };
        }
        Ok(canonical)
    }
    /// Genesis is block0 and receives no scheduled block emission.
    pub fn new(genesis_issued: u128) -> Result<Self, Error> {
        if genesis_issued > CAP {
            return Err(Error::SupplyAlreadyExceeded);
        }
        Ok(Self {
            last_height: 0,
            genesis_issued,
            cumulative_issued: genesis_issued,
            base_issued: 0,
            ai_issued: 0,
            reward_period: 0,
            reward_per_block: INITIAL_REWARD,
        })
    }
    pub fn last_height(&self) -> u64 {
        self.last_height
    }
    pub fn cumulative_issued(&self) -> u128 {
        self.cumulative_issued
    }
    pub fn genesis_issued(&self) -> u128 {
        self.genesis_issued
    }
    pub fn base_issued(&self) -> u128 {
        self.base_issued
    }
    pub fn ai_issued(&self) -> u128 {
        self.ai_issued
    }

    /// Exactly the next block is accepted. All fallible arithmetic precedes commit.
    /// No burn or payout method exists: neither should create new issuance headroom.
    /// Reward lookup is constant work during sequential application. This is
    /// not a benchmarked Runtime hook; checkpoint validation remains planning-only.
    pub fn apply_next(&mut self, height: u64) -> Result<Split, Error> {
        let expected = self
            .last_height
            .checked_add(1)
            .ok_or(Error::HeightOverflow)?;
        if height != expected {
            return Err(Error::UnexpectedHeight);
        }
        let period = u32::try_from((height - 1) as u128 / BLOCKS_PER_PERIOD)
            .map_err(|_| Error::InvalidPeriod)?;
        if period >= 10000 {
            return Err(Error::InvalidPeriod);
        }
        let reward = if period == self.reward_period {
            self.reward_per_block
        } else if period.checked_sub(1) == Some(self.reward_period) {
            next_reward(self.reward_per_block, self.reward_period)
        } else {
            return Err(Error::InvalidCheckpoint);
        };
        let split = quote_block(reward, self.cumulative_issued)?;
        let cumulative = self
            .cumulative_issued
            .checked_add(split.issued)
            .ok_or(Error::AccountingOverflow)?;
        let base = self
            .base_issued
            .checked_add(split.base)
            .ok_or(Error::AccountingOverflow)?;
        let ai = self
            .ai_issued
            .checked_add(split.ai)
            .ok_or(Error::AccountingOverflow)?;
        if self
            .genesis_issued
            .checked_add(base)
            .and_then(|n| n.checked_add(ai))
            != Some(cumulative)
        {
            return Err(Error::AccountingOverflow);
        }
        *self = Self {
            last_height: height,
            genesis_issued: self.genesis_issued,
            cumulative_issued: cumulative,
            base_issued: base,
            ai_issued: ai,
            reward_period: period,
            reward_per_block: reward,
        };
        Ok(split)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{period_reward, quote_blocks_in_period, quote_period, INITIAL_REWARD};
    #[test]
    fn streaming_recovery_matches_independent_period_lookup_for_genesis_and_partial_prefixes() {
        for genesis in [0, CAP - 1, CAP] {
            let mut total = genesis;
            let mut base = 0;
            let mut ai = 0;
            for period in 0..=10000u32 {
                if [0, 23, 24, 2978, 9999, 10000].contains(&period) {
                    for remainder in [0, 1, BLOCKS_PER_PERIOD as u32 - 1] {
                        if period == 10000 && remainder > 0 {
                            continue;
                        }
                        let split = if period == 10000 {
                            Split {
                                issued: 0,
                                base: 0,
                                ai: 0,
                            }
                        } else {
                            quote_blocks_in_period(period, remainder, total).unwrap()
                        };
                        let height =
                            u64::from(period) * BLOCKS_PER_PERIOD as u64 + u64::from(remainder);
                        let checkpoint = PlanningCheckpoint {
                            version: 1,
                            last_height: height,
                            genesis_issued: genesis,
                            cumulative_issued: total + split.issued,
                            base_issued: base + split.base,
                            ai_issued: ai + split.ai,
                        };
                        let restored =
                            PlanningLedger::from_checkpoint(checkpoint, height, genesis).unwrap();
                        assert_eq!(restored.checkpoint(), checkpoint);
                        if period < 10000 {
                            let mut next = restored;
                            assert_eq!(
                                next.apply_next(height + 1),
                                Ok(quote_block(
                                    period_reward(period).unwrap(),
                                    checkpoint.cumulative_issued
                                )
                                .unwrap())
                            );
                        }
                    }
                }
                if period < 10000 {
                    let split = quote_period(period, total).unwrap();
                    total += split.issued;
                    base += split.base;
                    ai += split.ai;
                }
            }
        }
    }
    #[test]
    fn restored_cache_matches_reference_across_rate_transition_zero_and_horizon() {
        let mut total = 0;
        let mut base = 0;
        let mut ai = 0;
        for period in 0..10000u32 {
            if [0, 1, 22, 23, 24, 2000, 2977, 2978, 9999].contains(&period) {
                let height = u64::from(period) * BLOCKS_PER_PERIOD as u64;
                let checkpoint = PlanningCheckpoint {
                    version: 1,
                    last_height: height,
                    genesis_issued: 0,
                    cumulative_issued: total,
                    base_issued: base,
                    ai_issued: ai,
                };
                let mut restored = PlanningLedger::from_checkpoint(checkpoint, height, 0).unwrap();
                let expected = quote_block(period_reward(period).unwrap(), total).unwrap();
                assert_eq!(restored.apply_next(height + 1), Ok(expected));
                assert_eq!(restored.reward_period, period);
                assert_eq!(restored.reward_per_block, period_reward(period).unwrap());
                let restored_again =
                    PlanningLedger::from_checkpoint(restored.checkpoint(), height + 1, 0).unwrap();
                assert_eq!(restored_again, restored);
            }
            let split = quote_period(period, total).unwrap();
            total += split.issued;
            base += split.base;
            ai += split.ai;
        }
    }
    #[test]
    fn validated_checkpoint_roundtrip_matches_sequential_period_boundaries() {
        let mut ledger = PlanningLedger::new(321).unwrap();
        assert_eq!(
            PlanningLedger::from_checkpoint(ledger.checkpoint(), 0, 321),
            Ok(ledger)
        );
        for height in 1..=BLOCKS_PER_PERIOD as u64 + 1 {
            ledger.apply_next(height).unwrap();
            if [1, BLOCKS_PER_PERIOD as u64, BLOCKS_PER_PERIOD as u64 + 1].contains(&height) {
                let mut restored =
                    PlanningLedger::from_checkpoint(ledger.checkpoint(), height, 321).unwrap();
                assert_eq!(restored, ledger);
                let mut reference = ledger;
                assert_eq!(
                    restored.apply_next(height + 1),
                    reference.apply_next(height + 1)
                );
                assert_eq!(restored, reference);
            }
        }
    }
    #[test]
    fn balanced_counter_forgery_is_rejected_not_only_inconsistent_totals() {
        let mut ledger = PlanningLedger::new(0).unwrap();
        ledger.apply_next(1).unwrap();
        let checkpoint = ledger.checkpoint();
        let mut forged = checkpoint;
        forged.cumulative_issued -= 1;
        forged.base_issued -= 1;
        assert_eq!(
            PlanningLedger::from_checkpoint(forged, 1, 0),
            Err(Error::InvalidCheckpoint)
        );
        let mut forged = checkpoint;
        forged.base_issued -= 1;
        forged.ai_issued += 1;
        assert_eq!(
            PlanningLedger::from_checkpoint(forged, 1, 0),
            Err(Error::InvalidCheckpoint)
        );
        assert_eq!(ledger.checkpoint(), checkpoint);
    }
    #[test]
    fn checkpoint_pins_version_height_genesis_and_rejects_unbounded_input() {
        let checkpoint = PlanningLedger::new(7).unwrap().checkpoint();
        assert_eq!(
            PlanningLedger::from_checkpoint(checkpoint, 1, 7),
            Err(Error::InvalidCheckpoint)
        );
        assert_eq!(
            PlanningLedger::from_checkpoint(checkpoint, 0, 8),
            Err(Error::InvalidCheckpoint)
        );
        let mut bad = checkpoint;
        bad.version = 2;
        assert_eq!(
            PlanningLedger::from_checkpoint(bad, 0, 7),
            Err(Error::InvalidCheckpoint)
        );
        let mut bad = checkpoint;
        bad.last_height = u64::MAX;
        assert_eq!(
            PlanningLedger::from_checkpoint(bad, u64::MAX, 7),
            Err(Error::InvalidCheckpoint)
        );
        let mut bad = checkpoint;
        bad.cumulative_issued = u128::MAX;
        bad.base_issued = u128::MAX;
        assert_eq!(
            PlanningLedger::from_checkpoint(bad, 0, 7),
            Err(Error::InvalidCheckpoint)
        );
    }
    #[test]
    fn capped_checkpoint_preserves_genesis_and_final_partial_reward() {
        let mut ledger = PlanningLedger::new(CAP - 5).unwrap();
        ledger.apply_next(1).unwrap();
        ledger.apply_next(2).unwrap();
        assert_eq!(
            PlanningLedger::from_checkpoint(ledger.checkpoint(), 2, CAP - 5),
            Ok(ledger)
        );
        assert_eq!(quote_blocks_in_period(0, 0, 0).unwrap().issued, 0);
        assert_eq!(
            quote_blocks_in_period(0, BLOCKS_PER_PERIOD as u32 + 1, 0),
            Err(Error::InvalidPeriod)
        );
    }
    #[test]
    fn duplicate_genesis_and_skipped_heights_do_not_change_state() {
        let mut ledger = PlanningLedger::new(0).unwrap();
        let initial = ledger;
        for height in [0, 2, 100] {
            assert_eq!(ledger.apply_next(height), Err(Error::UnexpectedHeight));
            assert_eq!(ledger, initial);
        }
        assert_eq!(ledger.apply_next(1).unwrap().issued, INITIAL_REWARD);
        let first = ledger;
        for height in [0, 1, 3] {
            assert_eq!(ledger.apply_next(height), Err(Error::UnexpectedHeight));
            assert_eq!(ledger, first);
        }
    }
    #[test]
    fn genesis_allocation_counts_towards_cap_and_zero_rewards_still_advance_height() {
        assert_eq!(
            PlanningLedger::new(CAP + 1),
            Err(Error::SupplyAlreadyExceeded)
        );
        let mut ledger = PlanningLedger::new(CAP - 5).unwrap();
        assert_eq!(
            ledger.apply_next(1),
            Ok(Split {
                issued: 5,
                base: 4,
                ai: 1
            })
        );
        assert_eq!(ledger.cumulative_issued(), CAP);
        assert_eq!(ledger.genesis_issued(), CAP - 5);
        assert_eq!(
            ledger.apply_next(2),
            Ok(Split {
                issued: 0,
                base: 0,
                ai: 0
            })
        );
        assert_eq!(ledger.last_height(), 2);
        assert_eq!(ledger.base_issued() + ledger.ai_issued(), 5);
        assert_eq!(ledger.cumulative_issued(), CAP);
    }
    #[test]
    fn sequential_blocks_match_bulk_quotes_across_actual_period_boundary() {
        let mut ledger = PlanningLedger::new(0).unwrap();
        let first = quote_period(0, 0).unwrap();
        let second = quote_period(1, first.issued).unwrap();
        for height in 1..=2 * BLOCKS_PER_PERIOD as u64 {
            ledger.apply_next(height).unwrap();
        }
        assert_eq!(ledger.cumulative_issued(), first.issued + second.issued);
        assert_eq!(ledger.base_issued(), first.base + second.base);
        assert_eq!(ledger.ai_issued(), first.ai + second.ai);
    }
    #[test]
    fn restoring_a_caller_checkpoint_replays_identical_math_without_double_counting() {
        let mut ledger = PlanningLedger::new(123).unwrap();
        ledger.apply_next(1).unwrap();
        let checkpoint = ledger;
        let block2 = ledger.apply_next(2).unwrap();
        ledger.apply_next(3).unwrap();
        let branch = ledger;
        ledger = checkpoint;
        assert_eq!(ledger.apply_next(2), Ok(block2));
        ledger.apply_next(3).unwrap();
        assert_eq!(ledger, branch);
    }
    #[test]
    fn overflow_and_invalid_period_faults_are_atomic() {
        let mut ledger = PlanningLedger::new(0).unwrap();
        ledger.last_height = u64::MAX;
        let before = ledger;
        assert_eq!(ledger.apply_next(0), Err(Error::HeightOverflow));
        assert_eq!(ledger, before);
        let mut ledger = PlanningLedger::new(0).unwrap();
        ledger.base_issued = u128::MAX;
        let before = ledger;
        assert_eq!(ledger.apply_next(1), Err(Error::AccountingOverflow));
        assert_eq!(ledger, before);
        let mut ledger = PlanningLedger::new(0).unwrap();
        ledger.last_height = 10000 * BLOCKS_PER_PERIOD as u64;
        let before = ledger;
        assert_eq!(
            ledger.apply_next(before.last_height + 1),
            Err(Error::InvalidPeriod)
        );
        assert_eq!(ledger, before);
    }
}
