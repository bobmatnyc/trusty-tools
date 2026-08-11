//! Bulk delete-by-name-glob for the `tm ls` picker (#5539).
//!
//! Why: the picker's `d<N>` deletes exactly one slot per confirmation round, so
//! clearing a batch of throwaway sessions (`tm-tmpsywch8`, `tm-tmpomyigk`, … —
//! the adopted-temp family, plus any `tm-test-*` run) costs one prompt each.
//! Bob's report: "we have a bunch of test sessions to delete". A pattern turns
//! that into one action.
//!
//! What: `d <glob>` compiles the glob against each session's TMUX NAME (the
//! `name` field) and routes the survivors through the SAME
//! [`super::picker_delete::delete_managed_then_local`] the single-slot path
//! uses — no second deletion implementation. The grammar, the match plan, and
//! the confirmation classifier are pure functions so the whole destructive
//! decision is unit-testable without a daemon; the I/O driver at the bottom is
//! the only part that can delete anything, and it is reachable only through
//! [`GlobDeleteOutcome::Confirm`].
//!
//! Why the tmux name and not the id: the name is what the operator reads off
//! the menu and what carries the batch structure they mean to select —
//! `tm-test-01`, `tm-tmpsywch8`, `tm-apex-02`. The id is a UUID; nobody writes a
//! pattern against one, so matching on it would make the feature useless in
//! practice. The slot number is already served by `d<N>`.
//!
//! Safety, and why it is shaped this way: a bulk destructive action fails by
//! matching MORE than the operator pictured, so every guard here narrows rather
//! than widens.
//!   1. Only stopped/errored sessions are ever deleted in bulk
//!      ([`super::picker_delete::delete_needs_force`]). A running match is
//!      reported as skipped, never deleted — which is what protects every live
//!      session on the machine, not just the caller's. Bulk never forces;
//!      `force` is hard-wired `false`, so a session that needs force can only
//!      be deleted one at a time through `d<N>`'s own force-confirm.
//!   2. The caller's OWN session is additionally excluded by tmux name, so it
//!      survives even if the daemon reported its state as stopped (a stale or
//!      display-reconciled record). Two independent guards, since this is the
//!      one deletion the operator can never undo from inside the session.
//!   3. A session with an EMPTY tmux name matches no pattern at all, `*`
//!      included. See [`build_plan`] — treating an absent name as an empty
//!      string is what would let `*` sweep up every nameless record.
//!   4. A pattern matching nothing is an explicit, non-destructive report — it
//!      must never read as a successful no-op.
//!   5. Confirmation is the exact MATCH COUNT typed back, not `y`. Typing the
//!      number is what proves the operator read it; a `y` habit does not.
//!   6. `--dry-run` prints the same plan and exits before the prompt.
//!
//! Test: `glob_*` in `picker_delete_glob_tests.rs`.

use globset::GlobBuilder;
use std::io::Write as _;

use trusty_mpm::client::ManagedSessionSummary;

use super::picker_delete::{DeleteReport, delete_managed_then_local, delete_needs_force};

/// A parsed `d <glob>` request.
///
/// Why: separating the grammar from the match plan lets the parse be checked
/// (and the `--dry-run` suffix stripped) before any session list is consulted.
/// What: `pattern` is the raw glob with the `--dry-run` suffix already removed
/// and whitespace trimmed; `dry_run` records whether that suffix was present.
/// Test: `glob_parse_*`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) struct GlobDeleteRequest {
    /// The glob, verbatim as typed (minus the `--dry-run` suffix).
    pub(crate) pattern: String,
    /// True when the operator asked for a preview instead of a deletion.
    pub(crate) dry_run: bool,
}

/// Why a matched session is not going to be deleted.
///
/// Why: a bulk report that silently drops protected rows teaches the operator
/// the pattern matched fewer sessions than it did. Each skip is shown with its
/// reason so the count on screen always reconciles with the pattern.
/// What: `Running` carries the session's state word for display; `Tombstoned`
/// is a slot the daemon already reports `deleted` (#3034 — no double-delete);
/// `SelfSession` is the caller's own tmux session.
/// Test: `glob_plan_*`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GlobSkip {
    /// Not stopped/errored — deleting it needs an individual force-confirm.
    Running(String),
    /// Already soft-deleted; deleting again is a no-op, not a second removal.
    Tombstoned,
    /// The tmux session this very `tm ls` is running inside.
    SelfSession,
}

