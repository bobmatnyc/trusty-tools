//! Sliding-window history truncation.
//!
//! Why: Long-lived agents accumulate dozens of conversation turns; sending
//! all of them on every API call wastes tokens. A simple sliding window
//! (with optional pinned turn 0) keeps recent context while bounding cost.
//! What: `TokenBudget` configures the window, `truncate_history` applies it.
//! Test: See module-level `tests` — covers empty, under/at/over budget,
//! pinned and non-pinned modes.

/// Sliding-window budget for `truncate_history`.
#[derive(Debug, Clone)]
pub struct TokenBudget {
    /// Maximum number of turns to keep in the wire copy.
    pub max_turns: usize,
    /// If true, the turn at index 0 is always retained even when the
    /// remaining window is full.
    pub pin_turn_zero: bool,
}

impl Default for TokenBudget {
    fn default() -> Self {
        Self {
            max_turns: 12,
            pin_turn_zero: true,
        }
    }
}

/// Truncate `turns` so at most `budget.max_turns` items remain.
///
/// Why: Bound the wire-cost of long-lived sessions without mutating the
/// stored history. The pin-zero behavior preserves the original task framing
/// (turn 0 typically contains the user's first request).
/// What: When `pin_turn_zero`, index 0 is always retained; the remaining
/// `max_turns - 1` slots are filled with the most recent turns. When
/// `pin_turn_zero` is false, simply keeps the last `max_turns` turns.
/// Returns a fresh Vec; never panics; never returns more than `max_turns`.
/// Test: `truncate_*` cases in `tests` module.
pub fn truncate_history<T: Clone>(turns: &[T], budget: &TokenBudget) -> Vec<T> {
    let n = turns.len();
    if n == 0 || budget.max_turns == 0 {
        // max_turns == 0 with pin_turn_zero is an edge case: pinned turn 0
        // would exceed the budget; we choose to honor max_turns and return empty.
        if budget.max_turns == 0 {
            return Vec::new();
        }
        return Vec::new();
    }
    if n <= budget.max_turns {
        return turns.to_vec();
    }

    if budget.pin_turn_zero {
        if budget.max_turns == 1 {
            return vec![turns[0].clone()];
        }
        // Keep turn 0 + the last (max_turns - 1) turns.
        let tail_count = budget.max_turns - 1;
        let tail_start = n - tail_count;
        let mut out: Vec<T> = Vec::with_capacity(budget.max_turns);
        out.push(turns[0].clone());
        // Avoid duplicating turn 0 if it would be in the tail window.
        let effective_start = tail_start.max(1);
        out.extend(turns[effective_start..n].iter().cloned());
        out
    } else {
        let start = n - budget.max_turns;
        turns[start..n].to_vec()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn truncate_empty_returns_empty() {
        let v: Vec<i32> = Vec::new();
        let out = truncate_history(&v, &TokenBudget::default());
        assert!(out.is_empty());
    }

    #[test]
    fn truncate_under_budget_unchanged() {
        let v: Vec<i32> = (0..5).collect();
        let out = truncate_history(&v, &TokenBudget::default());
        assert_eq!(out, v);
    }

    #[test]
    fn truncate_exactly_at_budget_unchanged() {
        let v: Vec<i32> = (0..12).collect();
        let out = truncate_history(&v, &TokenBudget::default());
        assert_eq!(out, v);
    }

    #[test]
    fn truncate_over_budget_drops_oldest() {
        let v: Vec<i32> = (0..20).collect();
        let budget = TokenBudget {
            max_turns: 5,
            pin_turn_zero: false,
        };
        let out = truncate_history(&v, &budget);
        assert_eq!(out, vec![15, 16, 17, 18, 19]);
    }

    #[test]
    fn truncate_pinned_zero_always_kept() {
        let v: Vec<i32> = (0..100).collect();
        let budget = TokenBudget {
            max_turns: 5,
            pin_turn_zero: true,
        };
        let out = truncate_history(&v, &budget);
        assert_eq!(out[0], 0);
    }

    #[test]
    fn truncate_pinned_zero_with_overflow() {
        let v: Vec<i32> = (0..20).collect();
        let budget = TokenBudget {
            max_turns: 5,
            pin_turn_zero: true,
        };
        let out = truncate_history(&v, &budget);
        // turn 0 + last 4 turns (16..20)
        assert_eq!(out, vec![0, 16, 17, 18, 19]);
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn truncate_no_pin_zero() {
        let v: Vec<i32> = (0..10).collect();
        let budget = TokenBudget {
            max_turns: 3,
            pin_turn_zero: false,
        };
        let out = truncate_history(&v, &budget);
        assert_eq!(out, vec![7, 8, 9]);
    }

    #[test]
    fn truncate_single_turn_always_kept() {
        let v = vec![42_i32];
        let out = truncate_history(&v, &TokenBudget::default());
        assert_eq!(out, vec![42]);
    }

    #[test]
    fn truncate_max_turns_zero_with_pin() {
        let v: Vec<i32> = (0..5).collect();
        let budget = TokenBudget {
            max_turns: 0,
            pin_turn_zero: true,
        };
        let out = truncate_history(&v, &budget);
        // Honor budget over pin: 0 turns means 0 turns.
        assert!(out.is_empty());
    }

    #[test]
    fn truncate_budget_default_is_12() {
        let b = TokenBudget::default();
        assert_eq!(b.max_turns, 12);
        assert!(b.pin_turn_zero);
    }

    #[test]
    fn truncate_never_exceeds_max_turns() {
        let v: Vec<i32> = (0..1000).collect();
        let budget = TokenBudget {
            max_turns: 7,
            pin_turn_zero: true,
        };
        let out = truncate_history(&v, &budget);
        assert!(out.len() <= 7);
    }

    #[test]
    fn truncate_max_turns_one_with_pin_keeps_zero() {
        let v: Vec<i32> = (0..50).collect();
        let budget = TokenBudget {
            max_turns: 1,
            pin_turn_zero: true,
        };
        let out = truncate_history(&v, &budget);
        assert_eq!(out, vec![0]);
    }
}
