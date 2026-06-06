//! Daily cost accumulator persisted at `.trusty-agents/state/usage.json`.
//!
//! Why: The statusline already shows a session cost (`$0.0002`), but users
//! want to know what they've spent across an entire day — sessions are short,
//! days are not. Persisting a small JSON blob keyed by local date lets the
//! REPL surface a "today" total that survives `/clear` and process restarts,
//! and resets cleanly when the calendar rolls over.
//! What: `DailyUsage` is the on-disk shape; `load` reads & date-checks the
//! file, `save_atomic` writes via `.tmp` + rename. Both helpers are best-
//! effort: a missing file or stale date silently yields zeroed totals.
//! Test: see `tests` — round-trip serialization, rollover-on-new-day, and
//! atomic-write semantics.
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// Haiku pricing — kept here so both the live statusline and the persisted
/// daily total agree on the rate. Mirrors the constants in
/// `src/repl/tui.rs::format_cost_chunk`.
pub const PROMPT_RATE: f64 = 0.00000025;
pub const COMPLETION_RATE: f64 = 0.00000125;

/// Compute USD cost from token counts using the haiku rate table.
///
/// Why: Centralized so daily aggregation never drifts from the per-session
/// number rendered on the statusline.
/// What: prompt * $0.00000025 + completion * $0.00000125.
/// Test: `cost_from_tokens_matches_published_rates`.
pub fn cost_from_tokens(prompt: u64, completion: u64) -> f64 {
    (prompt as f64) * PROMPT_RATE + (completion as f64) * COMPLETION_RATE
}

/// Persisted shape of `.trusty-agents/state/usage.json`.
///
/// Why: Flat struct serialises to the exact JSON the spec calls for and is
/// trivial to inspect with `jq`.
/// What: `date` is local-date YYYY-MM-DD; the three numeric fields are the
/// running daily totals.
/// Test: `daily_usage_serializes_round_trip`.
#[derive(Debug, Clone, Serialize, Deserialize, PartialEq)]
pub struct DailyUsage {
    pub date: String,
    pub prompt_tokens: u64,
    pub completion_tokens: u64,
    pub cost_usd: f64,
}

impl DailyUsage {
    /// Build an empty record dated today.
    pub fn empty_today() -> Self {
        Self {
            date: today_local(),
            prompt_tokens: 0,
            completion_tokens: 0,
            cost_usd: 0.0,
        }
    }
}

/// Today's date as `YYYY-MM-DD` in the local timezone.
///
/// Why: The "daily" rollover is a human concept — local midnight, not UTC.
/// What: `chrono::Local::now().format("%Y-%m-%d")`.
/// Test: Indirectly via `load_returns_today_with_zero_when_missing`.
pub fn today_local() -> String {
    chrono::Local::now().format("%Y-%m-%d").to_string()
}

/// Resolve `<project_dir>/.trusty-agents/state/usage.json`.
pub fn usage_path(project_dir: &Path) -> PathBuf {
    project_dir
        .join(".trusty-agents")
        .join("state")
        .join("usage.json")
}

/// Load the persisted daily usage, or return a zeroed record dated today
/// when the file is missing, malformed, or refers to a previous day.
///
/// Why: Treat any failure as "fresh day" so a corrupt file can never leak a
/// stale cost into the statusline.
/// What: Reads `<project_dir>/.trusty-agents/state/usage.json`, parses it, and
/// returns the record verbatim only when `date == today_local()`. Otherwise
/// returns `DailyUsage::empty_today()`.
/// Test: `load_returns_today_with_zero_when_missing`,
/// `load_resets_when_date_differs`, `load_returns_record_when_date_matches`.
pub fn load(project_dir: &Path) -> DailyUsage {
    let path = usage_path(project_dir);
    let bytes = match std::fs::read(&path) {
        Ok(b) => b,
        Err(_) => return DailyUsage::empty_today(),
    };
    let parsed: DailyUsage = match serde_json::from_slice(&bytes) {
        Ok(v) => v,
        Err(_) => return DailyUsage::empty_today(),
    };
    if parsed.date == today_local() {
        parsed
    } else {
        DailyUsage::empty_today()
    }
}

