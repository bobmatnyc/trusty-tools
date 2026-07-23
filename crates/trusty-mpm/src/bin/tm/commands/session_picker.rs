//! Shared managed-session picker + fetch helpers.
//!
//! Why: two entry points now surface the interactive session picker — bare `tm`
//! (the project-scoped guided default in `guided.rs`) and the top-level `tm ls`
//! connector (fleet-wide, optionally scoped). Extracting the numbered-menu loop,
//! the pure choice-parser, and the fetch-and-filter step into one module keeps
//! both call sites on a single implementation (no divergence, no duplication) and
//! keeps `guided.rs` under the 500-SLOC production cap.
//!
//! What: [`parse_picker_choice`] is the pure input→decision seam; [`run_tty_picker`]
//! is the I/O driver that renders the menu, reads stdin, and dispatches to
//! resume/launch; [`fetch_live_sessions`] is the single GET+filter path shared by
//! the static `tm session ls` renderer and the picker; [`run_ls_connector`] is the
//! `tm ls` orchestrator that decides between the interactive picker and static
//! output based on the TTY/`--json`/`--all`/session-count gate
//! ([`should_show_picker`]). [`next_launch_slot`] is the shared "launch new
//! session" number computation (issue #3723) both `run_tty_picker`'s render
//! and `parse_picker_choice` use, kept in one place so they cannot drift.
//! Row TEXT/color rendering lives in the sibling `session_picker_render`
//! module (verbless, color-coded — #3723); the picker's rename action lives
//! in `session_picker_rename` (#3724) — both extracted to keep this file
//! under the 500-SLOC production cap.
//!
//! Test: `parse_picker_choice` unit tests live in `tests_behavior_c_tests.rs`
//! (re-exported through `guided`); the TTY gate is unit-tested by
//! `ls_connector_should_show_picker_*` in `tests_behavior_d_tests.rs`;
//! `next_launch_slot`, `PickerDecision::Rename` parsing, and the row
//! rendering/color logic are unit-tested in `session_picker_tests.rs`; the
//! I/O path is exercised by the e2e suite and manual smoke tests.

use anyhow::Context as _;
use std::io::IsTerminal as _;

use trusty_mpm::client::{ManagedListResponse, ManagedSessionSummary};

use super::managed::filter_live_sessions;

/// Decision returned by [`parse_picker_choice`].
///
/// Why: extracting the parse-and-decide logic from the I/O driver makes it
/// unit-testable without stdin/tmux. The driver calls parse, checks the variant,
/// and shells out only for Resume and LaunchNew.
/// What: seven variants cover every valid and invalid input the picker can
/// receive. [`Self::ConfirmRestart`] is the #2148 safety default: a bare Enter
/// that WOULD have silently restarted (destructively recreated the tmux pane
/// of) a stopped/errored session instead asks for an explicit numeric choice.
/// [`Self::Unresumable`] (#2595) is the analogous safety gate for a DEAD
/// session — no confirmation would ever help, since the resume/restart is
/// guaranteed to fail, so the driver never round-trips to the daemon for it.
/// [`Self::SlotDeleted`] (#3034) is the analogous safety gate for a TOMBSTONED
/// slot number — typing (or bare-Entering onto) a number the daemon reports as
/// deleted must be a clear, explicit error, never a silent fallthrough to
/// whichever session now happens to occupy a neighboring row.
/// Test: `guided_picker_bare_enter_no_sessions_launches_new`,
/// `guided_picker_bare_enter_live_session_resumes_first`,
/// `guided_picker_bare_enter_stopped_session_requires_confirm`,
/// `guided_picker_q_returns_quit`, `guided_picker_numeric_valid_resumes`,
/// `guided_picker_numeric_launch_new`, `guided_picker_out_of_range_unrecognised`,
/// `guided_picker_non_numeric_unrecognised`,
/// `guided_picker_bare_enter_unresumable_session_blocked`,
/// `guided_picker_numeric_unresumable_session_blocked`,
/// `guided_picker_numeric_deleted_slot_blocked`,
/// `guided_picker_delete_prefix_on_deleted_slot_blocked`.
/// [`Self::Rename`] (#3724) parsing is covered by
/// `parse_picker_choice_rename_*` in `session_picker_tests.rs`.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum PickerDecision {
    /// Resume the session at this 0-based index into the sessions slice.
    Resume(usize),
    /// Launch a brand-new session.
    LaunchNew,
    /// User chose to quit without action.
    Quit,
    /// Input was not recognised; the caller quits cleanly.
    Unrecognised,
    /// Bare Enter was pressed but the 0-based-indexed session at this slot is
    /// stopped/errored — resuming it would KILL and recreate its tmux pane
    /// (or, if the pane already happened to be dead, spawn a fresh one). #2148:
    /// this is no longer an implicit default; the operator must type the
    /// number explicitly to confirm the restart.
    ConfirmRestart(usize),
    /// Delete the session at this 0-based index (#2304). Entered as `d<N>`
    /// (e.g. `d2`) or `d <N>`. The driver runs a confirm prompt — and, for a
    /// running session, a force-confirm — before touching the store, so this
    /// variant only signals intent, never an unconditional destructive action.
    Delete(usize),
    /// Selection (bare Enter OR an explicit numeric choice) targeted the
    /// 0-based-indexed session whose workspace is gone for good — the
    /// server-computed `unresumable` flag was `true` (#2595). Resuming it
    /// would only ever fail (see #2577/#2594): the driver never attempts the
    /// daemon round-trip; it prints the dead-session notice and points the
    /// operator at `d<N>` (delete) instead.
    Unresumable(usize),
    /// Selection (bare Enter, an explicit numeric choice, or `d<N>`) targeted
    /// the 0-based-indexed row whose slot the daemon reports as deleted —
    /// `ManagedSessionSummary::deleted == true` (#3034). This is the tombstone
    /// resolution safety gate: it MUST be a clear, explicit outcome, never a
    /// fallthrough to `Unrecognised` (which would look identical to a typo)
    /// or — worse — a silent match against a neighboring live session.
    SlotDeleted(usize),
    /// Rename the session at this 0-based index to the given name (issue
    /// #3724). Entered as `r<N> <new-name>` or `r <N> <new-name>`. The driver
    /// routes this through the EXISTING hardened
    /// `commands::rename::do_rename_request` — the same PATCH
    /// `tm sessions rename` issues, including its #3692
    /// auto-suffix-on-collision behavior — never a reimplementation.
    Rename(usize, String),
}