/// What the pattern matched, split into what will and will not be deleted.
///
/// Why: the driver prints both halves before prompting, so the operator sees
/// the complete match set — not just the destructive part of it — and can tell
/// a too-broad pattern from a correctly-scoped one before confirming.
/// What: both vectors hold indices into the `sessions` slice the plan was built
/// from, in menu order.
/// Test: `glob_plan_*`.
#[derive(Debug, Default, PartialEq, Eq)]
pub(crate) struct GlobDeletePlan {
    /// Indices that will be deleted on confirmation (stopped/errored, live rows).
    pub(crate) deletable: Vec<usize>,
    /// Indices the pattern matched but which are protected, with the reason.
    pub(crate) skipped: Vec<(usize, GlobSkip)>,
}

impl GlobDeletePlan {
    /// Total rows the pattern matched, protected ones included.
    pub(crate) fn matched(&self) -> usize {
        self.deletable.len() + self.skipped.len()
    }
}

/// The decision the driver acts on — only one variant can lead to a deletion.
///
/// Why: making "did this turn destructive?" a single enum variant means a test
/// asserting `outcome != Confirm` is a complete proof that nothing was deleted,
/// with no daemon and no HTTP mock. Every other variant returns before the
/// prompt, and the prompt itself still has to pass
/// [`confirm_matches_count`].
/// What: `InvalidPattern` carries the compiler's message; `NoMatch` means the
/// glob matched zero sessions; `NothingDeletable` means it matched only
/// protected rows; `DryRun` is the explicit preview; `Confirm` is the only path
/// to the prompt.
/// Test: `glob_decide_*`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum GlobDeleteOutcome {
    /// The glob failed to compile; carries the reason to show the operator.
    InvalidPattern(String),
    /// Zero sessions matched — reported explicitly, never as a silent success.
    NoMatch,
    /// Matched only running and/or already-tombstoned rows; nothing to delete.
    NothingDeletable,
    /// `--dry-run` was requested; print the plan and stop.
    DryRun,
    /// Proceed to the count-confirmation prompt.
    Confirm,
}

/// Recognise the bulk form of the picker's `d` command (#5539).
///
/// Why the remainder must contain a glob metacharacter: `d<N>`'s prefix strip is
/// safe today only because the remainder has to parse as a slot number, which is
/// what keeps a typed `delete` falling through to `Unrecognised` instead of
/// doing something (see `parse_picker_choice`'s doc). Accepting any non-numeric
/// remainder as a pattern would turn that same typo into a bulk destructive
/// action against a session literally named `elete`. Requiring `*`, `?` or `[`
/// keeps the typo inert: bulk delete is reachable only when the operator typed
/// a character that has no other meaning here.
///
/// What: `None` when `rest` is not a bulk pattern (empty, or no glob
/// metacharacter — including the numeric `d2` form, which the caller handles
/// first). `Some(request)` otherwise, with a trailing `--dry-run` token stripped
/// and recorded. The pattern itself is NOT validated here — an unparseable glob
/// is reported by [`decide_glob_delete`] so the operator sees the compiler's
/// message rather than a bare `Unrecognised`.
/// Test: `glob_parse_requires_metacharacter`, `glob_parse_strips_dry_run`,
/// `glob_parse_rejects_empty`, `glob_parse_keeps_inner_dry_run_text`.
pub(crate) fn parse_glob_delete(rest: &str) -> Option<GlobDeleteRequest> {
    let rest = rest.trim();
    // Only a TRAILING `--dry-run` is a flag; the same text mid-pattern stays
    // part of the glob (a session may legitimately be named `tm--dry-run-01`).
    let (pattern, dry_run) = match rest.strip_suffix("--dry-run") {
        Some(head) => (head.trim_end(), true),
        None => (rest, false),
    };
    let pattern = pattern.trim();
    if pattern.is_empty() || !has_glob_metachar(pattern) {
        return None;
    }
    Some(GlobDeleteRequest {
        pattern: pattern.to_string(),
        dry_run,
    })
}

