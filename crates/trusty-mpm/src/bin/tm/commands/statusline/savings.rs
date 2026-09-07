//! The `💸` estimated-savings segment for `tm statusline` (#6958).
//!
//! Why: the owner asked to see what the harness saves by not sending tokens —
//! instruction folding today, diverted file reads and compressed gate output as
//! those producers land. The status bar is where the operator already looks for
//! the session's cost, so the saving belongs beside it.
//!
//! What: folds `~/.trusty-mpm/usage/savings.jsonl` for the current session at
//! render time and renders one segment. Two forms, one absence:
//!
//! | Fold | Renders |
//! |---|---|
//! | `>= $0.01` | `💸~$0.42` |
//! | `> $0` but `< $0.01` | `💸~5k tok` |
//! | no ledger, unreadable, or zero | nothing at all |
//!
//! The leading `~` is the estimate marker. It is what keeps this segment
//! visually distinct from the exact `<cost>` segment two positions earlier,
//! which carries Claude Code's own billed total. The two are NOT subtractable —
//! a saving is measured against tokens never sent, not against the session's
//! bill — and the docs page says so.
//!
//! **`$0.00` is unreachable by construction.** A rendered `$0.00` is
//! indistinguishable from "no savings" and states a measurement that was never
//! made, so a sub-cent fold falls back to the token form and an empty fold
//! omits the segment entirely.
//!
//! Test: `savings_segment_renders_dollars_above_a_cent`,
//! `savings_segment_renders_tokens_below_a_cent`,
//! `savings_segment_is_absent_on_a_zero_fold`,
//! `savings_segment_never_renders_zero_dollars`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::savings::{SavingsTotal, fold_session, savings_log_in};

/// The floor below which the dollar form would round to `$0.00`.
///
/// Test: `savings_segment_never_renders_zero_dollars`.
const CENT: f64 = 0.01;

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
/// Why: this is the rule the whole segment exists to get right — never `$0.00`,
/// never a fabricated figure, and a visible estimate marker on everything it
/// does show.
/// What: `💸~$X.XX` at or above one cent; `💸~<N>k tok` / `💸~<N> tok` for a
/// positive sub-cent fold; `None` when [`SavingsTotal::is_zero`].
/// Test: `savings_segment_renders_dollars_above_a_cent`,
/// `savings_segment_renders_tokens_below_a_cent`,
/// `savings_segment_is_absent_on_a_zero_fold`,
/// `savings_segment_never_renders_zero_dollars`.
pub(crate) fn render_savings_segment(total: &SavingsTotal) -> Option<String> {
    if total.is_zero() {
        return None;
    }
    if total.cost_saved_usd >= CENT {
        return Some(format!("\u{1f4b8}~${:.2}", total.cost_saved_usd));
    }
    let tokens = total.tokens_saved;
    if tokens == 0 {
        return None;
    }
    Some(if tokens >= 1_000 {
        format!("\u{1f4b8}~{}k tok", tokens / 1_000)
    } else {
        format!("\u{1f4b8}~{tokens} tok")
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::savings::{SavingsRow, append_row, now_ts};

    fn total(tokens: u64, usd: f64) -> SavingsTotal {
        SavingsTotal {
            tokens_saved: tokens,
            cost_saved_usd: usd,
            rows: 1,
        }
    }

    /// Why: the dollar form is the primary reading, and two decimals with the
    /// `~` estimate marker is the exact shape the docs page describes.
    /// Test: itself.
    #[test]
    fn savings_segment_renders_dollars_above_a_cent() {
        assert_eq!(
            render_savings_segment(&total(140_000, 0.42)).as_deref(),
            Some("\u{1f4b8}~$0.42")
        );
        assert_eq!(
            render_savings_segment(&total(4_000_000, 12.005)).as_deref(),
            Some("\u{1f4b8}~$12.01")
        );
    }

    /// Why (#6958): a sub-cent fold rendered as dollars is `$0.00`, which reads
    /// as "we saved nothing" — the opposite of what the row says. The token
    /// form is approximate but true.
    /// Test: itself.
    #[test]
    fn savings_segment_renders_tokens_below_a_cent() {
        assert_eq!(
            render_savings_segment(&total(5_400, 0.0009)).as_deref(),
            Some("\u{1f4b8}~5k tok")
        );
        assert_eq!(
            render_savings_segment(&total(320, 0.0004)).as_deref(),
            Some("\u{1f4b8}~320 tok")
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
                cost_saved_usd: 0.0,
                rows: 3,
            }),
            None
        );
    }

    /// Why (#6958): the one output this segment may never produce, asserted
    /// directly rather than inferred from the two format tests. A naive
    /// implementation that formatted every fold as `${:.2}` passes both of
    /// those and fails this.
    /// Test: itself.
    #[test]
    fn savings_segment_never_renders_zero_dollars() {
        for (tokens, usd) in [
            (500_u64, 0.0),
            (500, 0.000_001),
            (1, 0.004_9),
            (0, 0.0),
            (12_000, 0.009_9),
        ] {
            let rendered = render_savings_segment(&total(tokens, usd)).unwrap_or_default();
            assert!(
                !rendered.contains("$0.00"),
                "the segment must never render $0.00 (tokens={tokens}, usd={usd}): {rendered:?}"
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
                cost_saved_usd: 0.18,
                basis: "sources 60000 B - compiled 12000 B".to_string(),
            },
        )
        .expect("append");
        assert_eq!(
            savings_segment_at(&ledger, "sess-1").as_deref(),
            Some("\u{1f4b8}~$0.18")
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
