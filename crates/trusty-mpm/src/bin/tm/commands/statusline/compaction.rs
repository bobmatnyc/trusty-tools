//! Per-session compaction tracking for `tm statusline`.
//!
//! Why: Claude Code fires the `statusLine` hook on every render cycle with no
//! persistent state; this module maintains a tiny JSON file in
//! `~/.trusty-mpm/statusline/<session_id>.json` so the statusline can show how
//! much the last auto-compaction shrank the context window.
//! What: persists a `CompactionState` (running peak + last compaction record)
//! keyed by `session_id`. Pure helper functions (`humanize_tokens`,
//! `update_state`) are side-effect-free and unit-testable without I/O.
//! Test: `humanize_tokens_examples`, `compaction_detection_sequence`,
//! `state_round_trip`, `graceful_on_missing_fields` in the inline test module.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

// ── Input types (deserialised from Claude Code stdin) ─────────────────────────

/// Context-window summary provided by Claude Code on every statusLine tick.
///
/// Why: exposes both raw token counts (for precise compaction delta) and the
/// pre-computed percentage (as a fallback when size is unknown).
/// What: all fields default so older Claude Code versions that omit
/// `context_window` entirely still parse without error. `current_usage` is
/// `None` while a compaction is in flight and `Some` once the next API call
/// completes; we store it as an opaque `serde_json::Value` because we only
/// test for null-vs-non-null.
/// Test: `render_statusline_with_context_window` in mod.rs tests.
#[derive(Debug, Default, Clone, Deserialize)]
pub(crate) struct ContextWindow {
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub context_window_size: u64,
    #[serde(default)]
    pub used_percentage: f64,
    /// `None` during compaction; `Some(...)` once the next API round-trip completes.
    #[serde(default)]
    pub current_usage: Option<serde_json::Value>,
}

// ── Persisted state ───────────────────────────────────────────────────────────

/// Session-scoped compaction state serialised to
/// `~/.trusty-mpm/statusline/<session_id>.json`.
///
/// Why: the statusline binary is invoked fresh on every render tick; this state
/// file bridges invocations so the segment can show a compaction that happened
/// several ticks ago.
/// What: tracks the highest `total_input_tokens` seen (`peak_input_tokens`),
/// whether the previous tick's `current_usage` was null, and the most recent
/// compaction record.
/// Test: `state_round_trip`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct CompactionState {
    #[serde(default)]
    pub peak_input_tokens: u64,
    #[serde(default)]
    pub prev_current_usage_was_null: bool,
    #[serde(default)]
    pub last_compaction: Option<CompactionRecord>,
}

/// Record of a detected context-compaction event.
///
/// Why: persisted so the segment keeps showing the compaction delta even after
/// tokens have risen again above the post-compaction floor.
/// What: `before`/`after` raw token counts and `reclaimed_pct` at detection time.
/// Test: `compaction_detection_sequence`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct CompactionRecord {
    pub before: u64,
    pub after: u64,
    pub reclaimed_pct: f64,
}

// ── Pure helpers ──────────────────────────────────────────────────────────────

/// Humanize a raw token count to a short display string (e.g. 182 000 → "182k").
///
/// Why: raw token counts are too wide for a status bar segment; the humanised
/// form keeps segments compact and readable at a glance.
/// What: formats ≥1 M as `{n}m`, ≥1 k as `{n}k`, smaller as `{n}`.
/// Uses integer floor division (truncation) for consistent compact output.
/// Test: `humanize_tokens_examples`, `humanize_tokens_boundaries`.
pub(crate) fn humanize_tokens(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{}m", n / 1_000_000)
    } else if n >= 1_000 {
        format!("{}k", n / 1_000)
    } else {
        format!("{n}")
    }
}