/// True when `s` contains a character that only makes sense as a glob.
///
/// What: `*`, `?` or `[`. `]` alone is deliberately not a trigger — it appears
/// in no useful pattern without a preceding `[`.
/// Test: `glob_parse_requires_metacharacter`.
fn has_glob_metachar(s: &str) -> bool {
    s.contains(['*', '?', '['])
}

/// Split the sessions a compiled glob matches into deletable and protected.
///
/// Why: the guards that make a bulk delete safe live here, as a pure function
/// over the menu the operator is looking at — same slice, same order, no
/// re-fetch that could drift from what was shown.
///
/// Why a session with NO tmux name can never match: `name` is the field the
/// pattern is written against, and a record whose name is empty (a session
/// paused outside tmux, an older snapshot the daemon rehydrated without one)
/// has nothing for the operator to have matched ON. Treating it as an empty
/// string instead would make `*` — and every `*`-suffixed pattern — sweep up
/// every nameless record in the fleet, which is precisely the silent over-match
/// this feature must not have. Absent name means no match, never a match on
/// emptiness.
///
/// What: matches `pattern` against each session's `name`, skipping empty names
/// outright. A matched row goes to `skipped` when it is `current` (the caller's
/// own tmux session), when the daemon reports it `deleted`, or when
/// [`delete_needs_force`] says its state is not stopped/errored; otherwise to
/// `deletable`. `current` is `None` when not running inside tmux, which
/// disables only the self-check — every other guard still applies.
/// Test: `glob_plan_splits_running_from_stopped`,
/// `glob_plan_skips_tombstoned_rows`, `glob_plan_empty_on_no_match`,
/// `glob_plan_nameless_session_never_matches`,
/// `glob_plan_excludes_current_session`.
pub(crate) fn build_plan(
    sessions: &[ManagedSessionSummary],
    pattern: &globset::GlobMatcher,
    current: Option<&str>,
) -> GlobDeletePlan {
    let mut plan = GlobDeletePlan::default();
    for (idx, s) in sessions.iter().enumerate() {
        // A nameless record is unmatchable, `*` included — see the doc above.
        if s.name.trim().is_empty() {
            continue;
        }
        if !pattern.is_match(s.name.as_str()) {
            continue;
        }
        if current.is_some_and(|c| c == s.name) {
            plan.skipped.push((idx, GlobSkip::SelfSession));
        } else if s.deleted {
            plan.skipped.push((idx, GlobSkip::Tombstoned));
        } else if delete_needs_force(&s.state) {
            plan.skipped.push((idx, GlobSkip::Running(s.state.clone())));
        } else {
            plan.deletable.push(idx);
        }
    }
    plan
}

/// Compile the pattern and decide what the driver does with it.
///
/// Why: every branch that must NOT delete is resolved here, before any prompt
/// and before any HTTP client is touched — so the destructive path has exactly
/// one entrance and a test can prove a given input never reaches it.
/// What: compiles `req.pattern` with `literal_separator(false)` so `*` spans the
/// `-` and `/` in a session name (`tm-*` is meant to match `tm-a-b`), and
/// `case_insensitive(true)` because session names are lowercase kebab and an
/// operator should not lose a batch to a capital letter. `current` is the
/// caller's own tmux session name (`None` outside tmux), threaded to
/// [`build_plan`]'s self-check. Returns the outcome paired with the plan it was
/// derived from; the plan is empty for `InvalidPattern`.
/// Test: `glob_decide_invalid_pattern`, `glob_decide_no_match_is_not_destructive`,
/// `glob_decide_all_running_is_not_destructive`,
/// `glob_decide_dry_run_never_confirms`, `glob_decide_confirm_on_deletable`,
/// `glob_decide_star_never_deletes_nameless_sessions`.
pub(crate) fn decide_glob_delete(
    sessions: &[ManagedSessionSummary],
    req: &GlobDeleteRequest,
    current: Option<&str>,
) -> (GlobDeleteOutcome, GlobDeletePlan) {
    let glob = match GlobBuilder::new(&req.pattern)
        .literal_separator(false)
        .case_insensitive(true)
        .build()
    {
        Ok(g) => g,
        Err(e) => {
            return (
                GlobDeleteOutcome::InvalidPattern(e.to_string()),
                GlobDeletePlan::default(),
            );
        }
    };
    let plan = build_plan(sessions, &glob.compile_matcher(), current);
    let outcome = if plan.matched() == 0 {
        GlobDeleteOutcome::NoMatch
    } else if plan.deletable.is_empty() {
        GlobDeleteOutcome::NothingDeletable
    } else if req.dry_run {
        GlobDeleteOutcome::DryRun
    } else {
        GlobDeleteOutcome::Confirm
    };
    (outcome, plan)
}

