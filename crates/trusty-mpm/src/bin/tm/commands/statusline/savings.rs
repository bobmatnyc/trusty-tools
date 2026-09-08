//! The `💸` estimated-savings segment for `tm statusline` (#6958, percent
//! form since #7179).
//!
//! Why: the owner asked to see what the harness saves by not sending tokens —
//! instruction folding today, diverted file reads and compressed gate output as
//! those producers land. The status bar is where the operator already looks for
//! the session's cost, so the saving belongs beside it. #7179 (owner ruling
//! 2026-09-08) replaced the dollar/token figure with a percentage: "how much of
//! what would have been sent did we avoid" reads at a glance in a way a dollar
//! amount — which needs the session's spend as context to interpret — does not.
//!
//! What: folds `~/.trusty-mpm/usage/savings.jsonl` for the current session at
//! render time and renders one segment:
//!
//! | Fold | Renders |
//! |---|---|
//! | `tokens_saved > 0` and a percent denominator exists | `💸34%` |
//! | zero fold, or no denominator (every row predates #7179) | nothing at all |
//!
//! The percent is [`SavingsTotal::percent_saved`] — `tokens_saved / (this
//! session's ledger rows' own pre-saving token count)`, folded across every
//! accepted row, rounded to the nearest whole number. See that method's doc and
//! the `savings` module header for why this denominator was chosen over the
//! session's live context-window fill.
//!
//! **`0%` is unreachable by construction**, the same way `$0.00` was before
//! #7179: [`SavingsTotal::is_zero`] gates the whole segment, so a fold with
//! nothing to show renders nothing rather than a false `0%`.
//!
//! Test: `savings_segment_renders_a_percent`,
//! `savings_segment_is_absent_on_a_zero_fold`,
//! `savings_segment_never_renders_zero_percent`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::savings::{SavingsTotal, fold_session, savings_log_in};

/// Fold the ledger for `session_id` and render the segment, or omit it.
///
/// Why: the probe half is separated from [`render_savings_segment`] so the
/// render rules are unit-testable against hand-built totals, with no filesystem
/// and no resolved framework root.
/// What: resolves the ledger under the operator's framework root — the same
/// `--root` / `TRUSTY_MPM_ROOT` / XDG-config / `~/.trusty-mpm` chain every other
/// `tm` command honours — folds it for this session, and renders. An empty
/// `session_id` (Claude Code sends one only once the session has an id) omits
/// the segment without touching the disk.
/// Test: `savings_segment_probe_is_absent_without_a_session_id`,
/// `savings_segment_reads_the_ledger_under_an_explicit_root`.
pub(crate) fn savings_segment_probe(session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    let root = savings_root()?;
    savings_segment_at(&savings_log_in(&root), session_id)
}

/// [`savings_segment_probe`] against an explicit ledger path.
///
/// Why: makes the missing-ledger and populated-ledger branches assertable end
/// to end from a temp directory, with no environment mutation.
/// What: folds `ledger` for `session_id` and renders the result.
/// Test: `savings_segment_is_absent_when_the_ledger_is_missing`,
/// `savings_segment_reads_the_ledger_under_an_explicit_root`.
pub(crate) fn savings_segment_at(ledger: &Path, session_id: &str) -> Option<String> {
    render_savings_segment(&fold_session(ledger, session_id))
}

/// Remember which model this session runs, for the divert producer to price at.
///
/// Why (#6972): `tm divert` runs in its own process and has no way to learn the
/// parent session's model — Claude Code exports no model variable to a hook
/// child, and the PreToolUse payload carries no model field. The `statusLine`
/// payload is the ONE place the authoritative `model.id` reaches `tm`, so the
/// render that already reads it is what persists it. Before this, every
/// diversion priced at the config chain's Sonnet default: an Opus session
/// under-reported its savings by five times, and three smoke diversions wrote no
/// row at all because the Haiku worker's bill exceeded the understated delta.
/// What: writes `model_id` under the same framework root the ledger uses, and
/// only when it changed — the store does the comparison, so a steady session
/// costs one small read per render. `model.display_name` is deliberately not a
/// fallback: the price table matches on slugs (`claude-opus-…`), and a bare
/// "Opus" would not price. The two early returns exist to skip resolving the
/// root at all before Claude Code has assigned a session id.
/// Test: the store's own suite — `a_recorded_model_reads_back`,
/// `an_unchanged_model_leaves_the_file_untouched`,
/// `a_blank_model_is_never_recorded`.
pub(crate) fn record_parent_model(session_id: &str, model_id: &str) {
    if session_id.is_empty() || model_id.trim().is_empty() {
        return;
    }
    let Some(root) = savings_root() else {
        return;
    };
    trusty_mpm::core::session_model::record_session_model(&root, session_id, model_id);
}

/// Resolve the framework root the ledger lives under.
///
/// Why: `tm` lets an operator relocate the whole framework root, and a status
/// bar reading a different root from the producers would silently show nothing.
/// Routing through the existing resolver rather than `FrameworkPaths::default()`
/// is what keeps the two in agreement.
/// What: [`crate::commands::managed_root::resolve_managed_paths`] with no
/// `--root` flag; `None` when the root cannot be resolved at all (a stripped
/// environment with no home directory), which omits the segment.
/// Test: covered through `savings_segment_probe`; the resolver has its own
/// precedence tests (`test_resolve_env_wins_over_config`).
fn savings_root() -> Option<PathBuf> {
    crate::commands::managed_root::resolve_managed_paths(None)
        .ok()
        .map(|paths| paths.root)
}