/// Scope describing which sessions the picker operates over and how to launch new.
///
/// Why: the picker serves both a single-project context (bare `tm`, where a
/// `repo_url` is known so "launch new" can spawn into that project) and a
/// fleet-wide/filtered context (`tm ls`, where sessions may span projects and no
/// single launch target exists). Threading the scope through one struct keeps
/// [`run_tty_picker`] agnostic to its caller.
/// What: `source_id` filters the daemon session list (`None` = every managed
/// session); `repo_url` is the launch-new target (`None` disables launch-new with
/// an actionable hint instead of attaching to the wrong project). `sort`/`term`
/// (#3483) are the `tm ls` inline sort-keyword + filter grammar
/// ([`parse_ls_terms`]) re-applied on every re-fetch inside [`run_tty_picker`]'s
/// loop so the picker's ordering/filtering never drifts from the initial menu.
/// Test: constructed by `guided::try_show_picker` and [`run_ls_connector`];
/// behavior is covered by the picker's e2e path.
pub(crate) struct PickerScope {
    /// `owner/repo` slug to filter by, or `None` for every managed session.
    pub(crate) source_id: Option<String>,
    /// Git-root path used as the launch-new target, or `None` to disable it.
    pub(crate) repo_url: Option<String>,
    /// Sort order re-applied after every re-fetch (#3483).
    pub(crate) sort: SessionSortArg,
    /// Case-insensitive substring filter re-applied after every re-fetch
    /// (#3483); `None` shows every (live) session.
    pub(crate) term: Option<String>,
}

impl PickerScope {
    /// Build a single-project scope (bare `tm` guided default).
    ///
    /// Why: the guided default always knows both the project slug and the git
    /// root, so both fields are always populated. It predates (and is out of
    /// scope for) the `tm ls` sort/filter grammar, so `sort`/`term` are always
    /// the no-op defaults here.
    /// What: returns a scope filtered to `source_id` with `repo_url` as the
    /// launch target.
    /// Test: exercised by `guided::try_show_picker`.
    pub(crate) fn project(source_id: &str, repo_url: &str) -> Self {
        Self {
            source_id: Some(source_id.to_string()),
            repo_url: Some(repo_url.to_string()),
            sort: SessionSortArg::Recent,
            term: None,
        }
    }
}

/// GET the raw managed-session list body, optionally scoped by `source_id`.
///
/// Why: the `--json` passthrough needs the byte-for-byte daemon response while
/// the table/picker paths need the deserialized form — both must issue exactly
/// one GET. Returning the raw text lets each caller decide.
/// What: GETs `/api/v1/sessions/managed`, appending `?source_id=` when a filter
/// is active, and returns the response body as a `String` after status checks.
/// Test: HTTP round-trip covered by `tests/session_manager_mvp.rs`.
pub(crate) async fn fetch_managed_raw(
    client: &reqwest::Client,
    url: &str,
    source_id: Option<&str>,
) -> anyhow::Result<String> {
    let endpoint = format!("{url}/api/v1/sessions/managed");
    let mut req = client.get(&endpoint);
    if let Some(sid) = source_id {
        req = req.query(&[("source_id", sid)]);
    }
    Ok(req.send().await?.error_for_status()?.text().await?)
}

/// Parse a raw managed-session list body into a scoped `Vec` for display.
///
/// Why: the static table and the picker share one filtering/sorting policy so
/// the two views never diverge (#1809/#1841).
/// What: deserializes `ManagedListResponse`; when `all` is false, drops
/// decommissioned records via [`filter_live_sessions`] (a `"deleted"` slot
/// tombstone, #3034, is NOT a decommissioned record and always passes through
/// this branch too — see [`super::managed::is_live_session_state`]'s doc).
/// When `all` is true, keeps every row — live, decommissioned, AND tombstoned
/// — but stable-sorts ONLY `"decommissioned"` records to the end.
///
/// `"deleted"` (#3034) tombstones are deliberately EXCLUDED from that
/// sink-to-bottom sort key, even though a reviewer might expect both "dead"
/// states to be grouped identically. The two are not interchangeable: a
/// `"decommissioned"` row is a soft-retired but still-present record — sinking
/// it to the bottom is a pre-existing (#1809) forensic declutter for the
/// `--all` view, where recency/liveness ordering is more useful than slot
/// order. A `"deleted"` tombstone, by contrast, IS the numbered slot itself —
/// Bob's #3034 directive requires it to render at its ORIGINAL position in the
/// numbered listing, not wherever a liveness sort would relocate it, so an
/// operator scanning the table top-to-bottom sees the exact gap where a
/// session used to be. `[`render_session_table`](super::managed::render_session_table)
/// and the picker menu both label each row with its own `slot` field (not a
/// recomputed position), so this is not merely cosmetic — a sink-to-bottom
/// move for tombstones would visually separate a deleted slot from its live
/// neighbors, breaking the "see it where it was" guarantee even though the
/// printed number itself would still be technically correct. `fetched` already
/// arrives in ascending-slot order from the daemon's `numbered_summaries`,
/// and `Vec::sort_by_key` is stable, so every row that is not
/// `"decommissioned"` (live rows AND tombstones alike) keeps that incoming
/// slot order untouched.
/// Test: `picker_filter_excludes_decommissioned_keeps_active`,
/// `ls_source_id_filter_selects_correct_slug` in `tests_behavior_c_tests.rs`;
/// `parse_scoped_sessions_all_keeps_tombstone_in_slot_order` in
/// `commands/session_tests.rs` covers the sink-to-bottom exclusion this doc
/// describes.
pub(crate) fn parse_scoped_sessions(
    raw: &str,
    all: bool,
) -> anyhow::Result<Vec<ManagedSessionSummary>> {
    let fetched = serde_json::from_str::<ManagedListResponse>(raw)?.sessions;
    let sessions = if all {
        let mut s = fetched;
        // Sink ONLY soft-retired "decommissioned" records — never "deleted"
        // slot tombstones, which must stay in their original slot position
        // (see this function's doc for the full reasoning).
        s.sort_by_key(|sess| u8::from(sess.state == "decommissioned"));
        s
    } else {
        filter_live_sessions(fetched)
    };
    Ok(sessions)
}