/// Apply one context-window reading to the mutable `CompactionState`.
///
/// Why: keeping detection logic pure (no I/O) allows exhaustive unit testing
/// of the heuristic without touching the filesystem.
/// What: when `cur > 0`, detects a compaction if `cur` has dropped below
/// 60 % of `peak` AND the absolute drop exceeds 10 000 tokens; on detection,
/// records the event and resets `peak` to `cur`; otherwise advances `peak`.
/// The `current_usage_is_null` flag is stored for the next call.
/// Test: `compaction_detection_sequence`.
pub(crate) fn update_state(state: &mut CompactionState, cur: u64, current_usage_is_null: bool) {
    if cur == 0 {
        // No usable token info this tick; just record the null flag.
        state.prev_current_usage_was_null = current_usage_is_null;
        return;
    }

    let peak = state.peak_input_tokens;

    if peak > 0 {
        let drop = peak.saturating_sub(cur);
        let below_ratio = (cur as f64) < (peak as f64) * 0.6;

        if below_ratio && drop > 10_000 {
            let reclaimed_pct = 100.0 * drop as f64 / peak as f64;
            state.last_compaction = Some(CompactionRecord {
                before: peak,
                after: cur,
                reclaimed_pct,
            });
            state.peak_input_tokens = cur;
            state.prev_current_usage_was_null = current_usage_is_null;
            return;
        }
    }

    if cur > state.peak_input_tokens {
        state.peak_input_tokens = cur;
    }
    state.prev_current_usage_was_null = current_usage_is_null;
}

// ── I/O helpers ───────────────────────────────────────────────────────────────

/// Load `CompactionState` from `path`, returning `Default` on any error.
///
/// Why: the state file may be absent (first run), malformed, or on a slow
/// filesystem; all error paths produce a clean default rather than propagating.
/// What: reads raw bytes, deserialises JSON; any failure → `CompactionState::default()`.
/// Test: `state_round_trip`.
pub(crate) fn load_state_from(path: &Path) -> CompactionState {
    (|| -> Option<CompactionState> {
        let bytes = std::fs::read(path).ok()?;
        serde_json::from_slice(&bytes).ok()
    })()
    .unwrap_or_default()
}

/// Write `state` to `path`, creating parent directories as needed; silently
/// discards all errors so a filesystem issue never breaks the statusline.
///
/// Why: the statusline must never fail or block due to transient I/O problems.
/// What: `create_dir_all` + atomic `write`; every fallible step uses `.ok()`.
/// Test: `state_round_trip` (nested-dir variant).
pub(crate) fn save_state_to(path: &Path, state: &CompactionState) {
    let _ = (|| -> Option<()> {
        if let Some(dir) = path.parent() {
            std::fs::create_dir_all(dir).ok()?;
        }
        let bytes = serde_json::to_vec(state).ok()?;
        std::fs::write(path, bytes).ok()
    })();
}

/// Return the canonical state file path for `session_id`.
///
/// Why: centralises the path formula so no caller duplicates it.
/// What: `~/.trusty-mpm/statusline/<session_id>.json`; `None` when the home
/// directory cannot be determined.
/// Test: used transitively by `compaction_segment`.
pub(crate) fn state_file_path(session_id: &str) -> Option<PathBuf> {
    let home = dirs::home_dir()?;
    Some(
        home.join(".trusty-mpm")
            .join("statusline")
            .join(format!("{session_id}.json")),
    )
}

// ── Public entry point ────────────────────────────────────────────────────────

/// Run one compaction-tracking cycle and return the displayable segment string.
///
/// Why: wraps the load/update/save cycle so `render_statusline` stays free of
/// I/O concerns; every error path degrades gracefully to `None`.
/// What: returns `None` when `session_id` is empty or `context_window` is
/// absent; otherwise persists state, then returns either
/// `"⤵ {before}→{after} ({pct}%)"` (post-compaction) or `"ctx {pct}%"` (live fill).
/// Test: `graceful_on_missing_fields`; integration via `render_statusline`.
pub(crate) fn compaction_segment(
    session_id: &str,
    context_window: Option<&ContextWindow>,
) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    let cw = context_window?;
    let cur = cw.total_input_tokens;
    let path = state_file_path(session_id)?;

    let mut state = load_state_from(&path);
    let usage_is_null = cw.current_usage.is_none();
    update_state(&mut state, cur, usage_is_null);
    save_state_to(&path, &state);

    render_compaction_segment(&state, cw)
}

