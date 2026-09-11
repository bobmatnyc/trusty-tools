//! The `💸` estimated-savings segment for `tm statusline` (#6958, percent
//! form since #7179, per-session average beside it since #7074).
//!
//! Why: the owner asked to see what the harness saves by not sending tokens —
//! instruction folding today, diverted file reads and compressed gate output as
//! those producers land. The status bar is where the operator already looks for
//! the session's cost, so the saving belongs beside it. #7179 replaced the
//! dollar/token figure with a percentage, then the owner ruled the percentage
//! itself must be a *session share* — `saved / (session actual tokens +
//! saved)` — rather than a ratio scoped only to the ledger's own rows: "how
//! much of everything this session sent did we avoid" is the number that
//! actually answers "how much are we saving", where a ledger-only ratio only
//! answers it for the subset of tokens a savings technique happened to touch.
//! #7074 then added the average, because one session's percentage says nothing
//! about whether the harness is saving anything in general.
//!
//! What: folds `~/.trusty-mpm/usage/savings.jsonl` for the current session,
//! reads that session's cumulative actual-token count from
//! [`compaction::session_actual_tokens_for`] (the same per-session store
//! `compaction.rs` already keys by `session_id` — no third store), folds the
//! same ledger once more grouped by session for the average, and renders one
//! segment:
//!
//! | Fold | Renders |
//! |---|---|
//! | `tokens_saved > 0`, a percent denominator, and other sessions on the ledger | `💸34%/29%` |
//! | the same, but the ledger holds only this session | `💸34%/34%` |
//! | zero fold, or no denominator at all (every row predates #7179 and no compaction tick has landed) | nothing at all |
//!
//! The average is the arithmetic mean of each session's OWN percentage
//! ([`average_percent_saved`]), which is the shape the owner ruled for on
//! 2026-09-09: percentages do not sum into a lifetime total but do average
//! cleanly. Both figures are derived at read time from the one ledger — this
//! segment writes nothing, and adds no accumulator file of its own.
//!
//! **`0%` is unreachable by construction**, the same way `$0.00` was before
//! #7179: [`SavingsTotal::is_zero`] gates the whole segment, so a fold with
//! nothing to show renders nothing rather than a false `0%`, and the average is
//! never rendered on its own.
//!
//! Test: `savings_segment_renders_a_percent`,
//! `savings_segment_renders_the_average_beside_the_session_figure`,
//! `savings_segment_is_absent_on_a_zero_fold`,
//! `savings_segment_never_renders_zero_percent`,
//! `rendering_writes_nothing_under_the_usage_directory`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::savings::{
    SavingsTotal, average_percent_saved, fold_session, fold_sessions, savings_log_in,
};

use super::compaction;

/// Fold the ledger for `session_id` and render the segment, or omit it.
///
/// Why: the probe half is separated from [`render_savings_segment`] so the
/// render rules are unit-testable against hand-built totals, with no filesystem
/// and no resolved framework root.
/// What: resolves the ledger under the operator's framework root — the same
/// `--root` / `TRUSTY_MPM_ROOT` / XDG-config / `~/.trusty-mpm` chain every other
/// `tm` command honours — folds it for this session and for every session, and
/// renders. Each session's actual-token count comes from
/// [`compaction::session_actual_tokens_for`] (#7179), which is why the lookup
/// is passed as a closure rather than a single reading: the average needs one
/// per session, not this session's applied to all of them. An empty
/// `session_id` (Claude Code sends one only once the session has an id) omits
/// the segment without touching the disk.
/// Test: `savings_segment_probe_is_absent_without_a_session_id`,
/// `savings_segment_reads_the_ledger_under_an_explicit_root`.
pub(crate) fn savings_segment_probe(session_id: &str) -> Option<String> {
    if session_id.is_empty() {
        return None;
    }
    let root = savings_root()?;
    savings_segment_at(&savings_log_in(&root), session_id, |id| {
        compaction::session_actual_tokens_for(id)
    })
}