/// Sort order for the `tm ls` / `tm sessions ls` table and picker (#3483).
///
/// Why: the repo owner asked for selectable recent/alpha ordering expressed as
/// an inline positional keyword (`tm ls recent|alpha …`), NOT a `--sort` flag —
/// see [`parse_ls_terms`] for the grammar that produces this value.
/// What: `Recent` (the default) orders by most-recently-active first; `Alpha`
/// orders case-insensitively by session name.
/// Test: `sort_sessions_recent_orders_by_last_activity`,
/// `sort_sessions_alpha_orders_by_name_case_insensitive`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum SessionSortArg {
    /// Most-recently-active session first (falls back to `created_at` when
    /// `last_activity_at` is absent; a session with neither sorts last).
    #[default]
    Recent,
    /// Alphabetical by session `name`, case-insensitive.
    Alpha,
}

/// Resolve `tm ls`'s / `tm sessions ls`'s positional `terms` into a sort mode
/// and an optional filter substring.
///
/// Why (PM correction): sort/filter must be bare positional words, not
/// `--sort`/`--filter` flags — mirroring the `tm ticket <issue> [system]`
/// precedent (a positional keyword with a default) rather than introducing a
/// new flag convention.
/// What: grammar — if the FIRST word case-insensitively equals `recent` or
/// `alpha`, it is consumed as the sort keyword and every remaining word
/// (joined with a single space) becomes the filter (`None` if none remain).
/// Otherwise the sort defaults to [`SessionSortArg::Recent`] and EVERY word
/// (joined with a single space) is the filter. No words at all → `(Recent,
/// None)`, i.e. bare `tm ls` is unchanged. This means a filter term that
/// happens to equal a sort keyword (e.g. filtering for the literal substring
/// "alpha") is reachable by prefixing the OTHER keyword: `tm ls recent alpha`
/// sorts by recency and filters for "alpha".
/// Test: `parse_ls_terms_empty_defaults_recent_no_filter`,
/// `parse_ls_terms_recent_keyword_only`, `parse_ls_terms_alpha_keyword_only`,
/// `parse_ls_terms_keyword_case_insensitive`,
/// `parse_ls_terms_recent_with_filter`, `parse_ls_terms_alpha_with_filter`,
/// `parse_ls_terms_non_keyword_first_word_is_filter`,
/// `parse_ls_terms_multi_word_filter_without_keyword_joins_with_space`,
/// `parse_ls_terms_keyword_then_filter_equal_to_other_keyword`.
pub(crate) fn parse_ls_terms(terms: &[String]) -> (SessionSortArg, Option<String>) {
    match terms.split_first() {
        None => (SessionSortArg::Recent, None),
        Some((first, rest)) if first.eq_ignore_ascii_case("recent") => {
            (SessionSortArg::Recent, join_non_empty(rest))
        }
        Some((first, rest)) if first.eq_ignore_ascii_case("alpha") => {
            (SessionSortArg::Alpha, join_non_empty(rest))
        }
        Some(_) => (SessionSortArg::Recent, join_non_empty(terms)),
    }
}

/// Join `words` with a single space, or `None` when empty.
fn join_non_empty(words: &[String]) -> Option<String> {
    if words.is_empty() {
        None
    } else {
        Some(words.join(" "))
    }
}

/// Case-insensitive substring filter over a session's visible identifying
/// columns (#3483).
///
/// Why: the operator scans `id`, `name`, the project slug, `state`, and `task`
/// on the rendered table/picker — the filter should match anything they can
/// actually see, not an internal-only field.
/// What: keeps a session when `term` (lowercased) is a substring of ANY of:
/// `id`, `name`, `source_id` (project), `state`, `task`. A `None`/absent field
/// is simply skipped. `term = None` is a no-op (returns `sessions` unchanged).
/// Test: `filter_sessions_by_term_matches_name`,
/// `filter_sessions_by_term_matches_task`,
/// `filter_sessions_by_term_matches_source_id`,
/// `filter_sessions_by_term_is_case_insensitive`,
/// `filter_sessions_by_term_no_match_returns_empty`,
/// `filter_sessions_by_term_none_is_noop`.
pub(crate) fn filter_sessions_by_term(
    sessions: Vec<ManagedSessionSummary>,
    term: Option<&str>,
) -> Vec<ManagedSessionSummary> {
    let Some(term) = term else {
        return sessions;
    };
    let needle = term.to_lowercase();
    sessions
        .into_iter()
        .filter(|s| session_matches_term(s, &needle))
        .collect()
}

/// Pure predicate backing [`filter_sessions_by_term`]; `needle_lower` must
/// already be lowercased.
fn session_matches_term(s: &ManagedSessionSummary, needle_lower: &str) -> bool {
    [
        Some(s.id.as_str()),
        Some(s.name.as_str()),
        s.source_id.as_deref(),
        Some(s.state.as_str()),
        s.task.as_deref(),
    ]
    .into_iter()
    .flatten()
    .any(|field| field.to_lowercase().contains(needle_lower))
}

/// Sort `sessions` in place per [`SessionSortArg`] (#3483).
///
/// Why: shared by the static table (`tm ls` / `tm sessions ls`) and the
/// interactive picker so both views order sessions identically.
/// What: `Recent` sorts descending by [`recency_key`] (most recent first);
/// `Alpha` sorts ascending, case-insensitively, by `name`. Both use the
/// stable `sort_by`, so equal keys preserve the daemon's original relative
/// order.
/// Test: `sort_sessions_recent_orders_by_last_activity`,
/// `sort_sessions_recent_falls_back_to_created_at`,
/// `sort_sessions_alpha_orders_by_name_case_insensitive`.
pub(crate) fn sort_sessions(sessions: &mut [ManagedSessionSummary], sort: SessionSortArg) {
    match sort {
        SessionSortArg::Recent => {
            sessions.sort_by(|a, b| recency_key(b).cmp(recency_key(a)));
        }
        SessionSortArg::Alpha => {
            sessions.sort_by_key(|s| s.name.to_lowercase());
        }
    }
}

/// Best-available recency signal for a session (#3483).
///
/// Why: `last_activity_at` reflects actual usage (the daemon updates it on
/// every interaction), which is the signal an operator scanning `tm ls`
/// actually wants — a session touched five minutes ago should outrank one
/// merely CREATED first. `created_at` is the fallback for legacy/additive
/// records that predate the activity timestamp; a session with neither sorts
/// last (empty string is the lexicographic minimum).
/// What: RFC 3339 timestamps compare correctly as plain strings because the
/// daemon always emits them in the same normalized (UTC, fixed-precision)
/// form.
/// Test: covered indirectly by `sort_sessions_recent_orders_by_last_activity`
/// and `sort_sessions_recent_falls_back_to_created_at`.
fn recency_key(s: &ManagedSessionSummary) -> &str {
    s.last_activity_at
        .as_deref()
        .or(s.created_at.as_deref())
        .unwrap_or("")
}