/// Accept a bulk-delete confirmation only when the operator typed the count.
///
/// Why: `y` is a reflex; a number is not. The failure this defends against is a
/// pattern that matches more sessions than the operator pictured — and the only
/// input that proves the number on screen was actually read is that number,
/// typed back. It also scales with the danger for free: confirming 3 deletions
/// is easy, confirming 63 makes the operator look at 63 first.
/// What: `true` only when the trimmed line parses as a `usize` equal to `count`.
/// `y`, `yes`, empty, a wrong number, and `all` are all `false`.
/// Test: `glob_confirm_requires_exact_count`, `glob_confirm_rejects_yes`.
pub(crate) fn confirm_matches_count(line: &str, count: usize) -> bool {
    line.trim().parse::<usize>() == Ok(count)
}

/// Render the plan as the lines shown before the prompt.
///
/// Why: the operator has to recognise what is about to be deleted by the SAME
/// field they typed, so the tmux name leads every line. The id follows because
/// two records can carry the same tmux name — tmux names get reused over time,
/// so `tm-dogfood` may appear twice in one match set — and a listing that shows
/// only the name would render those two rows identical at the exact moment the
/// operator is deciding whether the count is right. Showing the short id on
/// every row rather than only on a detected collision keeps the rule simple and
/// is strictly more informative. The project follows when the record has one.
/// Pure, so the report's completeness — every matched row appears, protected
/// ones with a reason — is assertable without capturing stderr.
/// What: `  <name>  <short-id>  <project>  (<state>)` per deletable row, then
/// `  <name>  <short-id>  — kept: <reason>` per skipped row.
/// Test: `glob_report_lists_every_match`, `glob_report_disambiguates_same_name`.
pub(crate) fn plan_lines(sessions: &[ManagedSessionSummary], plan: &GlobDeletePlan) -> Vec<String> {
    // First 8 hex chars — the same prefix length `git` uses for a short SHA,
    // enough to separate two same-named records in a fleet of this size.
    let short = |id: &str| id.chars().take(8).collect::<String>();
    let project = |s: &ManagedSessionSummary| {
        s.source_id
            .as_deref()
            .map(|p| format!("  {p}"))
            .unwrap_or_default()
    };
    let mut lines = Vec::with_capacity(plan.matched());
    for &i in &plan.deletable {
        let s = &sessions[i];
        lines.push(format!(
            "  {}  {}{}  ({})",
            s.name,
            short(&s.id),
            project(s),
            s.state
        ));
    }
    for (i, reason) in &plan.skipped {
        let s = &sessions[*i];
        let why = match reason {
            GlobSkip::Running(state) => format!("kept: {state}, needs `d<N>` to force"),
            GlobSkip::Tombstoned => "kept: already deleted".to_string(),
            GlobSkip::SelfSession => "kept: this is the session you are in".to_string(),
        };
        lines.push(format!("  {}  {}  — {why}", s.name, short(&s.id)));
    }
    lines
}

