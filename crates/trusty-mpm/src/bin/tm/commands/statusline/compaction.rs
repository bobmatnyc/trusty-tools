//! Per-session compaction tracking for `tm statusline`.
//!
//! Why: Claude Code fires the `statusLine` hook on every render cycle with no
//! persistent state; this module maintains a tiny JSON file in
//! `~/.trusty-mpm/statusline/<session_id>.json` so the statusline can show how
//! much the last auto-compaction shrank the context window.
//! What: persists a `CompactionState` (running peak + last compaction record)
//! keyed by `session_id`. All filesystem I/O runs in a detached thread bounded
//! by a 100 ms wall-clock timeout so it can never stall the render path. Saves
//! are skipped on no-op ticks. Atomic writes use `NamedTempFile::persist`.
//! Test: `humanize_tokens_examples`, `compaction_detection_sequence`,
//! `state_round_trip`, `rejects_path_traversal_session_id` in inline tests.

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::sync::mpsc;
use std::time::Duration;

use serde::{Deserialize, Serialize};
use tempfile::NamedTempFile;

// ── Input types (deserialised from Claude Code stdin) ─────────────────────────

/// Context-window summary provided by Claude Code on every statusLine tick.
///
/// Why: exposes both raw token counts (for precise compaction delta) and the
/// pre-computed percentage (as a fallback when size is unknown).
/// What: all fields default so older Claude Code versions that omit
/// `context_window` entirely still parse without error. Extra JSON fields
/// (e.g. `current_usage`, `total_output_tokens`) are silently ignored because
/// `deny_unknown_fields` is absent.
/// Test: `render_statusline_with_context_window` in mod.rs tests.
#[derive(Debug, Default, Clone, Deserialize)]
pub(crate) struct ContextWindow {
    #[serde(default)]
    pub total_input_tokens: u64,
    #[serde(default)]
    pub context_window_size: u64,
    #[serde(default)]
    pub used_percentage: f64,
}

// ── Persisted state ───────────────────────────────────────────────────────────

/// Session-scoped compaction state serialised to
/// `~/.trusty-mpm/statusline/<session_id>.json`.
///
/// Why: the statusline binary is invoked fresh on every render tick; this state
/// file bridges invocations so the segment can show a compaction that happened
/// several ticks ago.
/// What: tracks the highest `total_input_tokens` seen (`peak_input_tokens`)
/// and the most recent compaction record.
/// Test: `state_round_trip`.
#[derive(Debug, Default, Serialize, Deserialize)]
pub(crate) struct CompactionState {
    #[serde(default)]
    pub peak_input_tokens: u64,
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

// ── Context-usage coloring (#2098) ────────────────────────────────────────────

/// Color tier for the context-usage indicator segment.
///
/// Why (#2098): Claude Code's `statusLine` hook has no persistent UI chrome —
/// the segment text itself is the only way to warn the operator that the
/// session is approaching auto-compaction / the hard context limit. Usage at
/// or above 50 % is close enough to that boundary to deserve visual emphasis.
/// What: a two-tier enum consumed by [`colorize_ctx_segment`]; kept separate
/// from the ANSI-emitting code so the threshold decision itself is trivially
/// unit-testable without depending on the `colored` crate's global override
/// state.
/// Test: `ctx_usage_color_boundaries`, `ctx_usage_color_clamps_out_of_range`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum CtxUsageColor {
    Normal,
    Red,
}

/// Decide the color tier for a context-window usage fraction.
///
/// Why (#2098): centralising the 50 % threshold as a pure function (rather
/// than inlining the comparison at each call site) makes the boundary
/// independently unit-testable and gives future callers (e.g. a "nearly full"
/// yellow tier) one place to extend.
/// What: clamps `used_fraction` to `[0.0, 1.0]` (out-of-range inputs — stale
/// or malformed hook data — degrade to the nearest valid tier rather than
/// panicking or silently misclassifying) and returns [`CtxUsageColor::Red`]
/// when the clamped value is `>= 0.5`, else [`CtxUsageColor::Normal`].
/// Test: `ctx_usage_color_boundaries`, `ctx_usage_color_clamps_out_of_range`.
pub(crate) fn ctx_usage_color(used_fraction: f64) -> CtxUsageColor {
    if used_fraction.clamp(0.0, 1.0) >= 0.5 {
        CtxUsageColor::Red
    } else {
        CtxUsageColor::Normal
    }
}