/// Fetch and filter managed sessions in one call — the shared picker fetch path.
///
/// Why: the interactive picker (both call sites) needs the deserialized, filtered
/// session list; combining the GET and the parse keeps re-fetch-after-detach a
/// single call and guarantees identical scoping to the static list.
/// What: [`fetch_managed_raw`] then [`parse_scoped_sessions`]. The picker always
/// requests live-only (`all = false`) — a decommissioned tombstone is never a
/// resume target.
/// Test: HTTP path in `tests/session_manager_mvp.rs`; the parse/filter seam is
/// unit-tested via `parse_scoped_sessions`.
pub(crate) async fn fetch_live_sessions(
    client: &reqwest::Client,
    url: &str,
    source_id: Option<&str>,
    all: bool,
) -> anyhow::Result<Vec<ManagedSessionSummary>> {
    let raw = fetch_managed_raw(client, url, source_id).await?;
    parse_scoped_sessions(&raw, all)
}

/// Find the 0-based position in `sessions` whose stable `slot` equals `n`
/// (issue #3034).
///
/// Why: since the daemon assigns slot numbers, a typed number no longer maps
/// to `n - 1` positionally — filtering (by `--source-id`, or a tombstoned row
/// dropped from an older daemon's response) can leave gaps, so every
/// number→session resolution must search by the `slot` field rather than
/// computing an index arithmetically. Centralizing the search here is what
/// guarantees `parse_picker_choice` can never silently resolve a typed number
/// to the WRONG session merely because a neighboring slot disappeared.
/// What: linear search (menus are small); returns the position or `None`.
/// Test: exercised indirectly by every `guided_picker_numeric_*` test.
pub(crate) fn find_slot(sessions: &[ManagedSessionSummary], n: u32) -> Option<usize> {
    if slots_are_stale(sessions) {
        // #3678: every row decoded to the shared `0` sentinel, so the real
        // field can't disambiguate rows — resolve the same 1-based position
        // the fallback render used instead (see `shown_slot`).
        let idx = usize::try_from(n.checked_sub(1)?).ok()?;
        return (idx < sessions.len()).then_some(idx);
    }
    sessions.iter().position(|s| s.slot == n)
}

/// True when every session in a non-empty menu reports the unassigned `0`
/// slot sentinel (issue #3678).
///
/// Why: a daemon that predates the #3034/#3044 stable-slot-numbering feature
/// never emits the `slot`/`deleted` fields on `GET /sessions/managed` at
/// all — `ManagedSessionSummary`'s `#[serde(default)]` on `slot` then
/// silently decodes EVERY row to `0` instead of erroring, since that field
/// was added additively for exactly this forward/backward-compat reason (see
/// its doc comment in `client::http_client::types`). A daemon process is not
/// restarted merely by installing a newer `tm` binary, so this is the normal
/// shape of the window between a CLI upgrade and the operator's next `tm
/// restart` (or daemon auto-restart) — not a corrupt response. Without this
/// guard, [`find_slot`]'s by-slot lookup and the render loop's `[N]` label
/// both collapse to the SAME value (`0`) for every row: the menu prints
/// `[0]` on every line and typing any number resolves to the first row only
/// (`Vec::position` returns the first match) — the exact symptom reported.
/// A healthy daemon never produces this shape: slots are assigned 1-based
/// and unique per session, so `0` on literally every row of a non-empty,
/// single-GET response is conclusive, not a heuristic guess.
/// What: `false` for an empty list; otherwise `true` only when EVERY
/// session's `slot == 0`.
/// Test: `slots_are_stale_true_when_all_zero`,
/// `slots_are_stale_false_when_any_nonzero`,
/// `slots_are_stale_false_when_empty`.
pub(crate) fn slots_are_stale(sessions: &[ManagedSessionSummary]) -> bool {
    !sessions.is_empty() && sessions.iter().all(|s| s.slot == 0)
}

/// The picker-menu number to print/accept for `sessions[idx]` (issue #3678).
///
/// Why: every render/prompt site needs the SAME fallback numbering when
/// [`slots_are_stale`] is true, or the printed menu and the accepted input
/// would drift apart again exactly like the bug this fixes.
/// What: the real, stable `slot` field when the menu's slots are trustworthy;
/// otherwise a locally-computed 1-based position (`idx + 1`) that is at
/// least internally consistent for the lifetime of this one redisplay (it is
/// NOT stable across a re-fetch, since it isn't backed by the daemon's
/// registry — restarting the daemon is still required to restore real
/// stable numbering).
/// Test: covered via `run_tty_picker`'s render (manual/e2e) and indirectly by
/// `find_slot`'s stale-fallback tests, which exercise the same arithmetic.
pub(crate) fn shown_slot(sessions: &[ManagedSessionSummary], idx: usize) -> u32 {
    if slots_are_stale(sessions) {
        idx as u32 + 1
    } else {
        sessions[idx].slot
    }
}

/// Compute the picker's "launch new session" menu number (issue #3723,
/// duplicate-index defect).
///
/// Why: both [`parse_picker_choice`] and the render loop in [`run_tty_picker`]
/// used to compute this as `sessions.last().slot + 1` — safe ONLY while
/// `sessions` stayed in the daemon's ascending-slot order end-to-end. #3483
/// added `sort_sessions` (`Recent`/`Alpha`), applied to this SAME slice
/// before it ever reaches either call site — the highest-slot session can
/// then sit anywhere in the (re-sorted) vector, not necessarily last. Taking
/// `.last().slot + 1` after a reorder can compute a number that COLLIDES
/// with an existing session's own slot elsewhere in the list — the exact
/// duplicate-`[N]` defect Bob reported (`[31] restart …` AND `[31] launch
/// new session` printed together). Centralizing the fix here (rather than
/// patching both call sites' arithmetic independently) also guarantees they
/// can never drift apart again.
/// What: `slots_are_stale` → `sessions.len() + 1` (unchanged; the positional
/// fallback numbers 1..=len have no real slots to collide with). Otherwise
/// the true MAXIMUM `slot` across the whole slice — order-independent — plus
/// one; `sessions.is_empty()` → `1`.
/// Test: `next_launch_slot_survives_recency_reorder`,
/// `next_launch_slot_survives_alpha_reorder`,
/// `next_launch_slot_matches_ascending_order`,
/// `next_launch_slot_empty_sessions_is_one`,
/// `next_launch_slot_stale_slots_uses_positional_fallback` in
/// `session_picker_tests.rs`.
pub(crate) fn next_launch_slot(sessions: &[ManagedSessionSummary]) -> u32 {
    if slots_are_stale(sessions) {
        return sessions.len() as u32 + 1;
    }
    sessions.iter().map(|s| s.slot).max().map_or(1, |m| m + 1)
}