/// Picker action: preview a glob, confirm the count, then delete the matches.
///
/// Why: the TTY half of the feature. Kept separate from the pure seams above so
/// the decision logic stays testable and this function holds nothing but I/O
/// and rendering.
/// What: resolves the caller's own tmux session name via the shared
/// [`super::statusline::branch::tmux_session_name`] probe (`None` outside tmux —
/// there is then no own-session to protect, and the running-session guard still
/// covers it), then runs [`decide_glob_delete`]. Every outcome except `Confirm`
/// prints its report and returns `Ok(0)` WITHOUT contacting the daemon.
/// `Confirm` prints the plan, requires the match count typed back
/// ([`confirm_matches_count`]), and only then loops over `plan.deletable`
/// calling [`delete_managed_then_local`] with `force = false` — a daemon 409 is
/// reported and skipped, never escalated to a force. Returns how many records
/// were actually removed, so the caller can tell a real deletion from a cancel.
/// Test: the pure seams it composes are unit-tested (see module doc); the
/// stdin/HTTP path is side-effect-only, as with `picker_delete::confirm_and_delete`.
pub(crate) async fn confirm_and_delete_glob(
    client: &reqwest::Client,
    url: &str,
    sessions: &[ManagedSessionSummary],
    req: &GlobDeleteRequest,
) -> anyhow::Result<usize> {
    let current = super::statusline::branch::tmux_session_name();
    let (outcome, plan) = decide_glob_delete(sessions, req, current.as_deref());
    let pattern = &req.pattern;
    match outcome {
        GlobDeleteOutcome::InvalidPattern(msg) => {
            eprintln!("tm: '{pattern}' is not a valid glob: {msg}");
            return Ok(0);
        }
        GlobDeleteOutcome::NoMatch => {
            // #5539: an empty match set is a REPORT, never a silent success —
            // the operator must be able to tell "deleted nothing" from
            // "matched nothing" without re-reading the list.
            eprintln!("tm: no sessions match '{pattern}' — nothing deleted.");
            return Ok(0);
        }
        GlobDeleteOutcome::NothingDeletable => {
            eprintln!(
                "tm: '{pattern}' matched {} session(s), none of them deletable:",
                plan.matched()
            );
            for line in plan_lines(sessions, &plan) {
                eprintln!("{line}");
            }
            eprintln!(
                "tm: a running session is deleted one at a time with `d<N>`, \
                 which force-confirms it. Nothing deleted."
            );
            return Ok(0);
        }
        GlobDeleteOutcome::DryRun => {
            eprintln!(
                "tm: --dry-run — '{pattern}' matches {} session(s); {} would be deleted:",
                plan.matched(),
                plan.deletable.len()
            );
            for line in plan_lines(sessions, &plan) {
                eprintln!("{line}");
            }
            eprintln!("tm: dry run — nothing deleted.");
            return Ok(0);
        }
        GlobDeleteOutcome::Confirm => {}
    }

    let count = plan.deletable.len();
    eprintln!(
        "tm: '{pattern}' matches {} session(s); {count} will be PERMANENTLY deleted:",
        plan.matched()
    );
    for line in plan_lines(sessions, &plan) {
        eprintln!("{line}");
    }
    eprint!("tm: type {count} to confirm, or anything else to cancel > ");
    let _ = std::io::stderr().flush();

    let mut line = String::new();
    if std::io::stdin().read_line(&mut line)? == 0 {
        // EOF (Ctrl-D) — cancel, never confirm.
        eprintln!("tm: cancelled — nothing deleted.");
        return Ok(0);
    }
    if !confirm_matches_count(&line, count) {
        eprintln!("tm: cancelled — nothing deleted.");
        return Ok(0);
    }

    let mut deleted = 0usize;
    for &i in &plan.deletable {
        let s = &sessions[i];
        // `force = false`, always: a bulk action never escalates past a guard.
        // The daemon's own tmux probe can still refuse (409) even though the
        // persisted state read as stopped — that refusal is reported and the
        // row is skipped.
        match delete_managed_then_local(client, url, &s.id, false).await? {
            DeleteReport::Deleted { name, .. } => {
                deleted += 1;
                eprintln!("tm: deleted '{name}'.");
            }
            DeleteReport::Refused(msg) => eprintln!("tm: '{}' refused: {msg}", s.name),
            DeleteReport::NotFound => eprintln!("tm: '{}' not found — already gone.", s.name),
        }
    }
    eprintln!("tm: {deleted} of {count} session(s) deleted.");
    Ok(deleted)
}

#[cfg(test)]
#[path = "picker_delete_glob_tests.rs"]
mod tests;
