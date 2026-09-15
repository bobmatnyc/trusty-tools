//! The `💸` estimated-savings segment for `tm statusline` (#6958, percent
//! form since #7179, two figures since #7074, latest-over-mean since #8063).
//!
//! Why: the owner asked to see what the harness saves by not sending tokens —
//! since the 2026-09-14 ruling (#7867) that is TOOL-OUTPUT COMPRESSION
//! specifically: what rtk- and shunt-style interception saves on a tool call,
//! credited to that call and visible on the next render. The status bar is
//! where the operator already looks for the session's cost, so the saving
//! belongs beside it. #7179 replaced the dollar/token figure with a
//! percentage, and #7074 put a second percentage beside it.
//!
//! The 2026-09-15 ruling (#8063) settled WHICH two: a session share reported
//! ~1 % for a `git diff` the compressor had just cut by 18.9 %, so the badge
//! read as "the harness saves nothing" while the ledger said otherwise. The
//! left figure is now the LATEST row's own reduction — what the last tool call
//! saved — and the right is the mean across this session's rows. Both are
//! per-row figures, so the two halves measure the same thing and one row shows
//! the same number twice.
//!
//! What: folds `~/.trusty-mpm/usage/savings.jsonl` for the current session
//! across [`trusty_mpm::core::savings::PER_CALL_TECHNIQUES`] — `compress` and
//! `divert` — prices each accepted row on its own `tokens_before`
//! ([`trusty_mpm::core::savings::session_row_percents`]), and renders one
//! segment:
//!
//! | Fold | Renders |
//! |---|---|
//! | rows whose newest saved 34 %, averaging 29 % | `💸34%/29%` |
//! | one row, saving 34 % | `💸34%/34%` |
//! | zero fold for this session id, but a sibling id on the same managed session has rows (#7617) | that sibling's figures |
//! | zero fold across every sibling, or no denominator at all (every row predates #7179's `tokens_before`) | `💸—` |
//!
//! #7617 replaced "nothing at all" in that last row with an explicit empty
//! state. The segment had gone dark twice, and the second time was not a defect
//! in the fold: Claude Code minted a new `session_id` after a relaunch and a
//! `/login` switch — three in one evening for one managed session — and the
//! segment keyed the fold purely by the id on stdin. An absent segment and a
//! genuinely-zero session were the same picture, so nothing anywhere said which
//! had happened.
//!
//! The mean is the arithmetic mean of each ROW's own percentage
//! ([`mean_percent`]), which keeps the 2026-09-09 shape — percentages do not
//! sum into a total but do average cleanly — at the row scope #8063 moved the
//! whole segment to. Both figures are derived at read time from the one
//! ledger: this segment writes nothing, and adds no accumulator file of its
//! own.
//!
//! **`0%` is unreachable by construction**, the same way `$0.00` was before
//! #7179: [`SavingsTotal::is_zero`] gates the whole segment, so a fold with
//! nothing to show renders nothing rather than a false `0%`, and the average is
//! never rendered on its own.
//!
//! **`instruction-compression` rows are not folded here (#7867).** They record
//! one comparison per session launch — the compiled prompt against the corpus
//! it was folded from — not what a tool call avoided sending, and summing the
//! two moved a per-call figure for a reason no tool call caused. Those rows
//! stay on the ledger; `tm doctor`'s `instruction_fold` row reports them.
//!
//! Test: `savings_segment_renders_a_percent`,
//! `an_instruction_compression_row_alone_renders_the_empty_state`,
//! `one_compress_row_moves_the_segment`,
//! `savings_segment_renders_the_average_beside_the_latest_row`,
//! `savings_segment_renders_the_latest_row_not_the_session_total`,
//! `savings_segment_is_absent_on_a_zero_fold`,
//! `savings_segment_never_renders_zero_percent`,
//! `rendering_writes_nothing_under_the_usage_directory`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::savings::{
    SavingsTotal, fold_session_per_call, mean_percent, savings_log_in, session_row_percents,
};

/// Fold the ledger for `session_id` and render the segment, or omit it.
///
/// Why: the probe half is separated from [`render_savings_segment`] so the
/// render rules are unit-testable against hand-built figures, with no
/// filesystem and no resolved framework root.
/// What: resolves the ledger under the operator's framework root — the same
/// `--root` / `TRUSTY_MPM_ROOT` / XDG-config / `~/.trusty-mpm` chain every other
/// `tm` command honours — folds this session's per-call rows from it, and
/// renders. An empty `session_id` (Claude Code sends one only once the session
/// has an id) omits the segment without touching the disk.
/// Test: `savings_segment_probe_is_absent_without_a_session_id`,
/// `savings_segment_reads_the_ledger_under_an_explicit_root`.
pub(crate) fn savings_segment_probe(session_id: &str) -> Option<String> {
    let root = savings_root()?;
    // #7617: the siblings come from the framework root too, so a restart's new
    // session id still folds the managed session's earlier rows.
    savings_segment_at_in(&root, &savings_log_in(&root), session_id)
}

