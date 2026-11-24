//! Versioned deterministic Gregorian issuance arithmetic for the isolated
//! rewards development runtime. Only the first 24 months are defined.

pub const UNIT: u128 = 1_000_000_000_000;
pub const CAP: u128 = 1_000_000_000 * UNIT;
pub const YEAR_MIN: u16 = 2000;
pub const YEAR_MAX: u16 = 2400;
const SECONDS_PER_DAY: u64 = 86_400;

/// A Gregorian date at exactly 00:00:00 UTC; no timezone or subsecond coercion.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UtcDate {
    pub year: u16,
    pub month: u8,
    pub day: u8,
}
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Error {
    InvalidDate,
    UnsupportedYear,
    NotMonthStart,
    UnsupportedGenesis,
    BeforeStart,
    NeedsPolicy,
    Overflow,
    NonMonotonic,
    IssuedAboveTarget,
}
pub const fn is_leap_year(year: u16) -> bool {
    year % 4 == 0 && (year % 100 != 0 || year % 400 == 0)
}
fn month_days(year: u16, month: u8) -> Result<u8, Error> {
    match month {
        1 | 3 | 5 | 7 | 8 | 10 | 12 => Ok(31),
        4 | 6 | 9 | 11 => Ok(30),
        2 => Ok(if is_leap_year(year) { 29 } else { 28 }),
        _ => Err(Error::InvalidDate),
    }
}
fn validate(date: UtcDate) -> Result<(), Error> {
    if date.year < YEAR_MIN || date.year > YEAR_MAX {
        return Err(Error::UnsupportedYear);
    }
    if date.day == 0 || date.day > month_days(date.year, date.month)? {
        return Err(Error::InvalidDate);
    }
    Ok(())
}
fn leaps_before(year: u64) -> Result<u64, Error> {
    let previous = year.checked_sub(1).ok_or(Error::Overflow)?;
    (previous / 4)
        .checked_sub(previous / 100)
        .and_then(|v| v.checked_add(previous / 400))
        .ok_or(Error::Overflow)
}
/// Integer Unix epoch seconds for midnight UTC. Gregorian leap centuries are
/// handled explicitly: 2000/2400 leap, 2100/2200/2300 not leap. Supported dates are
/// 2000-01-01 through 2400-12-31; unsupported dates never saturate or wrap.
pub fn epoch_seconds(date: UtcDate) -> Result<u64, Error> {
    validate(date)?;
    let year = u64::from(date.year);
    let years = year.checked_sub(1970).ok_or(Error::Overflow)?;
    let leaps = leaps_before(year)?
        .checked_sub(leaps_before(1970)?)
        .ok_or(Error::Overflow)?;
    let mut days = years
        .checked_mul(365)
        .and_then(|v| v.checked_add(leaps))
        .ok_or(Error::Overflow)?;
    for month in 1..date.month {
        days = days
            .checked_add(u64::from(month_days(date.year, month)?))
            .ok_or(Error::Overflow)?;
    }
    days = days
        .checked_add(
            u64::from(date.day)
                .checked_sub(1)
                .ok_or(Error::InvalidDate)?,
        )
        .ok_or(Error::Overflow)?;
    days.checked_mul(SECONDS_PER_DAY).ok_or(Error::Overflow)
}
pub const RATE_SCALE: u128 = 1_000_000_000;
pub const RETENTION_NUM: u128 = 995;
pub const RETENTION_DEN: u128 = 1000;

/// Exact average monthly ceiling rate in smallest token units per second.
/// Keep the fraction: integer division would conceal rate increases.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MonthlyRate {
    pub numerator: u128,
    pub seconds: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateEmission {
    boundaries: [u64; 25],
    budgets: [u128; 24],
}

fn allocate_year(seconds: &[u64], budget: u128) -> Result<[u128; 12], Error> {
    if seconds.len() != 12 || seconds.contains(&0) {
        return Err(Error::NonMonotonic);
    }
    let mut weighted = [0u128; 12];
    let mut weight = RATE_SCALE;
    let mut sum = 0u128;
    for i in 0..12 {
        if i > 0 {
            let rate_limit =
                weight.checked_mul(RETENTION_NUM).ok_or(Error::Overflow)? / RETENTION_DEN;
            let total_limit = weight
                .checked_mul(u128::from(seconds[i - 1]))
                .and_then(|v| v.checked_mul(RETENTION_NUM))
                .ok_or(Error::Overflow)?
                / u128::from(seconds[i])
                    .checked_mul(RETENTION_DEN)
                    .ok_or(Error::Overflow)?;
            weight = rate_limit.min(total_limit);
        }
        if weight == 0 {
            return Err(Error::NonMonotonic);
        }
        weighted[i] = weight
            .checked_mul(u128::from(seconds[i]))
            .ok_or(Error::Overflow)?;
        sum = sum.checked_add(weighted[i]).ok_or(Error::Overflow)?;
    }
    let mut out = [0; 12];
    let mut prefix = 0u128;
    let mut previous = 0u128;
    for i in 0..12 {
        prefix = prefix.checked_add(weighted[i]).ok_or(Error::Overflow)?;
        let target = budget.checked_mul(prefix).ok_or(Error::Overflow)? / sum;
        out[i] = target.checked_sub(previous).ok_or(Error::NonMonotonic)?;
        previous = target;
    }
    if previous != budget {
        return Err(Error::Overflow);
    }
    Ok(out)
}