/// Decide a picker choice for the (already-located) session at `idx`.
///
/// Why: bare Enter and an explicit numeric choice share every safety check
/// except the restart-confirmation gate (which applies ONLY to bare Enter on
/// index 0 — an explicit numeric choice always dispatches directly, per
/// #2148's original design). Factoring the shared checks out here is what
/// keeps the tombstone gate (#3034) and the dead-session gate (#2595)
/// exhaustively applied to BOTH input styles without duplicating the checks.
/// What: `PickerDecision::SlotDeleted(idx)` when `sessions[idx].deleted`
/// (checked FIRST — a deleted slot can never be resumed, confirmed, or
/// flagged merely unresumable); else `Unresumable(idx)` when
/// `sessions[idx].unresumable`; else `ConfirmRestart(idx)` only when
/// `bare_enter_needs_restart` is `true`; else `Resume(idx)`.
/// Test: `guided_picker_numeric_deleted_slot_blocked`,
/// `guided_picker_bare_enter_unresumable_session_blocked`,
/// `guided_picker_numeric_unresumable_session_blocked`.
fn decide_for_index(
    sessions: &[ManagedSessionSummary],
    idx: usize,
    bare_enter_needs_restart: bool,
) -> PickerDecision {
    let s = &sessions[idx];
    if s.deleted {
        PickerDecision::SlotDeleted(idx)
    } else if s.unresumable {
        PickerDecision::Unresumable(idx)
    } else if bare_enter_needs_restart {
        PickerDecision::ConfirmRestart(idx)
    } else {
        PickerDecision::Resume(idx)
    }
}

/// Parse one line of picker input into a [`PickerDecision`].
///
/// Why: separating parse-and-decide from the I/O driver makes the dispatch
/// logic unit-testable without needing a real stdin, tmux, or daemon. Folding
/// `first_needs_restart` in here (rather than deciding safety in the I/O
/// driver) keeps the destructive-default guard (#2148) exhaustively
/// unit-testable alongside every other picker-input case. `unresumable`
/// (#2595) and `deleted` (#3034) are folded in the same way via
/// [`decide_for_index`]: a session flagged dead, or a slot flagged deleted, by
/// the daemon must never reach `Resume`/`ConfirmRestart` from EITHER a bare
/// Enter or an explicit numeric choice.
/// What: `sessions` is the FULL numbered menu — live rows AND tombstoned rows
/// (`ManagedSessionSummary::deleted`, #3034) alike, since a typed number must
/// be able to resolve to a tombstone and produce `SlotDeleted` rather than
/// falling through to `Unrecognised` or (worse) silently matching a
/// neighboring row. Numbers are read off each session's stable `slot` field
/// (via [`find_slot`]), never a positional index, since filtering can leave
/// gaps. `first_needs_restart` is true when the session at position 0 is
/// `stopped`/`errored` — i.e. resuming it goes through the daemon's restart
/// path, which can recreate its tmux pane (see
/// [`super::guided_resume::needs_restart`]).
///   • `"q"` / `"Q"` → `Quit`
///   • empty / whitespace, `sessions` empty → `LaunchNew`
///   • empty / whitespace, `sessions` non-empty → [`decide_for_index`] on
///     position 0, gated by `first_needs_restart`
///   • `N` matching some `sessions[i].slot` → [`decide_for_index`] on `i`
///     (an EXPLICIT numeric choice never applies the restart-confirm gate —
///     it always dispatches directly, confirm or not, per #2148)
///   • `N` == `(highest displayed slot) + 1` (or `1` when `sessions` is
///     empty) → `LaunchNew`
///   • `d<N>` / `d <N>` matching a LIVE `sessions[i].slot` → `Delete(i)`
///     (#2304); the driver still runs a confirm/force-confirm prompt before
///     deleting. Matching a TOMBSTONED slot → `SlotDeleted(i)` (#3034 — no
///     double-delete, no silent no-op).
///   • `r<N> <new-name>` / `r <N> <new-name>` matching a LIVE `sessions[i].slot`,
///     with a non-empty trimmed name → `Rename(i, new-name)` (#3724). A
///     TOMBSTONED slot → `SlotDeleted(i)`; a missing/unparseable number or an
///     empty/whitespace-only name → `Unrecognised`.
///   • anything else → `Unrecognised`
/// Test: `guided_picker_bare_enter_no_sessions_launches_new`,
/// `guided_picker_bare_enter_live_session_resumes_first`,
/// `guided_picker_bare_enter_stopped_session_requires_confirm`,
/// `guided_picker_q_returns_quit`, `guided_picker_q_uppercase_returns_quit`,
/// `guided_picker_numeric_valid_resumes`, `guided_picker_numeric_launch_new`,
/// `guided_picker_out_of_range_unrecognised`,
/// `guided_picker_non_numeric_unrecognised`,
/// `guided_picker_bare_enter_unresumable_session_blocked`,
/// `guided_picker_numeric_unresumable_session_blocked`,
/// `guided_picker_numeric_deleted_slot_blocked`,
/// `guided_picker_delete_prefix_on_deleted_slot_blocked`,
/// `guided_picker_launch_new_uses_highest_slot_with_gaps`.
pub(crate) fn parse_picker_choice(
    line: &str,
    sessions: &[ManagedSessionSummary],
    first_needs_restart: bool,
) -> PickerDecision {
    let choice = line.trim();
    if choice.eq_ignore_ascii_case("q") {
        return PickerDecision::Quit;
    }
    // #3723: see `next_launch_slot`'s doc — this must be the MAXIMUM slot
    // across the whole (possibly `sort_sessions`-reordered, #3483) slice,
    // never `sessions.last().slot + 1`, or a re-sorted list can compute a
    // "launch new" number that collides with an existing session's slot.
    let next_slot = next_launch_slot(sessions);
    // #3724: `r<N> <new-name>` / `r <N> <new-name>` renames the LIVE session
    // at slot N through the existing hardened rename path
    // (`session_picker_rename::rename_selected`). Parsed before the numeric
    // resume branch, mirroring `d<N>`'s delete prefix, so the `r` prefix is
    // unambiguous. A tombstoned target resolves to `SlotDeleted`, never a
    // rename of a slot that no longer exists.
    if let Some(rest) = choice.strip_prefix(['r', 'R']) {
        return match rest.trim_start().split_once(char::is_whitespace) {
            Some((num_str, name)) => {
                let name = name.trim();
                match (
                    name.is_empty(),
                    num_str
                        .parse::<u32>()
                        .ok()
                        .and_then(|n| find_slot(sessions, n)),
                ) {
                    (false, Some(idx)) if sessions[idx].deleted => PickerDecision::SlotDeleted(idx),
                    (false, Some(idx)) => PickerDecision::Rename(idx, name.to_string()),
                    _ => PickerDecision::Unrecognised,
                }
            }
            None => PickerDecision::Unrecognised,
        };
    }
    // #2304: `d<N>` / `d <N>` deletes the LIVE session at slot N. Parsed
    // before the numeric-resume branch so the `d` prefix is unambiguous.
    // Deleting a dead (unresumable, but not yet deleted) session is exactly
    // the intended remedy, so this branch is NOT gated by `unresumable` — but
    // a slot the daemon already reports `deleted` (#3034) cannot be deleted
    // again, so it resolves to `SlotDeleted` instead of a silent no-op.
    if let Some(rest) = choice.strip_prefix(['d', 'D']) {
        return match rest
            .trim()
            .parse::<u32>()
            .ok()
            .and_then(|n| find_slot(sessions, n))
        {
            Some(idx) if sessions[idx].deleted => PickerDecision::SlotDeleted(idx),
            Some(idx) => PickerDecision::Delete(idx),
            None => PickerDecision::Unrecognised,
        };
    }
    if choice.is_empty() {
        if sessions.is_empty() {
            return PickerDecision::LaunchNew;
        }
        return decide_for_index(sessions, 0, first_needs_restart);
    }
    if let Ok(n) = choice.parse::<u32>() {
        if let Some(idx) = find_slot(sessions, n) {
            // An EXPLICIT numeric choice always dispatches directly — the
            // restart-confirm gate applies ONLY to bare Enter (#2148).
            return decide_for_index(sessions, idx, false);
        }
        if n == next_slot {
            return PickerDecision::LaunchNew;
        }
    }
    PickerDecision::Unrecognised
}