/// [`savings_segment_probe`] against an explicit ledger path.
///
/// Why: makes the missing-ledger and populated-ledger branches assertable end
/// to end from a temp directory, with no environment mutation. Keeping this
/// function's own I/O to the one ledger path its tests already control is what
/// lets them assert the rendered string, not just the fold.
/// What: folds `ledger` for `session_id` and renders that session's per-row
/// percents (#8063).
/// Test: `savings_segment_is_absent_when_the_ledger_is_missing`,
/// `savings_segment_reads_the_ledger_under_an_explicit_root`,
/// `savings_segment_renders_the_latest_row_not_the_session_total`.
// #7617: production now goes through `savings_segment_at_in`, which also needs
// the framework root for the link store. This ledger-only form is kept for the
// tests that predate the link store and assert on the fold alone.
#[cfg(test)]
pub(crate) fn savings_segment_at(ledger: &Path, session_id: &str) -> Option<String> {
    // No root means no link store; the fold is this session's rows alone.
    savings_segment_at_in(Path::new(""), ledger, session_id)
}

/// `savings_segment_at` with the framework root supplied for the link store.
///
/// (Plain span, not an intra-doc link: that ledger-only form is `#[cfg(test)]`
/// since #7617, so it does not exist in a `cargo doc` build.)
///
/// Why (#7617): the segment vanished twice, and the second disappearance was
/// not a defect in the fold at all — Claude Code had minted a new `session_id`
/// after a relaunch and a `/login` switch, three in one evening for one managed
/// session, and the segment keyed the fold purely by the id on stdin. So a
/// restart read exactly like "the harness saved nothing", and an operator had
/// no way to tell a zero from a disappearance. Two changes close that: fold
/// across every Claude session id linked to the same managed session before
/// concluding zero, and then render an EXPLICIT empty state rather than
/// nothing.
/// What: folds `session_id`'s per-call rows (#7867 —
/// [`trusty_mpm::core::savings::fold_session_per_call`]); on a zero, folds each sibling
/// ([`trusty_mpm::core::session_links::linked_claude_ids`]) and takes the first
/// that has rows; on a zero after that, returns [`EMPTY_STATE`]. Both rendered
/// figures then come from the folded id's own rows (#8063).
///
/// FAIL-OPEN: an absent or unreadable ledger, an absent link store, and an
/// unlinked session all fold to zero and render [`EMPTY_STATE`] — a mark that
/// claims no number. Nothing here writes, and no reading is ever substituted
/// for a measurement that did not happen.
/// Test: `savings_segment_folds_a_sibling_session_id`,
/// `savings_segment_renders_the_empty_state_on_a_zero_fold`,
/// `savings_segment_prefers_this_sessions_own_rows`,
/// `an_instruction_compression_row_alone_renders_the_empty_state`,
/// `one_compress_row_moves_the_segment`.
pub(crate) fn savings_segment_at_in(
    framework_root: &Path,
    ledger: &Path,
    session_id: &str,
) -> Option<String> {
    let mut folded_id = session_id.to_string();
    // #7867: per-call techniques only — an instruction-fold row is a launch-time
    // measurement and no longer moves this figure.
    let mut total = fold_session_per_call(ledger, session_id);
    if total.is_zero() {
        // #7617: a restart minted a new id; the managed session's earlier ids
        // still carry its rows.
        for sibling in
            trusty_mpm::core::session_links::linked_claude_ids(framework_root, session_id)
        {
            if sibling == session_id {
                continue;
            }
            let candidate = fold_session_per_call(ledger, &sibling);
            if !candidate.is_zero() {
                total = candidate;
                folded_id = sibling;
                break;
            }
        }
    }
    if total.is_zero() {
        // #7617: an explicit empty state, so a disappearance reads differently
        // from a zero. Never a `0%` — that would be a claim.
        return Some(EMPTY_STATE.to_string());
    }
    render_savings_segment(&total, &session_row_percents(ledger, &folded_id))
        .or_else(|| Some(EMPTY_STATE.to_string()))
}

/// What the segment shows when it has no figure to show (#7617).
///
/// Why: the owner's closure condition — "the segment never omits itself
/// silently". An em dash claims nothing, takes one cell, and is visibly
/// different from both a percentage and an absent segment.
pub(crate) const EMPTY_STATE: &str = "\u{1f4b8}\u{2014}";

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

/// Render this session's per-row percents as the segment text, or `None` to
/// omit it.
///
/// Why: this is the rule the whole segment exists to get right — never a false
/// `0%`, never a fabricated figure.
/// What: `💸<latest>%/<mean>%`, where `row_percents` is this session's accepted
/// per-call rows in ledger order ([`session_row_percents`]): the last element
/// is the newest row's own reduction, and [`mean_percent`] averages all of
/// them. One row renders its figure on both sides. `None` on
/// [`SavingsTotal::is_zero`] — nothing was saved — and `None` on an empty
/// slice, which is a fold whose every row predates `tokens_before` and so has
/// no denominator to divide by; the mean is never rendered alone, because a
/// session with no figure of its own shows neither.
/// Test: `savings_segment_renders_a_percent`,
/// `savings_segment_renders_the_average_beside_the_latest_row`,
/// `savings_segment_renders_the_latest_row_not_the_session_total`,
/// `savings_segment_is_absent_on_a_zero_fold`,
/// `savings_segment_never_renders_zero_percent`.
pub(crate) fn render_savings_segment(total: &SavingsTotal, row_percents: &[u32]) -> Option<String> {
    if total.is_zero() {
        return None;
    }
    // #8063: latest row first, then the mean of this session's rows — a
    // session-share left number rounded a real per-call reduction to 1%.
    let latest = row_percents.last()?;
    let average = mean_percent(row_percents)?;
    Some(format!("\u{1f4b8}{latest}%/{average}%"))
}

// #7867: the suite moved to its own file when the per-call fold's regression
// tests pushed this one against the 500-SLOC production cap.
#[cfg(test)]
#[path = "savings_tests.rs"]
mod tests;