/// Wrap `text` in a red ANSI foreground escape when `used_fraction` indicates
/// high context-window usage; otherwise return it unchanged.
///
/// Why (#2098): both the live-fill `ctx N%` segment (below) and the bare
/// `ctx>200k` overflow marker (`mod.rs`, used when Claude Code's hook payload
/// omits the full `context_window` object) need the same red-at-50% treatment,
/// so it is centralised here. This builds the ANSI escape directly rather than
/// going through the `colored` crate's `Colorize` trait (as
/// `formatters/services.rs` / `formatters/banner` do) for two reasons: (1)
/// `colored`'s calls gate on its process-global `SHOULD_COLORIZE` override,
/// which `formatters/banner/two_panel.rs`'s `image_shading_emits_truecolor`
/// test comment (issue #1858) documents as racy under parallel `cargo test`
/// threads — that module's fix was exactly this: take the color decision as
/// an explicit, already-resolved input and emit the escape directly; (2) `tm
/// statusline`'s stdout is always a pipe consumed by Claude Code's status-bar
/// renderer, never a real user terminal, so `colored`'s TTY/`NO_COLOR`
/// autodetection is not just irrelevant here but actively wrong — the red
/// marker must render unconditionally whenever usage crosses the threshold.
/// What: wraps `text` as `"\x1b[31m{text}\x1b[0m"` (standard SGR red
/// foreground, matching the color `colored::Colorize::red()` itself emits)
/// when [`ctx_usage_color`] returns `Red`; returns `text` unchanged otherwise.
/// Test: `render_ctx_segment_colors_high_usage_red`,
/// `render_ctx_segment_leaves_low_usage_plain`.
pub(crate) fn colorize_ctx_segment(text: &str, used_fraction: f64) -> String {
    match ctx_usage_color(used_fraction) {
        CtxUsageColor::Red => format!("\u{1b}[31m{text}\u{1b}[0m"),
        CtxUsageColor::Normal => text.to_string(),
    }
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

/// Apply one context-window reading to `CompactionState`; returns `true` when
/// the state was modified and a save is warranted.
///
/// Why: keeping detection logic pure (no I/O) allows exhaustive unit testing
/// of the heuristic without touching the filesystem. The `bool` return avoids
/// unnecessary disk writes on every no-op render tick.
/// What: when `cur == 0` returns `false` immediately. Otherwise detects a
/// compaction if `cur` has dropped below 60 % of `peak` AND the absolute drop
/// exceeds 10 000 tokens; on detection records the event and resets `peak` to
/// `cur` (returns `true`). Advances `peak` when `cur > peak` (returns `true`).
/// Returns `false` when nothing changed.
/// Test: `compaction_detection_sequence`, `zero_cur_returns_false`.
pub(crate) fn update_state(state: &mut CompactionState, cur: u64) -> bool {
    if cur == 0 {
        return false;
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
            return true;
        }
    }

    if cur > state.peak_input_tokens {
        state.peak_input_tokens = cur;
        return true;
    }

    false
}