/// Interactive numbered picker (TTY mode only).
///
/// Why: a simple numbered menu is the lowest-friction way to resume or launch
/// without requiring the operator to remember session names or UUIDs. After a
/// detach, the picker is redisplayed rather than exiting to the shell — the
/// common "pick → Ctrl-b d → pick again" flow stays in one command.
/// What: loops: print menu, read one line, dispatch, then re-fetch the session
/// list (via [`fetch_live_sessions`], honoring the scope) so the next iteration
/// shows current state. Exits cleanly on `Quit`, EOF (Ctrl-D), or unrecognised
/// input; propagates attach/launch errors.
///   • `Resume(i)` → [`super::guided_resume::resume_guided_session`] which
///     handles daemon restart when needed and then attaches internally;
///   • `LaunchNew` → [`super::guided_launch::launch_new_session_and_attach`] when
///     the scope carries a `repo_url`; otherwise prints an actionable hint and
///     redisplays the menu (fleet-wide `tm ls` has no single launch target);
///   • `ConfirmRestart(i)` (#2148) → print a one-line "type the number to
///     confirm" notice and redisplay the SAME menu — no daemon round-trip;
///   • `Unresumable(i)` (#2595) → print the dead-session notice pointing at
///     `d<N>` and redisplay the SAME menu — no daemon round-trip (a resume
///     would only ever fail: #2577/#2594);
///   • `Delete(i)` (#2304) → [`super::picker_delete::confirm_and_delete`] runs a
///     confirm (force-confirm for a running session) then the managed→local
///     routed delete; redisplay the re-fetched menu afterwards;
///   • `Rename(i, name)` (#3724) → [`super::session_picker_rename::rename_selected`]
///     issues the PATCH through the existing hardened rename path and prints
///     the outcome; redisplay the re-fetched menu afterwards;
///   • `SlotDeleted(i)` (#3034) → print a "session N was deleted" notice and
///     redisplay the SAME menu — no daemon round-trip, and never a
///     fallthrough to whichever session now occupies a neighboring slot;
///   • `Quit` / EOF / `Unrecognised` → print notice and return `Ok`.
///
/// #2678: both `Resume` and `LaunchNew` can end in a tmux hand-off — either a
/// blocking `attach-session` (outside tmux; control returns here once the
/// operator detaches, so the loop legitimately redisplays the menu) or a
/// non-blocking `switch-client` (inside tmux; control does NOT return to a
/// visible terminal — this process's own pane is now hidden). The
/// `AttachOutcome` each helper returns is checked via
/// [`super::tmux_attach::AttachOutcome::ends_interactive_loop`]; when it is `true`
/// (a `switch-client` handoff, or a fail-closed skip that could not resolve a
/// safe target) the loop `break`s immediately instead of falling through to
/// re-fetch sessions and block on `stdin` again — the exact hang/orphan
/// #2678 reported.
/// Test: `parse_picker_choice` is the testable seam; the `AttachOutcome`
/// decision itself is unit-tested by `attach_outcome_ends_interactive_loop_matrix`
/// in `tmux_attach.rs`; the full I/O path is exercised by manual smoke tests
/// and the e2e suite.
pub(crate) async fn run_tty_picker(
    client: &reqwest::Client,
    url: &str,
    scope: &PickerScope,
    mut sessions: Vec<ManagedSessionSummary>,
) -> anyhow::Result<()> {
    // #3723: resolved ONCE per invocation, not per row — see
    // `session_picker_render::picker_use_color`'s doc for why stderr (the
    // stream this menu is written to) rather than stdout, and why NO_COLOR
    // is honored even though `should_show_picker` already gates on a TTY.
    let use_color = super::session_picker_render::picker_use_color(std::io::stderr().is_terminal());
    loop {
        eprintln!();
        // #3034: the menu number is each session's STABLE daemon-assigned
        // slot, never a recomputed positional index — a tombstoned row keeps
        // its slot in the printed menu too, so an operator who typed that
        // number from an earlier listing sees exactly why it no longer
        // resolves to a session, rather than it silently vanishing.
        //
        // #3678: THAT guarantee only holds when the daemon actually reports
        // real slots. A daemon process that predates #3034 (i.e. hasn't been
        // restarted since the `tm` binary was upgraded) omits `slot`
        // entirely, and `#[serde(default)]` silently decodes it to `0` for
        // every row — `slots_are_stale` detects exactly that shape, and
        // `shown_slot`/the `next_slot` fallback below keep the printed
        // numbers distinct and incrementing (positional-only, not stable
        // across a re-fetch) instead of every row printing `[0]`.
        let stale_slots = slots_are_stale(&sessions);
        // #3723: see `next_launch_slot`'s doc — MUST be the maximum slot
        // across the whole (possibly reordered) slice, never
        // `sessions.last().slot + 1`, to avoid colliding with an existing
        // session's own slot after a `sort_sessions` reorder.
        let new_idx = next_launch_slot(&sessions);
        // #2148: bare Enter must not silently restart (kill+recreate the tmux
        // pane of) a stopped/errored session — only used to pick the menu's
        // default hint and to gate `parse_picker_choice`'s bare-Enter branch.
        // A tombstoned position 0 is never `stopped`/`errored` in this sense
        // (`decide_for_index` checks `deleted` first, ahead of this flag).
        let first_needs_restart = sessions
            .first()
            .map(|s| !s.deleted && super::guided_resume::needs_restart(&s.state))
            .unwrap_or(false);
        if sessions.is_empty() {
            eprintln!("tm:   [Enter] launch new session");
            eprintln!("tm:   [q]     quit");
        } else {
            if stale_slots {
                eprintln!(
                    "tm: warning — the running daemon appears to predate stable session \
                     numbering (issue #3034); the numbers below are positional for this \
                     listing only — run `tm restart` to restore permanent numbering."
                );
            }
            // #3723: verbless, color-coded rows — the restart-vs-resume verb
            // was an implementation detail of HOW the session gets reached
            // (decided at selection time by `guided_resume::plan_resume`),
            // not a property of the session's current state; the state word
            // (already server-reconciled against live tmux — #3302/#3714,
            // never re-derived here) now carries that signal via color as
            // well as text. See `session_picker_render::format_session_row`.
            for (i, s) in sessions.iter().enumerate() {
                let num = shown_slot(&sessions, i);
                eprintln!(
                    "{}",
                    super::session_picker_render::format_session_row(num, s, use_color)
                );
            }
            eprintln!("tm:   [{new_idx}] launch new session");
            eprintln!("tm:   [d<N>] delete session N (e.g. d1)");
            eprintln!("tm:   [r<N> <new-name>] rename session N (e.g. r1 tm-my-new-name)");
            eprintln!("tm:   [q] quit");
            let first = &sessions[0];
            let first_num = shown_slot(&sessions, 0);
            if first.deleted {
                eprintln!("tm: [Enter] is a DELETED slot — type another number, or [q] to quit");
            } else if first.unresumable {
                eprintln!(
                    "tm: [Enter] is DEAD (workspace removed) — type [d{first_num}] to remove it, or choose another"
                );
            } else if first_needs_restart {
                // #2148: no implicit destructive default — an explicit number is required.
                eprintln!(
                    "tm: [Enter] does NOT restart — type [{first_num}] to confirm the restart"
                );
            } else {
                eprintln!("tm: default: [{first_num}] resume most recent");
            }
        }
        eprint!("tm: > ");

        let mut line = String::new();
        let n = std::io::stdin()
            .read_line(&mut line)
            .context("failed to read choice from stdin")?;
        if n == 0 {
            break;
        } // EOF (Ctrl-D): exit cleanly.

        match parse_picker_choice(&line, &sessions, first_needs_restart) {
            PickerDecision::Quit => {
                eprintln!("tm: quit.");
                break;
            }
            // #1742: route through daemon resume when the session is stopped or its
            // tmux session is absent — never raw-attach a non-live session.
            // #2678: a `switch-client` hand-off (or a fail-closed skip) means this
            // pane is no longer visible to the operator — stop before looping back
            // to `stdin`.
            PickerDecision::Resume(i) => {
                let outcome =
                    super::guided_resume::resume_guided_session(client, url, &sessions[i]).await?;
                if outcome.ends_interactive_loop() {
                    break;
                }
            }
            PickerDecision::LaunchNew => match scope.repo_url.as_deref() {
                Some(repo) => {
                    let outcome =
                        super::guided_launch::launch_new_session_and_attach(client, url, repo)
                            .await?;
                    if outcome.ends_interactive_loop() {
                        break;
                    }
                }
                None => {
                    // Fleet-wide `tm ls` has no single launch target — steer the
                    // operator to the project directory or an explicit spawn.
                    eprintln!(
                        "tm: launch-new is unavailable from this list view — run `tm` from a \
                         project directory, or `tm session new <repo-url>`."
                    );
                    continue;
                }
            },
            // #2148: bare Enter hit a session that needs a restart — ask for an
            // explicit choice instead of silently destroying its pane. No daemon
            // round-trip; redisplay the SAME menu.
            PickerDecision::ConfirmRestart(i) => {
                eprintln!(
                    "tm: '{}' requires a restart (its pane is gone or will be recreated) — \
                     bare Enter no longer does this automatically.",
                    sessions[i].name
                );
                eprintln!(
                    "tm: type [{}] to confirm the restart, or [q] to quit.",
                    shown_slot(&sessions, i)
                );
                continue;
            }
            // #2595: selection targeted a dead session — its workspace no longer
            // exists anywhere on disk, so a resume/restart is guaranteed to fail
            // (#2577/#2594). No daemon round-trip; point at the delete remedy and
            // redisplay the SAME menu.
            PickerDecision::Unresumable(i) => {
                eprintln!(
                    "tm: '{}' is dead — its workspace no longer exists anywhere on disk; \
                     resuming it would only fail.",
                    sessions[i].name
                );
                eprintln!(
                    "tm: run [d{}] to remove the record, or choose another session.",
                    shown_slot(&sessions, i)
                );
                continue;
            }
            // #3034: selection resolved to a slot the daemon reports as
            // deleted — this is the exact misdirection #3034 reports: a
            // number captured from an earlier listing must error clearly
            // rather than silently falling through to a neighboring session.
            // No daemon round-trip; redisplay the SAME menu.
            PickerDecision::SlotDeleted(i) => {
                eprintln!(
                    "tm: session [{}] was deleted — choose another number, or [q] to quit.",
                    shown_slot(&sessions, i)
                );
                continue;
            }
            // #2304: delete the selected session. The confirm/force-confirm
            // prompt and the managed→local routing live in `picker_delete`; this
            // arm just runs it, then falls through to the re-fetch so the menu
            // reflects the removal. A cancel/refusal is a no-op re-fetch.
            PickerDecision::Delete(i) => {
                super::picker_delete::confirm_and_delete(client, url, &sessions[i]).await?;
            }
            // #3724: rename the selected session through the existing
            // hardened `commands::rename::do_rename_request` path (the same
            // PATCH `tm sessions rename` issues); the driver prints the
            // outcome and never propagates a rename failure as a fatal
            // error, then falls through to the re-fetch so the menu reflects
            // the (possibly auto-suffixed, #3692) new name.
            PickerDecision::Rename(i, new_name) => {
                super::session_picker_rename::rename_selected(client, url, &sessions[i], new_name)
                    .await?;
            }
            PickerDecision::Unrecognised => {
                eprintln!("tm: unrecognised choice '{}'; quitting.", line.trim());
                break;
            }
        }

        // Detached or session ended — re-fetch the list before redisplaying.
        // #1809: the shared fetch path applies the same live-only tombstone filter.
        // Issue TBD: re-apply the scope's filter/sort so the redisplayed menu
        // never drifts from the one the operator picked against.
        sessions = fetch_live_sessions(client, url, scope.source_id.as_deref(), false).await?;
        sessions = filter_sessions_by_term(sessions, scope.term.as_deref());
        sort_sessions(&mut sessions, scope.sort);
    }
    Ok(())
}