/// Render a folded total as the segment text, or `None` to omit it.
///
/// Why: this is the rule the whole segment exists to get right — never a false
/// `0%`, never a fabricated figure.
/// What: `💸<N>%` from [`SavingsTotal::percent_saved`]; `None` on
/// [`SavingsTotal::is_zero`] or when that method itself returns `None` (no
/// percent denominator — every accepted row predates #7179's `tokens_before`).
/// Test: `savings_segment_renders_a_percent`,
/// `savings_segment_is_absent_on_a_zero_fold`,
/// `savings_segment_never_renders_zero_percent`.
pub(crate) fn render_savings_segment(total: &SavingsTotal) -> Option<String> {
    if total.is_zero() {
        return None;
    }
    total.percent_saved().map(|pct| format!("\u{1f4b8}{pct}%"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::savings::{SavingsRow, append_row, now_ts};

    fn total(tokens_saved: u64, tokens_before: u64) -> SavingsTotal {
        SavingsTotal {
            tokens_saved,
            tokens_before,
            cost_saved_usd: 0.01,
            rows: 1,
        }
    }

    /// Why (#7179): the percent is the primary — now only — reading, and
    /// rounding to the nearest whole number is the exact shape the docs page
    /// describes.
    /// Test: itself.
    #[test]
    fn savings_segment_renders_a_percent() {
        assert_eq!(
            render_savings_segment(&total(1, 3)).as_deref(),
            Some("\u{1f4b8}33%")
        );
        assert_eq!(
            render_savings_segment(&total(5_000, 20_000)).as_deref(),
            Some("\u{1f4b8}25%")
        );
    }

    /// Why: a zero fold — no rows at all, or rows that summed to nothing — must
    /// omit the segment, not render a placeholder.
    /// Test: itself.
    #[test]
    fn savings_segment_is_absent_on_a_zero_fold() {
        assert_eq!(render_savings_segment(&SavingsTotal::default()), None);
        assert_eq!(
            render_savings_segment(&SavingsTotal {
                tokens_saved: 0,
                tokens_before: 0,
                cost_saved_usd: 0.0,
                rows: 3,
            }),
            None
        );
    }

    /// Why (#7179): a fold with `tokens_saved > 0` but no percent denominator
    /// (every accepted row predates #7179) must omit the segment, not fabricate
    /// a percent against nothing.
    /// Test: itself.
    #[test]
    fn savings_segment_is_absent_without_a_percent_denominator() {
        assert_eq!(
            render_savings_segment(&SavingsTotal {
                tokens_saved: 4_000,
                tokens_before: 0,
                cost_saved_usd: 0.01,
                rows: 1,
            }),
            None
        );
    }

    /// Why (#7179): the one output this segment may never produce, asserted
    /// directly rather than inferred from the format test. A naive
    /// implementation that let the ratio exceed 1.0 (a mixed old/new ledger,
    /// see [`SavingsTotal::percent_saved`]) would print `💸0%` on the wrong
    /// side or a percent above 100 without the clamp this pins.
    /// Test: itself.
    #[test]
    fn savings_segment_never_renders_zero_percent() {
        for (tokens_saved, tokens_before) in [(1_u64, 200), (12_000, 12_000_100), (5, 1_000)] {
            let rendered =
                render_savings_segment(&total(tokens_saved, tokens_before)).unwrap_or_default();
            assert_ne!(
                rendered, "\u{1f4b8}0%",
                "the segment must never render 0% while tokens_saved > 0 \
                 (tokens_saved={tokens_saved}, tokens_before={tokens_before})"
            );
        }
    }

    /// Why (#6958): the ledger is normally absent — no producer has run — and
    /// that must cost the status bar nothing and render nothing.
    /// Test: itself.
    #[test]
    fn savings_segment_is_absent_when_the_ledger_is_missing() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ledger = dir.path().join("usage").join("savings.jsonl");
        assert_eq!(savings_segment_at(&ledger, "sess-1"), None);
    }

    /// Why: proves the whole path — append a row, fold it back for that session
    /// id, and render — without touching the operator's real root.
    /// Test: itself.
    #[test]
    fn savings_segment_reads_the_ledger_under_an_explicit_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ledger = savings_log_in(dir.path());
        append_row(
            &ledger,
            &SavingsRow {
                ts: now_ts(),
                session_id: "sess-1".to_string(),
                technique: "instruction-compression".to_string(),
                tokens_saved: 12_000,
                tokens_before: 60_000,
                cost_saved_usd: 0.18,
                basis: "sources 60000 B - compiled 12000 B".to_string(),
                model_source: "launch-config".to_string(),
            },
        )
        .expect("append");
        assert_eq!(
            savings_segment_at(&ledger, "sess-1").as_deref(),
            Some("\u{1f4b8}20%")
        );
        // A different session's bar reads nothing from the same file.
        assert_eq!(savings_segment_at(&ledger, "sess-2"), None);
    }

    /// Why: before Claude Code assigns a session id there is nothing to fold,
    /// and the probe must not read the disk to discover that.
    /// Test: itself.
    #[test]
    fn savings_segment_probe_is_absent_without_a_session_id() {
        assert_eq!(savings_segment_probe(""), None);
    }
}
