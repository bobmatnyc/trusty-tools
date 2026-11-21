//! The Architect's context bar: `ctx_bar.py`, the one place that knows the
//! Architect's bar differs from the PMs'.
//!
//! Why: Bob D8f (2026-11-12) raised the bar for the Architect session only;
//! PMs keep 350k. One key, `thresholds.architect_bar_ctx` in
//! `local/il-schedule.json`, drives it; an absent key means the PM bar
//! (df-T7i P1b).
//! What: [`architect_bar`] reads the key (a positive integer, else `None`);
//! [`bar_pct`] is the bar as a whole percent of a window, used as the
//! `self_ctx` threshold when `SELF_CTX_THRESHOLD` is unset.
//! Test: `ctx_bar_matches_python_goldens` (tests/parity_p1b.rs),
//! `bar_pct_of_a_zero_window_is_an_error`.

use std::fs;
use std::path::Path;

use serde_json::Value;

/// The Architect's tmux session, as `ctx_bar.py` names it.
pub const ARCHITECT_SESSION: &str = "tm-architect";
/// Every PM's bar, and the Architect's when the schedule sets none.
pub const PM_BAR: i64 = 350_000;

/// The bar is a percent of a zero window: the prototype raises
/// `ZeroDivisionError` and loses the cycle; the port reports it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, thiserror::Error)]
#[error("context window is 0")]
pub struct ZeroWindow;

/// `architect_bar`: `thresholds.architect_bar_ctx` from the schedule file, or
/// `None` when the file, the key or a positive integer is missing.
pub fn architect_bar(schedule: &Path) -> Option<i64> {
    let text = fs::read_to_string(schedule).ok()?;
    let doc: Value = serde_json::from_str(&text).ok()?;
    // Python's `isinstance(v, int) and not isinstance(v, bool) and v > 0`.
    doc.get("thresholds")?
        .get("architect_bar_ctx")?
        .as_i64()
        .filter(|v| *v > 0)
}

/// `bar_pct(session, window)`: `int(100 * bar / window)`, truncated toward
/// zero like Python's `int()`.
///
/// # Errors
/// [`ZeroWindow`] for a window of 0.
pub fn bar_pct(session: &str, window: i64, schedule: &Path) -> Result<i64, ZeroWindow> {
    if window == 0 {
        return Err(ZeroWindow);
    }
    let bar = if session == ARCHITECT_SESSION {
        architect_bar(schedule).unwrap_or(PM_BAR)
    } else {
        PM_BAR
    };
    Ok((100.0 * bar as f64 / window as f64).trunc() as i64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn bar_pct_of_a_zero_window_is_an_error() {
        assert_eq!(
            bar_pct("tm-x", 0, Path::new("/nonexistent")),
            Err(ZeroWindow)
        );
        assert_eq!(
            bar_pct("tm-x", 1_000_000, Path::new("/nonexistent")),
            Ok(35)
        );
        assert_eq!(
            bar_pct("tm-x", -1_000_000, Path::new("/nonexistent")),
            Ok(-35)
        );
    }
}