/// Decide whether `tm ls` should open the interactive picker or print statically.
///
/// Why: a testable seam that folds every gate into one pure decision so the
/// non-TTY / `--json` / `--all` / empty-list branches are unit-testable without a
/// live terminal or daemon. Requiring BOTH stdin and stdout to be TTYs is the
/// anti-hang guarantee: a piped input (would EOF) or a piped output (must stay a
/// clean pipeable table) both fall through to static output.
/// What: returns `true` only when stdin AND stdout are TTYs, neither `--json` nor
/// `--all` was requested, and at least one session exists. `--all` forces static
/// output because its purpose is the forensic full list (including
/// decommissioned tombstones), not connecting.
/// Test: `ls_connector_should_show_picker_*` in `tests_behavior_d_tests.rs`.
pub(crate) fn should_show_picker(
    stdin_tty: bool,
    stdout_tty: bool,
    json: bool,
    all: bool,
    session_count: usize,
) -> bool {
    stdin_tty && stdout_tty && !json && !all && session_count > 0
}

/// `tm ls` — the interactive managed-session connector (top-level).
///
/// Why: bare `tm ls` should do the most useful thing for connecting to the
/// managed fleet: on a real terminal it opens the session picker; piped or
/// scripted it degrades to the same static, pipeable list as `tm session ls`.
/// What: resolves the scope (`--current` derives `owner/repo` from the cwd git
/// remote, mirroring `tm session ls`); routes `--json`, `--all`, or any non-TTY
/// invocation straight to the static [`super::managed::session_ls`] renderer
/// (preserving its raw `--json` passthrough byte-for-byte); otherwise fetches the
/// live sessions once and either renders the static table (0 sessions) or opens
/// [`run_tty_picker`] (≥1 session). Launch-new inside the picker targets the cwd
/// project only when it is a GitHub-backed git checkout.
/// Test: parse tests `cli_parses_ls_*` and the gate tests
/// `ls_connector_should_show_picker_*` in `tests_behavior_d_tests.rs`.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn run_ls_connector(
    client: &reqwest::Client,
    url: &str,
    json: bool,
    source_id: Option<String>,
    current: bool,
    all: bool,
    sort: SessionSortArg,
    term: Option<String>,
) -> anyhow::Result<()> {
    // `--current` derives the source_id from the cwd git remote, exactly like
    // `tm session ls --current`. `--source-id` and `--current` are mutually
    // exclusive at the clap layer, so at most one branch supplies a filter.
    let sid: Option<String> = if current {
        super::session::derive_source_id_from_cwd()
    } else {
        source_id
    };

    let stdin_tty = std::io::stdin().is_terminal();
    let stdout_tty = std::io::stdout().is_terminal();

    // Cheap pre-gate: `--json`, `--all`, or any non-interactive stream never
    // fetches for the picker — delegate straight to the static renderer, which
    // owns the raw `--json` passthrough and the `--all` tombstone sort. `sort`/
    // `term` ride along (the static renderer applies them; `--json` ignores
    // them, matching `--all`'s existing "no effect on --json" precedent).
    if json || all || !stdin_tty || !stdout_tty {
        return super::managed::session_ls(client, url, json, sid.as_deref(), all, sort, term)
            .await;
    }

    // Interactive stream: fetch the live sessions once. On any fetch error
    // (daemon unreachable, HTTP failure) fall back to the static renderer so the
    // operator sees the same actionable error rather than a bare picker crash.
    let sessions = match fetch_live_sessions(client, url, sid.as_deref(), false).await {
        Ok(s) => s,
        Err(_) => {
            return super::managed::session_ls(
                client,
                url,
                false,
                sid.as_deref(),
                false,
                sort,
                term,
            )
            .await;
        }
    };
    let mut sessions = filter_sessions_by_term(sessions, term.as_deref());
    sort_sessions(&mut sessions, sort);

    if !should_show_picker(stdin_tty, stdout_tty, json, all, sessions.len()) {
        // 0 sessions on a TTY: print the static "no managed sessions" line rather
        // than an empty picker.
        super::managed::render_session_table(&sessions, sid.as_deref());
        return Ok(());
    }

    // ≥1 session on a real terminal → the interactive picker. Launch-new targets
    // the cwd project only when it is a GitHub-backed git checkout.
    let repo_url = std::env::current_dir()
        .ok()
        .and_then(|cwd| super::guided::derive_project(&cwd))
        .map(|(_sid, _workspace, git_root)| git_root.to_string_lossy().to_string());
    let scope = PickerScope {
        source_id: sid,
        repo_url,
        sort,
        term,
    };
    run_tty_picker(client, url, &scope, sessions).await
}

#[cfg(test)]
#[path = "session_picker_tests.rs"]
mod tests;