/// Atomically write `record` to `<project_dir>/.trusty-agents/state/usage.json`.
///
/// Why: Daily totals are written on every TokenUpdate; a crash mid-write
/// would corrupt the file and lose the day's running cost. Tmp-file +
/// rename guarantees readers always see a complete JSON document.
/// What: `mkdir -p` the state dir, serialize with `serde_json`, write to
/// `usage.json.tmp`, then `rename` over the final path. Returns any I/O
/// error so the caller can decide whether to log it (the REPL throttle
/// loop logs at debug and continues).
/// Test: `save_atomic_creates_file`, `save_atomic_overwrites`.
pub fn save_atomic(project_dir: &Path, record: &DailyUsage) -> std::io::Result<()> {
    let state_dir = project_dir.join(".trusty-agents").join("state");
    std::fs::create_dir_all(&state_dir)?;
    let final_path = state_dir.join("usage.json");
    let tmp_path = state_dir.join("usage.json.tmp");
    let bytes = serde_json::to_vec_pretty(record)
        .map_err(|e| std::io::Error::new(std::io::ErrorKind::InvalidData, e))?;
    std::fs::write(&tmp_path, &bytes)?;
    std::fs::rename(&tmp_path, &final_path)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cost_from_tokens_matches_published_rates() {
        // 1000 prompt + 1000 completion = 0.00025 + 0.00125 = 0.0015
        let c = cost_from_tokens(1000, 1000);
        assert!((c - 0.0015).abs() < 1e-9, "got {c}");
    }

    #[test]
    fn daily_usage_serializes_round_trip() {
        let r = DailyUsage {
            date: "2026-05-03".to_string(),
            prompt_tokens: 12400,
            completion_tokens: 8700,
            cost_usd: 0.0142,
        };
        let s = serde_json::to_string(&r).unwrap();
        let back: DailyUsage = serde_json::from_str(&s).unwrap();
        assert_eq!(r, back);
    }

    #[test]
    fn load_returns_today_with_zero_when_missing() {
        let dir = tempfile::tempdir().unwrap();
        let r = load(dir.path());
        assert_eq!(r.date, today_local());
        assert_eq!(r.prompt_tokens, 0);
        assert_eq!(r.completion_tokens, 0);
        assert_eq!(r.cost_usd, 0.0);
    }

    #[test]
    fn load_resets_when_date_differs() {
        let dir = tempfile::tempdir().unwrap();
        let stale = DailyUsage {
            date: "1999-01-01".to_string(),
            prompt_tokens: 999,
            completion_tokens: 999,
            cost_usd: 9.99,
        };
        save_atomic(dir.path(), &stale).unwrap();
        let r = load(dir.path());
        assert_eq!(r.date, today_local());
        assert_eq!(r.prompt_tokens, 0);
        assert_eq!(r.cost_usd, 0.0);
    }

    #[test]
    fn load_returns_record_when_date_matches() {
        let dir = tempfile::tempdir().unwrap();
        let fresh = DailyUsage {
            date: today_local(),
            prompt_tokens: 100,
            completion_tokens: 50,
            cost_usd: 0.001,
        };
        save_atomic(dir.path(), &fresh).unwrap();
        let r = load(dir.path());
        assert_eq!(r, fresh);
    }

    #[test]
    fn save_atomic_creates_file() {
        let dir = tempfile::tempdir().unwrap();
        let r = DailyUsage::empty_today();
        save_atomic(dir.path(), &r).unwrap();
        assert!(usage_path(dir.path()).exists());
    }

    #[test]
    fn save_atomic_overwrites() {
        let dir = tempfile::tempdir().unwrap();
        let mut r = DailyUsage::empty_today();
        save_atomic(dir.path(), &r).unwrap();
        r.prompt_tokens = 42;
        r.cost_usd = 0.5;
        save_atomic(dir.path(), &r).unwrap();
        let back = load(dir.path());
        assert_eq!(back.prompt_tokens, 42);
        assert!((back.cost_usd - 0.5).abs() < 1e-9);
        // Tmp file should be cleaned up by the rename.
        assert!(
            !dir.path()
                .join(".trusty-agents/state/usage.json.tmp")
                .exists()
        );
    }
}