/// [`savings_segment_probe`] against an explicit ledger path.
///
/// Why: makes the missing-ledger and populated-ledger branches assertable end
/// to end from a temp directory, with no environment mutation. Taking the
/// actual-tokens lookup as a parameter (rather than reading the compaction
/// state files itself) keeps this function's own I/O to the one ledger path its
/// tests already control.
/// What: folds `ledger` for `session_id`, folds it again grouped by session for
/// the average (#7074), and renders. `actual_tokens` answers, per session id,
/// that session's cumulative actual-token count, or `None` when no compaction
/// tick has landed for it yet (#7179).
/// Test: `savings_segment_is_absent_when_the_ledger_is_missing`,
/// `savings_segment_reads_the_ledger_under_an_explicit_root`,
/// `savings_segment_renders_the_average_beside_the_session_figure`.
pub(crate) fn savings_segment_at(
    ledger: &Path,
    session_id: &str,
    actual_tokens: impl Fn(&str) -> Option<u64>,
) -> Option<String> {
    let total = fold_session(ledger, session_id);
    if total.is_zero() {
        // Criterion 3 (#7074): a session with no savings rows shows neither
        // figure, so the second fold is not even worth doing.
        return None;
    }
    let average = average_percent_saved(&fold_sessions(ledger), &actual_tokens);
    render_savings_segment(&total, actual_tokens(session_id), average)
}

/// Remember what only the `statusLine` payload knows: the session's model and
/// the path to its own transcript.
///
/// Why (#6972): `tm divert` runs in its own process and has no way to learn the
/// parent session's model — Claude Code exports no model variable to a hook
/// child, and the PreToolUse payload carries no model field. The `statusLine`
/// payload is the ONE place the authoritative `model.id` reaches `tm`, so the
/// render that already reads it is what persists it. Before this, every
/// diversion priced at the config chain's Sonnet default: an Opus session
/// under-reported its savings by five times, and three smoke diversions wrote no
/// row at all because the Haiku worker's bill exceeded the understated delta.
/// Since #7074 the same record also names the model in the commit footer, and
/// `transcript_path` joins it: the transcript is the only place a session's
/// output-token count exists, and `tm commit-trailers` runs in a different
/// process that is never handed the path.
/// What: writes both values under the same framework root the ledger uses, and
/// only when they changed — the store does the comparison, so a steady session
/// costs two small reads per render and no write. `model.display_name` is
/// deliberately not a fallback: the price table matches on slugs
/// (`claude-opus-…`), and a bare "Opus" would not price. An absent value is
/// skipped rather than written blank, so a payload that omits one field cannot
/// erase a good record. The transcript path is screened against the session's
/// own Claude config directory first (#7250); a path outside it is dropped and
/// the model is still recorded.
/// Test: the store's own suite — `a_recorded_value_reads_back`,
/// `an_unchanged_value_leaves_the_file_untouched`,
/// `a_blank_value_is_never_recorded`, `two_kinds_do_not_collide`; and
/// `a_transcript_path_outside_the_config_dir_is_not_recorded`,
/// `a_traversing_transcript_path_is_not_recorded`,
/// `a_transcript_path_is_not_recorded_without_a_config_dir`,
/// `a_transcript_path_under_the_config_dir_is_recorded`,
/// `the_first_render_records_the_startup_context`.
///
/// #7424 adds a third fact on the same footing: the session's turn-1 startup
/// context, folded from the transcript this function has just screened. `cwd`
/// joins the signature because that reading is only usable scoped to a project
/// — see [`trusty_mpm::core::startup_context`] — and the `statusLine` payload
/// is the one place `tm` learns the session's working directory alongside its
/// id.
pub(crate) fn record_session_facts(
    session_id: &str,
    model_id: &str,
    transcript_path: &str,
    cwd: &str,
) {
    if session_id.is_empty() {
        return;
    }
    if model_id.trim().is_empty() && transcript_path.trim().is_empty() {
        return;
    }
    let Some(root) = savings_root() else {
        return;
    };
    // #7278: the boundary resolver moved to `core::session_record` beside the
    // screen it feeds, so the parking detector and the pm-guard cost evaluator
    // compare against the same directory this recorder does.
    record_session_facts_at(
        &root,
        trusty_mpm::core::session_record::claude_config_dir().as_deref(),
        session_id,
        model_id,
        transcript_path,
        cwd,
    );
}

