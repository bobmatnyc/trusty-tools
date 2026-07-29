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

/// Compute USD cost for `model` from prompt/completion token counts.
///
/// Why: (#4098, COST-05) This function used to own a SECOND pricing table —
/// two hardcoded Haiku constants (`$0.25`/`$1.25` per million) applied to
/// every model. The REPL statusline and the persisted `usage.json` daily
/// total both went through it, so a Sonnet session (the actual default, see
/// the provider-policy note in `llm/`) was billed on screen at roughly a
/// twelfth of its real cost. The table is deleted, not corrected: a second
/// rate table is a defect regardless of the numbers in it, because nothing
/// keeps it in step with `perf::pricing`. This is now a thin adapter, kept
/// only so the two statusline call sites and the daily-total writer share one
/// spelling of "cost for this session".
/// What: Delegates to [`crate::perf::cost_usd`] — the single pricing entry
/// point — passing `0` for both cache buckets. The REPL's `TokenUpdate` feed
/// carries only in/out counts today; when #4101 threads cache tokens through
/// `UsageRecord`, this call gains them rather than growing a rate of its own.
/// Test: `cost_from_tokens_prices_sonnet_above_haiku`,
/// `cost_from_tokens_matches_pricing_entry_point`.
pub fn cost_from_tokens(model: &str, prompt: u64, completion: u64) -> f64 {
    // #4098: one pricing table for the whole crate — see perf::pricing's doc.
    crate::perf::cost_usd(model, prompt, completion, 0, 0)
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

    /// #4098 (COST-05) regression guard: the deleted local table priced every
    /// model at Haiku rates, so this assertion could not have held before.
    #[test]
    fn cost_from_tokens_prices_sonnet_above_haiku() {
        let sonnet = cost_from_tokens("anthropic/claude-sonnet-4-6", 1_000_000, 1_000_000);
        let haiku = cost_from_tokens("anthropic/claude-haiku-4", 1_000_000, 1_000_000);
        // Sonnet: $3 + $15 = $18. Haiku: $0.80 + $4 = $4.80.
        assert!((sonnet - 18.0).abs() < 1e-6, "sonnet got {sonnet}");
        assert!((haiku - 4.80).abs() < 1e-6, "haiku got {haiku}");
        assert!(sonnet > haiku, "sonnet must not be billed at haiku rates");
    }

    /// #4098 (COST-05): the statusline path and the pricing table must be the
    /// same number, not two numbers that happen to agree today.
    #[test]
    fn cost_from_tokens_matches_pricing_entry_point() {
        for model in ["anthropic/claude-sonnet-4-6", "claude-haiku-4", "mystery/x"] {
            let via_daily = cost_from_tokens(model, 12_345, 6_789);
            let via_pricing = crate::perf::cost_usd(model, 12_345, 6_789, 0, 0);
            assert!(
                (via_daily - via_pricing).abs() < f64::EPSILON,
                "{model}: daily={via_daily} pricing={via_pricing}"
            );
        }
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