/// Render the compaction segment string from persisted state and context window.
///
/// Why: separating rendering from I/O enables unit tests that verify output
/// format without a real state file.
/// What: returns `"⤵ {before}→{after} ({pct}%)"` when a compaction record
/// exists; otherwise `"ctx {pct}%"` from token counts or `used_percentage`;
/// `None` when no displayable information is available.
/// Test: `render_segment_after_compaction`, `render_segment_live_fill_*`.
pub(crate) fn render_compaction_segment(
    state: &CompactionState,
    cw: &ContextWindow,
) -> Option<String> {
    if let Some(ref rec) = state.last_compaction {
        let before = humanize_tokens(rec.before);
        let after = humanize_tokens(rec.after);
        let pct = rec.reclaimed_pct.round() as u64;
        return Some(format!("\u{2935} {before}\u{2192}{after} ({pct}%)"));
    }

    if cw.total_input_tokens > 0 && cw.context_window_size > 0 {
        let pct =
            (cw.total_input_tokens as f64 / cw.context_window_size as f64 * 100.0).round() as u64;
        return Some(format!("ctx {pct}%"));
    }
    if cw.used_percentage > 0.0 {
        let pct = cw.used_percentage.round() as u64;
        return Some(format!("ctx {pct}%"));
    }
    None
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;

    // ── humanize_tokens ───────────────────────────────────────────────────────

    #[test]
    fn humanize_tokens_examples() {
        assert_eq!(humanize_tokens(182_000), "182k");
        assert_eq!(humanize_tokens(58_000), "58k");
        assert_eq!(humanize_tokens(1_200_000), "1m");
        assert_eq!(humanize_tokens(999), "999");
        assert_eq!(humanize_tokens(0), "0");
    }

    #[test]
    fn humanize_tokens_boundaries() {
        assert_eq!(humanize_tokens(1_000), "1k");
        assert_eq!(humanize_tokens(1_999), "1k"); // floor truncation
        assert_eq!(humanize_tokens(2_000), "2k");
        assert_eq!(humanize_tokens(999_999), "999k");
        assert_eq!(humanize_tokens(1_000_000), "1m");
        assert_eq!(humanize_tokens(1_500_000), "1m"); // floor truncation
        assert_eq!(humanize_tokens(2_000_000), "2m");
    }

    // ── update_state (compaction detection) ───────────────────────────────────

    #[test]
    fn compaction_detection_sequence() {
        let mut state = CompactionState::default();

        // Rising context: no compaction
        update_state(&mut state, 50_000, false);
        assert_eq!(state.peak_input_tokens, 50_000);
        assert!(state.last_compaction.is_none());

        update_state(&mut state, 100_000, false);
        assert_eq!(state.peak_input_tokens, 100_000);
        assert!(state.last_compaction.is_none());

        update_state(&mut state, 182_000, false);
        assert_eq!(state.peak_input_tokens, 182_000);
        assert!(state.last_compaction.is_none());

        // Compaction: drop to 58k (< 60% of 182k = 109.2k, drop = 124k > 10k)
        update_state(&mut state, 58_000, true);
        assert!(state.last_compaction.is_some());
        let rec = state.last_compaction.as_ref().unwrap();
        assert_eq!(rec.before, 182_000);
        assert_eq!(rec.after, 58_000);
        // reclaimed ≈ 68.13 %
        assert!(
            (rec.reclaimed_pct - 68.13).abs() < 0.1,
            "reclaimed_pct = {}",
            rec.reclaimed_pct
        );
        assert_eq!(state.peak_input_tokens, 58_000);

        // Resumes growing: compaction record retained, peak advances
        update_state(&mut state, 60_000, false);
        assert_eq!(state.peak_input_tokens, 60_000);
        assert!(state.last_compaction.is_some());
    }

    #[test]
    fn small_drop_not_detected_as_compaction() {
        let mut state = CompactionState::default();
        update_state(&mut state, 100_000, false);
        // Drop only 5 k — below 10 k absolute threshold
        update_state(&mut state, 95_000, false);
        assert!(state.last_compaction.is_none());
    }

    #[test]
    fn moderate_drop_ratio_above_threshold_not_detected() {
        let mut state = CompactionState::default();
        update_state(&mut state, 100_000, false);
        // 70 % of peak → ratio 0.7 > 0.6, not detected
        update_state(&mut state, 70_000, false);
        assert!(state.last_compaction.is_none());
    }

    #[test]
    fn zero_cur_skips_peak_update() {
        let mut state = CompactionState::default();
        update_state(&mut state, 100_000, false);
        update_state(&mut state, 0, true); // cur=0 → skip update, record null flag
        assert_eq!(state.peak_input_tokens, 100_000);
        assert!(state.last_compaction.is_none());
        assert!(state.prev_current_usage_was_null);
    }

    // ── State file I/O ────────────────────────────────────────────────────────

    #[test]
    fn state_round_trip() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("state.json");

        // Non-existent file → Default
        let loaded = load_state_from(&path);
        assert_eq!(loaded.peak_input_tokens, 0);
        assert!(loaded.last_compaction.is_none());

        // Write and read back
        let state = CompactionState {
            peak_input_tokens: 182_000,
            prev_current_usage_was_null: false,
            last_compaction: Some(CompactionRecord {
                before: 182_000,
                after: 58_000,
                reclaimed_pct: 68.13,
            }),
        };
        save_state_to(&path, &state);

        let loaded2 = load_state_from(&path);
        assert_eq!(loaded2.peak_input_tokens, 182_000);
        let rec = loaded2.last_compaction.unwrap();
        assert_eq!(rec.before, 182_000);
        assert_eq!(rec.after, 58_000);
        assert!((rec.reclaimed_pct - 68.13).abs() < 0.001);

        // save_state_to creates parent dirs automatically
        let nested = dir.path().join("a").join("b").join("state.json");
        let state2 = CompactionState {
            peak_input_tokens: 42_000,
            ..Default::default()
        };
        save_state_to(&nested, &state2);
        let loaded3 = load_state_from(&nested);
        assert_eq!(loaded3.peak_input_tokens, 42_000);
    }

    // ── render_compaction_segment ─────────────────────────────────────────────

    #[test]
    fn render_segment_after_compaction() {
        let state = CompactionState {
            peak_input_tokens: 58_000,
            prev_current_usage_was_null: false,
            last_compaction: Some(CompactionRecord {
                before: 182_000,
                after: 58_000,
                reclaimed_pct: 68.13,
            }),
        };
        let cw = ContextWindow {
            total_input_tokens: 60_000,
            context_window_size: 200_000,
            used_percentage: 30.0,
            ..Default::default()
        };
        let seg = render_compaction_segment(&state, &cw).unwrap();
        assert!(seg.contains("182k"), "got: {seg}");
        assert!(seg.contains("58k"), "got: {seg}");
        assert!(seg.contains("68%"), "got: {seg}");
        assert!(seg.contains('\u{2935}'), "must contain ⤵: {seg}");
    }

    #[test]
    fn render_segment_live_fill_from_tokens() {
        let state = CompactionState::default();
        // 82 000 / 200 000 = 41.0 % → "ctx 41%"
        let cw = ContextWindow {
            total_input_tokens: 82_000,
            context_window_size: 200_000,
            used_percentage: 20.0, // ignored when raw counts are available
            ..Default::default()
        };
        let seg = render_compaction_segment(&state, &cw).unwrap();
        assert_eq!(seg, "ctx 41%", "got: {seg}");
    }

    #[test]
    fn render_segment_live_fill_fallback_to_percentage() {
        let state = CompactionState::default();
        let cw = ContextWindow {
            total_input_tokens: 0,
            context_window_size: 0,
            used_percentage: 41.0,
            ..Default::default()
        };
        let seg = render_compaction_segment(&state, &cw).unwrap();
        assert_eq!(seg, "ctx 41%");
    }

    #[test]
    fn render_segment_none_when_no_info() {
        let state = CompactionState::default();
        let cw = ContextWindow::default(); // all zeros
        assert!(render_compaction_segment(&state, &cw).is_none());
    }

    // ── graceful degradation ──────────────────────────────────────────────────

    #[test]
    fn graceful_on_missing_fields() {
        // Empty session_id → None (no filesystem access)
        assert!(compaction_segment("", None).is_none());
        // No context_window → None (no filesystem access)
        assert!(compaction_segment("some-session-id", None).is_none());
    }
}