/// [`record_session_facts`] against explicit roots.
///
/// Why: both directories are resolved from the process environment by the
/// caller above, and this bin target may not write `CLAUDE_CONFIG_DIR` or
/// `HOME` to steer them (#5544) — so the screening rule is only assertable if
/// the two paths arrive as arguments.
/// What: records the model unconditionally, and the transcript path only when
/// [`trusty_mpm::core::session_record::contained_transcript_path`] accepts it
/// against `claude_config_dir`. The value stored is that function's canonical
/// path, so the reader opens the file that was screened. A `None`
/// `claude_config_dir` records no transcript path: with no directory to
/// contain it there is nothing to screen against, and the model record — which
/// carries no path — is unaffected.
/// Test: `a_transcript_path_outside_the_config_dir_is_not_recorded`,
/// `a_traversing_transcript_path_is_not_recorded`,
/// `a_transcript_path_is_not_recorded_without_a_config_dir`,
/// `a_transcript_path_under_the_config_dir_is_recorded`,
/// `the_first_render_records_the_startup_context`.
fn record_session_facts_at(
    root: &Path,
    claude_config_dir: Option<&Path>,
    session_id: &str,
    model_id: &str,
    transcript_path: &str,
    cwd: &str,
) {
    trusty_mpm::core::session_model::record_session_model(root, session_id, model_id);
    let Some(config_dir) = claude_config_dir else {
        tracing::debug!("no Claude config directory resolves; transcript_path not recorded");
        return;
    };
    let Some(transcript) =
        trusty_mpm::core::session_record::contained_transcript_path(config_dir, transcript_path)
    else {
        return;
    };
    trusty_mpm::core::session_record::record_session_value(
        root,
        trusty_mpm::core::session_record::KIND_TRANSCRIPT,
        session_id,
        &transcript.to_string_lossy(),
    );
    // #7424: the screened transcript is also where the session's turn-1
    // startup context is read from, once. `record_startup_context` returns
    // early when a reading already exists, so a steady session costs one small
    // record read per render and no transcript scan at all.
    let cwd = cwd.trim();
    if !cwd.is_empty() {
        let _ = trusty_mpm::core::startup_context::record_startup_context(
            root,
            session_id,
            Path::new(cwd),
            &transcript,
        );
    }
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
/// What: `💸<N>%` from [`SavingsTotal::percent_saved`] against
/// `session_actual_tokens` (#7179's session-share denominator, or its
/// pre-ruling `tokens_before` fallback on `None`), followed by `/<M>%`
/// when an average exists (#7074, slash form since the owner's 2026-09-10
/// ruling). `None` on [`SavingsTotal::is_zero`] or when
/// that method itself returns `None` (no denominator on either path) — and in
/// that case the average is not rendered on its own, because a session with no
/// figure of its own shows neither (criterion 3).
/// Test: `savings_segment_renders_a_percent`,
/// `savings_segment_renders_the_average_beside_the_session_figure`,
/// `savings_segment_is_absent_on_a_zero_fold`,
/// `savings_segment_never_renders_zero_percent`.
pub(crate) fn render_savings_segment(
    total: &SavingsTotal,
    session_actual_tokens: Option<u64>,
    average_percent: Option<u32>,
) -> Option<String> {
    if total.is_zero() {
        return None;
    }
    let pct = total.percent_saved(session_actual_tokens)?;
    Some(match average_percent {
        // Owner ruling 2026-09-10: session percent, slash, average percent.
        Some(avg) => format!("\u{1f4b8}{pct}%/{avg}%"),
        None => format!("\u{1f4b8}{pct}%"),
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use trusty_mpm::core::savings::{SavingsRow, append_row, now_ts};
    use trusty_mpm::core::session_record::{KIND_MODEL, KIND_TRANSCRIPT, read_session_record};

    fn total(tokens_saved: u64, tokens_before: u64) -> SavingsTotal {
        SavingsTotal {
            tokens_saved,
            tokens_before,
            cost_saved_usd: 0.01,
            rows: 1,
        }
    }

    fn write_row(ledger: &Path, session_id: &str, tokens_saved: i64, tokens_before: u64) {
        append_row(
            ledger,
            &SavingsRow {
                ts: now_ts(),
                session_id: session_id.to_string(),
                technique: "instruction-compression".to_string(),
                tokens_saved,
                tokens_before,
                cost_saved_usd: 0.18,
                basis: "fixture".to_string(),
                model_source: "launch-config".to_string(),
            },
        )
        .expect("append");
    }

    /// Why (#7179): with no actual-tokens reading supplied, the segment falls
    /// back to the pre-ruling ledger-only formula — pinned here so a
    /// regression in the fallback path is caught independently of the
    /// session-share path below.
    /// Test: itself.
    #[test]
    fn savings_segment_renders_a_percent() {
        assert_eq!(
            render_savings_segment(&total(1, 3), None, None).as_deref(),
            Some("\u{1f4b8}33%")
        );
        assert_eq!(
            render_savings_segment(&total(5_000, 20_000), None, None).as_deref(),
            Some("\u{1f4b8}25%")
        );
    }

    /// Why (#7074): the average renders beside the session's own figure, not
    /// instead of it. A render that dropped either half would still look like a
    /// savings segment.
    /// Test: itself.
    #[test]
    fn savings_segment_renders_the_average_beside_the_session_figure() {
        assert_eq!(
            render_savings_segment(&total(5_000, 20_000), None, Some(29)).as_deref(),
            Some("\u{1f4b8}25%/29%")
        );
    }

    /// Why (#7179, owner ruling): once a compaction tick has landed for this
    /// session, the segment must use the session-share denominator —
    /// `saved / (actual + saved)` — even when `tokens_before` disagrees.
    /// Test: itself.
    #[test]
    fn savings_segment_uses_the_session_actual_denominator_when_available() {
        assert_eq!(
            render_savings_segment(&total(40_000, 999_999), Some(160_000), None).as_deref(),
            Some("\u{1f4b8}20%")
        );
    }

    /// Why: a zero fold — no rows at all, or rows that summed to nothing — must
    /// omit the segment, not render a placeholder.
    /// Test: itself.
    #[test]
    fn savings_segment_is_absent_on_a_zero_fold() {
        assert_eq!(
            render_savings_segment(&SavingsTotal::default(), None, None),
            None
        );
        assert_eq!(
            render_savings_segment(
                &SavingsTotal {
                    tokens_saved: 0,
                    tokens_before: 0,
                    cost_saved_usd: 0.0,
                    rows: 3,
                },
                None,
                Some(40),
            ),
            None,
            "an average must never render on its own"
        );
    }

    /// Why (#7179): a fold with `tokens_saved > 0` but no denominator on
    /// either path (no actual-tokens reading, and every accepted row predates
    /// #7179's `tokens_before`) must omit the segment, not fabricate a percent
    /// against nothing.
    /// Test: itself.
    #[test]
    fn savings_segment_is_absent_without_a_percent_denominator() {
        assert_eq!(
            render_savings_segment(
                &SavingsTotal {
                    tokens_saved: 4_000,
                    tokens_before: 0,
                    cost_saved_usd: 0.01,
                    rows: 1,
                },
                None,
                None,
            ),
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
            let rendered = render_savings_segment(&total(tokens_saved, tokens_before), None, None)
                .unwrap_or_default();
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
        assert_eq!(savings_segment_at(&ledger, "sess-1", |_| None), None);
    }

    /// Why (#7074, acceptance criterion b): an EMPTY ledger must render neither
    /// the per-session figure nor the average — a file that exists but holds no
    /// row is a different code path from a file that does not exist.
    /// Test: itself.
    #[test]
    fn savings_segment_is_absent_on_an_empty_ledger() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ledger = savings_log_in(dir.path());
        std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
        std::fs::write(&ledger, "").expect("write empty ledger");
        assert_eq!(savings_segment_at(&ledger, "sess-1", |_| None), None);
    }

    /// Why: proves the whole path — append a row, fold it back for that session
    /// id, and render — without touching the operator's real root. With one
    /// session on the ledger the average is that session's own figure
    /// (#7074, acceptance criterion a).
    /// Test: itself.
    #[test]
    fn savings_segment_reads_the_ledger_under_an_explicit_root() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ledger = savings_log_in(dir.path());
        write_row(&ledger, "sess-1", 12_000, 60_000);
        assert_eq!(
            savings_segment_at(&ledger, "sess-1", |_| None).as_deref(),
            Some("\u{1f4b8}20%/20%")
        );
        // A different session's bar reads nothing from the same file.
        assert_eq!(savings_segment_at(&ledger, "sess-2", |_| None), None);
    }

    /// Why (#7074, acceptance criterion a): three sessions on one ledger, and
    /// the rendered average is their arithmetic mean — 10, 30 and 50 average to
    /// 30, while a pooled ratio over the same rows would render 29.
    /// Test: itself.
    #[test]
    fn savings_segment_averages_across_every_session_on_the_ledger() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ledger = savings_log_in(dir.path());
        write_row(&ledger, "sess-a", 100, 1_000); // 10 %
        write_row(&ledger, "sess-b", 3_000, 10_000); // 30 %
        write_row(&ledger, "sess-c", 500, 1_000); // 50 %

        assert_eq!(
            savings_segment_at(&ledger, "sess-a", |_| None).as_deref(),
            Some("\u{1f4b8}10%/30%")
        );
    }

    /// Why (#7074, acceptance criterion e): the average is derived at read
    /// time. A future implementation that cached it into a rollup file would
    /// reintroduce the second writer the 2026-07-29 owner ruling forbids, and
    /// the two surfaces could then drift. Asserting the directory listing is
    /// byte-identical after a render is what catches that.
    /// Test: itself.
    #[test]
    fn rendering_writes_nothing_under_the_usage_directory() {
        let dir = tempfile::tempdir().expect("temp dir");
        let ledger = savings_log_in(dir.path());
        write_row(&ledger, "sess-a", 100, 1_000);
        write_row(&ledger, "sess-b", 300, 1_000);

        let usage_dir = ledger.parent().expect("usage dir").to_path_buf();
        let listing = |dir: &Path| -> Vec<(String, u64)> {
            let mut entries: Vec<(String, u64)> = std::fs::read_dir(dir)
                .expect("read usage dir")
                .map(|entry| {
                    let entry = entry.expect("entry");
                    let len = entry.metadata().expect("metadata").len();
                    (entry.file_name().to_string_lossy().into_owned(), len)
                })
                .collect();
            entries.sort();
            entries
        };

        let before = listing(&usage_dir);
        assert!(
            savings_segment_at(&ledger, "sess-a", |_| None).is_some(),
            "the fixture must render, or this test proves nothing"
        );
        assert_eq!(
            listing(&usage_dir),
            before,
            "rendering must add or grow no file under usage/"
        );
    }

    /// Why: before Claude Code assigns a session id there is nothing to fold,
    /// and the probe must not read the disk to discover that.
    /// Test: itself.
    #[test]
    fn savings_segment_probe_is_absent_without_a_session_id() {
        assert_eq!(savings_segment_probe(""), None);
    }

    /// A Claude-config-shaped directory holding one real transcript file.
    ///
    /// Why: `contained_transcript_path` canonicalizes both sides, and
    /// `canonicalize` refuses a path that does not exist — so a fixture that
    /// only names a file proves nothing.
    fn transcript_under(config_dir: &Path, session_id: &str) -> PathBuf {
        let projects = config_dir.join("projects").join("slug");
        std::fs::create_dir_all(&projects).expect("mkdir");
        let transcript = projects.join(format!("{session_id}.jsonl"));
        std::fs::write(&transcript, "{}\n").expect("write transcript");
        transcript
    }

    fn recorded_transcript(root: &Path, session_id: &str) -> Option<String> {
        read_session_record(root, KIND_TRANSCRIPT, session_id)
    }

    /// Why (#7250): the payload names the file a LATER `tm` process opens, so a
    /// path outside the session's Claude config directory must never reach the
    /// store. The model, which carries no path, is still recorded.
    /// Test: itself.
    #[test]
    fn a_transcript_path_outside_the_config_dir_is_not_recorded() {
        let root = tempfile::tempdir().expect("temp dir");
        let config = tempfile::tempdir().expect("temp dir");
        let elsewhere = tempfile::tempdir().expect("temp dir");
        let outside = elsewhere.path().join("stolen.jsonl");
        std::fs::write(&outside, "{}\n").expect("write");

        record_session_facts_at(
            root.path(),
            Some(config.path()),
            "sess-1",
            "claude-opus-4-1",
            "/etc/passwd",
            "",
        );
        assert_eq!(recorded_transcript(root.path(), "sess-1"), None);

        record_session_facts_at(
            root.path(),
            Some(config.path()),
            "sess-1",
            "claude-opus-4-1",
            &outside.to_string_lossy(),
            "",
        );
        assert_eq!(recorded_transcript(root.path(), "sess-1"), None);
        assert_eq!(
            read_session_record(root.path(), KIND_MODEL, "sess-1").as_deref(),
            Some("claude-opus-4-1"),
            "a rejected transcript path must not cost the model record"
        );
    }

    /// Why (#7250): a `..` component walks out of the config directory while
    /// still looking like it starts inside it.
    /// Test: itself.
    #[test]
    fn a_traversing_transcript_path_is_not_recorded() {
        let root = tempfile::tempdir().expect("temp dir");
        let config = tempfile::tempdir().expect("temp dir");
        transcript_under(config.path(), "sess-1");
        let traversing = config
            .path()
            .join("projects")
            .join("..")
            .join("..")
            .join("etc")
            .join("passwd");

        record_session_facts_at(
            root.path(),
            Some(config.path()),
            "sess-1",
            "claude-opus-4-1",
            &traversing.to_string_lossy(),
            "",
        );
        assert_eq!(recorded_transcript(root.path(), "sess-1"), None);
    }

    /// Why (#7250, critic round 2): with no `CLAUDE_CONFIG_DIR` and no home
    /// directory there is no directory to contain the path, and the earlier
    /// `FrameworkPaths::default()` fallback resolved to `"."` — which
    /// canonicalizes to the working directory, quietly re-scoping containment
    /// to `<cwd>/.claude` instead of refusing. `claude_config_dir` now answers
    /// `None` there, and nothing lands in the store. This bin target may not
    /// write `HOME` (#5544), so the `None` arrives as an argument.
    /// Test: itself.
    #[test]
    fn a_transcript_path_is_not_recorded_without_a_config_dir() {
        let root = tempfile::tempdir().expect("temp dir");
        let config = tempfile::tempdir().expect("temp dir");
        let transcript = transcript_under(config.path(), "sess-1");

        record_session_facts_at(
            root.path(),
            None,
            "sess-1",
            "claude-opus-4-1",
            &transcript.to_string_lossy(),
            "",
        );
        assert_eq!(
            recorded_transcript(root.path(), "sess-1"),
            None,
            "with no config directory there is nothing to contain the path"
        );
        assert_eq!(
            read_session_record(root.path(), KIND_MODEL, "sess-1").as_deref(),
            Some("claude-opus-4-1"),
            "the model carries no path, so it is still recorded"
        );
    }

    /// Why (#7250): the screen must still accept the ordinary payload, or the
    /// commit footer silently loses its token counts. What lands is the
    /// canonical path, which on macOS differs from the temp directory's own
    /// spelling.
    /// Test: itself.
    #[test]
    fn a_transcript_path_under_the_config_dir_is_recorded() {
        let root = tempfile::tempdir().expect("temp dir");
        let config = tempfile::tempdir().expect("temp dir");
        let transcript = transcript_under(config.path(), "sess-1");

        record_session_facts_at(
            root.path(),
            Some(config.path()),
            "sess-1",
            "claude-opus-4-1",
            &transcript.to_string_lossy(),
            "",
        );
        assert_eq!(
            recorded_transcript(root.path(), "sess-1"),
            Some(
                transcript
                    .canonicalize()
                    .expect("canonicalize")
                    .to_string_lossy()
                    .into_owned()
            )
        );
    }

    /// Why (#7424): the render that first sees an assistant turn is the only
    /// process holding the session id, the screened transcript path and the
    /// working directory at once, so it is where the startup reading is taken.
    /// A render that takes it must also not take it twice.
    /// Test: itself.
    #[test]
    fn the_first_render_records_the_startup_context() {
        let root = tempfile::tempdir().expect("temp dir");
        let config = tempfile::tempdir().expect("temp dir");
        let project = tempfile::tempdir().expect("temp dir");
        let transcript = transcript_under(config.path(), "sess-1");
        std::fs::write(
            &transcript,
            "{\"type\":\"assistant\",\"message\":{\"id\":\"m1\",\"usage\":{\"input_tokens\":4,\
             \"cache_creation_input_tokens\":74685,\"cache_read_input_tokens\":27297,\
             \"output_tokens\":9}}}\n",
        )
        .expect("write transcript");

        record_session_facts_at(
            root.path(),
            Some(config.path()),
            "sess-1",
            "claude-opus-4-1",
            &transcript.to_string_lossy(),
            &project.path().to_string_lossy(),
        );

        let stored = trusty_mpm::core::startup_context::read_startup_context(root.path(), "sess-1")
            .expect("a startup reading");
        assert_eq!(stored.tokens, 101_986);
        // A render with no cwd in its payload records nothing new, and the
        // reading already taken is never revised.
        record_session_facts_at(
            root.path(),
            Some(config.path()),
            "sess-1",
            "claude-opus-4-1",
            &transcript.to_string_lossy(),
            "",
        );
        assert_eq!(
            trusty_mpm::core::startup_context::read_startup_context(root.path(), "sess-1")
                .expect("still there")
                .tokens,
            101_986
        );
    }
}