impl RateEmission {
    pub fn new(start: UtcDate, genesis_issued: u128) -> Result<Self, Error> {
        if genesis_issued >= CAP {
            return Err(Error::UnsupportedGenesis);
        }
        let remaining = CAP.checked_sub(genesis_issued).ok_or(Error::Overflow)?;
        epoch_seconds(start)?;
        if start.day != 1 {
            return Err(Error::NotMonthStart);
        }
        let month_index = u32::from(start.month)
            .checked_sub(1)
            .ok_or(Error::InvalidDate)?;
        let absolute = u32::from(start.year)
            .checked_mul(12)
            .and_then(|v| v.checked_add(month_index))
            .ok_or(Error::Overflow)?;
        let mut boundaries = [0u64; 25];
        for (i, boundary) in boundaries.iter_mut().enumerate() {
            let month = absolute
                .checked_add(u32::try_from(i).map_err(|_| Error::Overflow)?)
                .ok_or(Error::Overflow)?;
            *boundary = epoch_seconds(UtcDate {
                year: u16::try_from(month / 12).map_err(|_| Error::UnsupportedYear)?,
                month: u8::try_from(month % 12 + 1).map_err(|_| Error::Overflow)?,
                day: 1,
            })?;
        }
        let mut seconds = [0u64; 24];
        for i in 0..24 {
            seconds[i] = boundaries[i + 1]
                .checked_sub(boundaries[i])
                .ok_or(Error::NonMonotonic)?;
        }
        let first = remaining.checked_mul(15).ok_or(Error::Overflow)? / 100;
        let total = remaining.checked_mul(25).ok_or(Error::Overflow)? / 100;
        let second = total.checked_sub(first).ok_or(Error::Overflow)?;
        let mut budgets = [0u128; 24];
        budgets[..12].copy_from_slice(&allocate_year(&seconds[..12], first)?);
        budgets[12..].copy_from_slice(&allocate_year(&seconds[12..], second)?);
        // Validate the real rounded output across ALL boundaries, including the
        // independently normalized year boundary. No floating point or integer
        // division comparison is permitted here.
        for i in 1..24 {
            if budgets[i] >= budgets[i - 1] {
                return Err(Error::NonMonotonic);
            }
            let next = budgets[i]
                .checked_mul(u128::from(seconds[i - 1]))
                .ok_or(Error::Overflow)?;
            let previous = budgets[i - 1]
                .checked_mul(u128::from(seconds[i]))
                .ok_or(Error::Overflow)?;
            if next >= previous {
                return Err(Error::NonMonotonic);
            }
        }
        let sum = |slice: &[u128]| {
            slice
                .iter()
                .try_fold(0u128, |a, b| a.checked_add(*b))
                .ok_or(Error::Overflow)
        };
        if sum(&budgets[..12])? != first || sum(&budgets)? != total || total > CAP {
            return Err(Error::Overflow);
        }
        Ok(Self {
            boundaries,
            budgets,
        })
    }
    pub fn month_bounds(&self, index: u8) -> Result<(u64, u64), Error> {
        if index >= 24 {
            return Err(Error::NeedsPolicy);
        }
        let i = usize::from(index);
        Ok((self.boundaries[i], self.boundaries[i + 1]))
    }
    pub fn monthly_budget(&self, index: u8) -> Result<u128, Error> {
        self.budgets
            .get(usize::from(index))
            .copied()
            .ok_or(Error::NeedsPolicy)
    }
    pub fn monthly_rate(&self, index: u8) -> Result<MonthlyRate, Error> {
        let (a, b) = self.month_bounds(index)?;
        Ok(MonthlyRate {
            numerator: self.monthly_budget(index)?,
            seconds: b.checked_sub(a).ok_or(Error::NonMonotonic)?,
        })
    }
    pub fn target_cumulative(&self, timestamp: u64) -> Result<u128, Error> {
        if timestamp < self.boundaries[0] {
            return Err(Error::BeforeStart);
        }
        if timestamp > self.boundaries[24] {
            return Err(Error::NeedsPolicy);
        }
        let mut prior = 0u128;
        for i in 0..24 {
            if timestamp >= self.boundaries[i + 1] {
                prior = prior.checked_add(self.budgets[i]).ok_or(Error::Overflow)?;
            } else {
                let elapsed = timestamp
                    .checked_sub(self.boundaries[i])
                    .ok_or(Error::BeforeStart)?;
                let duration = self.boundaries[i + 1]
                    .checked_sub(self.boundaries[i])
                    .ok_or(Error::NonMonotonic)?;
                let partial = self.budgets[i]
                    .checked_mul(u128::from(elapsed))
                    .ok_or(Error::Overflow)?
                    / u128::from(duration);
                return prior.checked_add(partial).ok_or(Error::Overflow);
            }
        }
        Ok(prior)
    }
    /// Interval math only; not a persistent claim, payout or replay fence.
    pub fn delta_between(&self, previous_timestamp: u64, timestamp: u64) -> Result<u128, Error> {
        if timestamp < previous_timestamp {
            return Err(Error::NonMonotonic);
        }
        self.target_cumulative(timestamp)?
            .checked_sub(self.target_cumulative(previous_timestamp)?)
            .ok_or(Error::NonMonotonic)
    }
    /// Caller must supply a genuine gross counter, NOT circulating stock. This
    /// function cannot detect false input or atomically persist the returned delta.
    pub fn quote_delta(
        &self,
        timestamp: u64,
        last_timestamp: u64,
        gross_already_issued: u128,
    ) -> Result<u128, Error> {
        if timestamp < last_timestamp {
            return Err(Error::NonMonotonic);
        }
        let previous_target = self.target_cumulative(last_timestamp)?;
        let target = self.target_cumulative(timestamp)?;
        // Future headroom cannot repair an already-invalid historical counter.
        // Underclaims remain valid; caller authentication/storage are separate.
        if gross_already_issued > previous_target {
            return Err(Error::IssuedAboveTarget);
        }
        target
            .checked_sub(gross_already_issued)
            .ok_or(Error::IssuedAboveTarget)
    }
}

