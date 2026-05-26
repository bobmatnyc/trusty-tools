//! Per-session token budget enforcement.
//!
//! Why: MemoryConfig tracks current window fraction; TokenBudget tracks
//! cumulative tokens spent across the whole session lifetime, enabling
//! absolute-spend limits independent of window size.
//! What: TokenBudget (limit + spent counter), BudgetStatus (ok/warn/exceeded).
//! Test: `cargo test -p trusty-mpm-core budget` exercises accumulation,
//! status transitions, and reset.

/// Fraction of the limit at which a budget enters the `Warning` state.
const WARNING_FRACTION: f64 = 0.80;

/// Hard token limit for one session (0 = unlimited).
#[derive(Debug, Clone, Copy, serde::Serialize, serde::Deserialize)]
pub struct TokenBudget {
    /// Hard limit in tokens; 0 means unlimited.
    pub limit: u64,
    /// Total tokens spent so far in this session.
    pub spent: u64,
}

impl TokenBudget {
    /// Construct an unlimited budget (no enforcement).
    ///
    /// Why: sessions without an explicit cap still need a budget value.
    /// What: `limit = 0` (the sentinel for unlimited), `spent = 0`.
    /// Test: `unlimited_budget_never_warns`.
    pub fn unlimited() -> Self {
        Self { limit: 0, spent: 0 }
    }

    /// Construct a budget with a hard token `limit`.
    ///
    /// Why: callers that want absolute-spend enforcement start here.
    /// What: stores `limit`, zeroes `spent`.
    /// Test: `budget_warns_at_80_pct`, `budget_exceeded_at_limit`.
    pub fn with_limit(limit: u64) -> Self {
        Self { limit, spent: 0 }
    }

    /// Add `tokens` to the cumulative spend counter.
    ///
    /// Why: each `TokenUsageUpdate` hook reports an increment.
    /// What: saturating add so a pathological huge increment cannot overflow.
    /// Test: `budget_remaining_tracks_spend`.
    pub fn record(&mut self, tokens: u64) {
        self.spent = self.spent.saturating_add(tokens);
    }

    /// Reset the spend counter to zero (keeps the limit).
    ///
    /// Why: a compaction or new top-level turn may restart accounting.
    /// What: zeroes `spent`.
    /// Test: `budget_reset_clears_spent`.
    pub fn reset(&mut self) {
        self.spent = 0;
    }

    /// Classify the current spend against the limit.
    ///
    /// Why: the daemon decides whether to warn the user or block further work.
    /// What: `Ok` below 80%, `Warning` in `[80%, 100%)`, `Exceeded` at/above
    /// the limit; always `Ok` for an unlimited budget.
    /// Test: `budget_warns_at_80_pct`, `budget_exceeded_at_limit`.
    pub fn status(&self) -> BudgetStatus {
        if self.limit == 0 {
            return BudgetStatus::Ok;
        }
        if self.spent >= self.limit {
            BudgetStatus::Exceeded
        } else if (self.spent as f64) >= (self.limit as f64) * WARNING_FRACTION {
            BudgetStatus::Warning
        } else {
            BudgetStatus::Ok
        }
    }

    /// Tokens remaining before the limit is hit.
    ///
    /// Why: dashboards display headroom; `None` signals an unlimited budget.
    /// What: `Some(limit - spent)` saturating at 0, or `None` if unlimited.
    /// Test: `budget_remaining_tracks_spend`, `unlimited_budget_never_warns`.
    pub fn remaining(&self) -> Option<u64> {
        if self.limit == 0 {
            None
        } else {
            Some(self.limit.saturating_sub(self.spent))
        }
    }
}

/// Spend status of a [`TokenBudget`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub enum BudgetStatus {
    /// Spend is comfortably below the limit.
    Ok,
    /// Spend has reached the 80% warning threshold.
    Warning,
    /// Spend has met or exceeded the hard limit.
    Exceeded,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unlimited_budget_never_warns() {
        let mut b = TokenBudget::unlimited();
        b.record(1_000_000_000);
        assert_eq!(b.status(), BudgetStatus::Ok);
        assert_eq!(b.remaining(), None);
    }

    #[test]
    fn budget_warns_at_80_pct() {
        let mut b = TokenBudget::with_limit(1000);
        assert_eq!(b.status(), BudgetStatus::Ok);
        b.record(799);
        assert_eq!(b.status(), BudgetStatus::Ok);
        b.record(1); // now at 800 = 80%
        assert_eq!(b.status(), BudgetStatus::Warning);
    }

    #[test]
    fn budget_exceeded_at_limit() {
        let mut b = TokenBudget::with_limit(1000);
        b.record(999);
        assert_eq!(b.status(), BudgetStatus::Warning);
        b.record(1); // exactly at limit
        assert_eq!(b.status(), BudgetStatus::Exceeded);
        b.record(500); // well past
        assert_eq!(b.status(), BudgetStatus::Exceeded);
    }

    #[test]
    fn budget_remaining_tracks_spend() {
        let mut b = TokenBudget::with_limit(1000);
        assert_eq!(b.remaining(), Some(1000));
        b.record(300);
        assert_eq!(b.remaining(), Some(700));
        b.record(900); // overspend saturates at 0
        assert_eq!(b.remaining(), Some(0));
    }

    #[test]
    fn budget_reset_clears_spent() {
        let mut b = TokenBudget::with_limit(1000);
        b.record(950);
        assert_eq!(b.status(), BudgetStatus::Warning);
        b.reset();
        assert_eq!(b.spent, 0);
        assert_eq!(b.status(), BudgetStatus::Ok);
        assert_eq!(b.remaining(), Some(1000));
    }
}