/// Return `true` when `s` is a safe session identifier: non-empty and composed
/// entirely of ASCII alphanumeric characters, underscores, or hyphens.
///
/// Why: `session_id` is attacker-influenceable (it comes from Claude Code's
/// stdin JSON); allowing arbitrary strings in `state_file_path` would expose a
/// path-traversal vulnerability (`../../.bashrc` or `/tmp/evil`).
/// What: rejects any `s` that contains `/`, `.`, or any other character outside
/// `[A-Za-z0-9_-]`; also rejects the empty string.
/// Test: `rejects_path_traversal_session_id`.
fn is_valid_session_id(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
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

/// Write `state` to `path` atomically using a temp-file-then-rename strategy,
/// creating parent directories as needed; silently discards all errors so a
/// filesystem issue never breaks the statusline.
///
/// Why: a plain `fs::write` is truncate-then-write; an unclean kill between
/// those two operations leaves a partial/zero-byte file that silently resets
/// `peak_input_tokens` and clears `last_compaction` on the next render. The
/// `NamedTempFile::persist` pattern avoids this: the rename(2) syscall is
/// atomic on POSIX, so readers always see either the old or the new file.
/// What: creates a `NamedTempFile` in the same directory as `path` (ensuring
/// same filesystem for `rename`), writes bytes, then renames to `path`. On any
/// error the temp file is cleaned up by `NamedTempFile`'s `Drop` impl.
/// Test: `state_round_trip` (verifies correct round-trip after atomic save).
pub(crate) fn save_state_to(path: &Path, state: &CompactionState) {
    let _ = (|| -> Option<()> {
        let dir = path.parent()?;
        std::fs::create_dir_all(dir).ok()?;
        let bytes = serde_json::to_vec(state).ok()?;
        let mut tmp = NamedTempFile::new_in(dir).ok()?;
        tmp.write_all(&bytes).ok()?;
        // On failure, PersistError::Drop cleans up the temp file.
        let _ = tmp.persist(path);
        Some(())
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

/// Run one compaction-tracking cycle inside a bounded thread and return the
/// displayable segment string, or `None` on timeout / missing inputs.
///
/// Why: `tm statusline` is on Claude Code's hot render path; a stuck NFS
/// mount, full disk, or slow credential helper in `load_state_from` /
/// `save_state_to` would stall every render cycle. The bounded-thread +
/// `recv_timeout(100 ms)` pattern matches `git_branch` in `mod.rs`.
/// What: validates `session_id` against `[A-Za-z0-9_-]+` (rejects empty /
/// path-traversal IDs); then spawns a detached thread that loads state,
/// conditionally updates + saves (skipped when `cur==0` or state unchanged),
/// and sends back the rendered segment. The caller waits ≤100 ms.
/// Test: `graceful_on_missing_fields`, `rejects_path_traversal_session_id`.
pub(crate) fn compaction_segment(
    session_id: &str,
    context_window: Option<&ContextWindow>,
) -> Option<String> {
    if !is_valid_session_id(session_id) {
        return None;
    }
    let cw = context_window?;
    let path = state_file_path(session_id)?;
    let cur = cw.total_input_tokens;
    // Clone only what crosses the thread boundary.
    let cw2 = cw.clone();

    let (tx, rx) = mpsc::channel::<Option<String>>();
    std::thread::spawn(move || {
        let mut state = load_state_from(&path);
        if cur > 0 {
            // update_state returns true only when state actually changed.
            if update_state(&mut state, cur) {
                save_state_to(&path, &state);
            }
        }
        // cur==0 → skip update+save; still read+render so last_compaction shows.
        let _ = tx.send(render_compaction_segment(&state, &cw2));
    });

    rx.recv_timeout(Duration::from_millis(100)).ok().flatten()
}

/// Render the compaction segment string from persisted state and context window.
///
/// Why: separating rendering from I/O enables unit tests that verify output
/// format without a real state file.
/// What: returns `"⤵ {before}→{after} ({pct}%)"` when a compaction record
/// exists (a past event, not current usage — never colored); otherwise
/// `"ctx {pct}%"` from token counts or `used_percentage`, colored red via
/// [`colorize_ctx_segment`] (#2098) when usage is `>= 50%`; `None` when no
/// displayable information is available.
/// Test: `render_segment_after_compaction`, `render_segment_live_fill_*`,
/// `render_ctx_segment_colors_high_usage_red`,
/// `render_ctx_segment_leaves_low_usage_plain`.
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
        let fraction = cw.total_input_tokens as f64 / cw.context_window_size as f64;
        let pct = (fraction * 100.0).round() as u64;
        return Some(colorize_ctx_segment(&format!("ctx {pct}%"), fraction));
    }
    if cw.used_percentage > 0.0 {
        let fraction = cw.used_percentage / 100.0;
        let pct = cw.used_percentage.round() as u64;
        return Some(colorize_ctx_segment(&format!("ctx {pct}%"), fraction));
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
        assert!(
            update_state(&mut state, 50_000),
            "peak advance must return true"
        );
        assert_eq!(state.peak_input_tokens, 50_000);
        assert!(state.last_compaction.is_none());

        assert!(update_state(&mut state, 100_000));
        assert_eq!(state.peak_input_tokens, 100_000);
        assert!(state.last_compaction.is_none());

        assert!(update_state(&mut state, 182_000));
        assert_eq!(state.peak_input_tokens, 182_000);
        assert!(state.last_compaction.is_none());

        // Compaction: drop to 58k (< 60% of 182k = 109.2k, drop = 124k > 10k)
        assert!(
            update_state(&mut state, 58_000),
            "compaction must return true"
        );
        assert!(state.last_compaction.is_some());
        let rec = state.last_compaction.as_ref().unwrap();
        assert_eq!(rec.before, 182_000);
        assert_eq!(rec.after, 58_000);
        assert!(
            (rec.reclaimed_pct - 68.13).abs() < 0.1,
            "reclaimed_pct = {}",
            rec.reclaimed_pct
        );
        assert_eq!(state.peak_input_tokens, 58_000);

        // Resumes growing: compaction record retained, peak advances
        assert!(update_state(&mut state, 60_000));
        assert_eq!(state.peak_input_tokens, 60_000);
        assert!(state.last_compaction.is_some());
    }

    #[test]
    fn small_drop_not_detected_as_compaction() {
        let mut state = CompactionState::default();
        update_state(&mut state, 100_000);
        // Drop only 5 k — below 10 k absolute threshold → no change
        let changed = update_state(&mut state, 95_000);
        assert!(
            !changed,
            "sub-threshold drop must not be detected as compaction"
        );
        assert!(state.last_compaction.is_none());
    }

    #[test]
    fn moderate_drop_ratio_above_threshold_not_detected() {
        let mut state = CompactionState::default();
        update_state(&mut state, 100_000);
        // 70 % of peak → ratio 0.7 > 0.6, not detected
        let changed = update_state(&mut state, 70_000);
        assert!(!changed, "above-ratio drop must not be detected");
        assert!(state.last_compaction.is_none());
    }

    #[test]
    fn zero_cur_returns_false() {
        let mut state = CompactionState::default();
        update_state(&mut state, 100_000);
        let changed = update_state(&mut state, 0); // cur=0 → no-op
        assert!(!changed, "cur=0 must return false (no save needed)");
        assert_eq!(state.peak_input_tokens, 100_000, "peak must be unchanged");
        assert!(state.last_compaction.is_none());
    }

    #[test]
    fn unchanged_state_returns_false() {
        let mut state = CompactionState::default();
        update_state(&mut state, 100_000);
        // Same value as current peak → no change
        let changed = update_state(&mut state, 100_000);
        assert!(!changed, "cur == peak must return false");
        // Slightly below peak but above 60% and below 10k drop → no change
        let changed2 = update_state(&mut state, 95_000);
        assert!(!changed2, "small drop must return false");
    }

    // ── session_id validation ─────────────────────────────────────────────────

    #[test]
    fn valid_session_id_accepted() {
        assert!(is_valid_session_id("abc-123_XYZ"));
        assert!(is_valid_session_id("a"));
        assert!(is_valid_session_id("session-id-with-dashes"));
        assert!(is_valid_session_id("ABC0123456789"));
    }

    #[test]
    fn rejects_path_traversal_session_id() {
        // None of these should pass validation; compaction_segment returns None
        // immediately without any filesystem access.
        for bad in &[
            "../../evil",
            "../evil",
            "/tmp/evil",
            "evil/subdir",
            "evil.json",
            "",
            "has space",
            "has\nnewline",
        ] {
            assert!(
                !is_valid_session_id(bad),
                "expected validation failure for: {bad:?}"
            );
            // Verify no file access: compaction_segment must return None
            let cw = ContextWindow {
                total_input_tokens: 100_000,
                context_window_size: 200_000,
                ..Default::default()
            };
            assert!(
                compaction_segment(bad, Some(&cw)).is_none(),
                "compaction_segment must return None for bad id: {bad:?}"
            );
        }
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

        // Write (atomically) and read back
        let state = CompactionState {
            peak_input_tokens: 182_000,
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

    // ── ctx_usage_color / colorize_ctx_segment (#2098) ───────────────────────

    /// Why (#2098): pins the exact 50% threshold contract that drives the
    /// red-at-high-usage statusline requirement.
    /// Test: itself.
    #[test]
    fn ctx_usage_color_boundaries() {
        assert_eq!(ctx_usage_color(0.0), CtxUsageColor::Normal);
        assert_eq!(ctx_usage_color(0.49), CtxUsageColor::Normal);
        assert_eq!(ctx_usage_color(0.4999), CtxUsageColor::Normal);
        // Exactly 50% must already be Red ("≥ 50%").
        assert_eq!(ctx_usage_color(0.5), CtxUsageColor::Red);
        assert_eq!(ctx_usage_color(0.51), CtxUsageColor::Red);
        assert_eq!(ctx_usage_color(1.0), CtxUsageColor::Red);
    }

    /// Why (#2098): stale/malformed hook data could in principle produce a
    /// negative or >1.0 fraction; both must clamp to a valid tier rather than
    /// panicking or silently misclassifying.
    /// Test: itself.
    #[test]
    fn ctx_usage_color_clamps_out_of_range() {
        assert_eq!(ctx_usage_color(-0.3), CtxUsageColor::Normal);
        assert_eq!(ctx_usage_color(-100.0), CtxUsageColor::Normal);
        assert_eq!(ctx_usage_color(1.5), CtxUsageColor::Red);
        assert_eq!(ctx_usage_color(1_000.0), CtxUsageColor::Red);
    }

    /// Why (#2098): the actual ANSI red escape must appear in the rendered
    /// text at/above the 50% threshold. `colorize_ctx_segment` builds the
    /// escape directly rather than through `colored`'s global override (see
    /// its doc comment re: issue #1858), so this assertion is deterministic
    /// under parallel `cargo test` execution — no override setup/teardown
    /// needed.
    /// Test: itself.
    #[test]
    fn render_ctx_segment_colors_high_usage_red() {
        let seg = colorize_ctx_segment("ctx 62%", 0.62);
        assert!(
            seg.contains('\u{1b}'),
            "expected an ANSI escape in high-usage segment: {seg:?}"
        );
        assert!(
            seg.contains("ctx 62%"),
            "original text must survive the ANSI wrapping: {seg:?}"
        );
    }

    /// Why (#2098): below the 50% threshold the segment must render as plain
    /// text — no stray ANSI codes.
    /// Test: itself.
    #[test]
    fn render_ctx_segment_leaves_low_usage_plain() {
        let seg = colorize_ctx_segment("ctx 30%", 0.30);
        assert_eq!(seg, "ctx 30%", "low usage must not be colorized");
        assert!(!seg.contains('\u{1b}'), "must contain no ANSI escape");
    }

    /// Why (#2098): exercises the same red-at-threshold behaviour through the
    /// full `render_compaction_segment` entry point (live-fill path, no prior
    /// compaction record) rather than only the lower-level helper.
    /// Test: itself.
    #[test]
    fn render_compaction_segment_colors_high_live_fill_red() {
        let state = CompactionState::default();
        // 164 000 / 200 000 = 82% → well above the 50% threshold.
        let cw = ContextWindow {
            total_input_tokens: 164_000,
            context_window_size: 200_000,
            used_percentage: 0.0,
        };
        let seg = render_compaction_segment(&state, &cw).unwrap();
        assert!(seg.contains("ctx 82%"), "got: {seg}");
        assert!(seg.contains('\u{1b}'), "expected ANSI escape: {seg:?}");
    }

    // ── graceful degradation ──────────────────────────────────────────────────

    #[test]
    fn graceful_on_missing_fields() {
        // Invalid session_id → None (no filesystem access)
        assert!(compaction_segment("", None).is_none());
        assert!(compaction_segment("../bad", None).is_none());
        // Valid session_id, no context_window → None (no filesystem access)
        assert!(compaction_segment("valid-session-id", None).is_none());
    }
}