#[cfg(test)]
mod promoted_tests {
    use super::*;

    fn plan() -> RateEmission {
        RateEmission::new(
            UtcDate {
                year: 2026,
                month: 11,
                day: 1,
            },
            0,
        )
        .unwrap()
    }

    #[test]
    fn fixed_start_and_exact_endpoints_fail_closed_after_month_24() {
        assert_eq!(
            epoch_seconds(UtcDate {
                year: 2026,
                month: 11,
                day: 1
            }),
            Ok(1_793_491_200)
        );
        let plan = plan();
        assert_eq!(
            plan.target_cumulative(plan.month_bounds(11).unwrap().1),
            Ok(CAP * 15 / 100)
        );
        let end = plan.month_bounds(23).unwrap().1;
        assert_eq!(plan.target_cumulative(end), Ok(CAP * 25 / 100));
        assert_eq!(plan.target_cumulative(end + 1), Err(Error::NeedsPolicy));
    }

    #[test]
    fn genesis_budget_and_dual_monotonicity_are_enforced() {
        assert_eq!(
            RateEmission::new(
                UtcDate {
                    year: 2026,
                    month: 11,
                    day: 1
                },
                CAP
            ),
            Err(Error::UnsupportedGenesis)
        );
        let genesis = CAP * 5 / 100;
        let remaining = CAP - genesis;
        let funded = RateEmission::new(
            UtcDate {
                year: 2026,
                month: 11,
                day: 1,
            },
            genesis,
        )
        .unwrap();
        assert_eq!(
            funded.target_cumulative(funded.month_bounds(11).unwrap().1),
            Ok(remaining * 15 / 100)
        );
        assert_eq!(
            funded.target_cumulative(funded.month_bounds(23).unwrap().1),
            Ok(remaining * 25 / 100)
        );
        assert!(
            genesis
                + funded
                    .target_cumulative(funded.month_bounds(23).unwrap().1)
                    .unwrap()
                <= CAP
        );
        let plan = plan();
        for index in 1..24 {
            let previous = plan.monthly_rate(index - 1).unwrap();
            let current = plan.monthly_rate(index).unwrap();
            assert!(current.numerator < previous.numerator);
            assert!(
                current.numerator * u128::from(previous.seconds)
                    < previous.numerator * u128::from(current.seconds)
            );
        }
    }
}
