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
//! the static `tm session ls` renderer and the picker. [`next_launch_slot`] is
//! the shared "launch new session" number computation (issue #3723) both
//! `run_tty_picker`'s render and `parse_picker_choice` use, kept in one place
//! so they cannot drift.
//! Row TEXT/color rendering lives in the sibling `session_picker_render`
//! module (verbless, color-coded — #3723); the picker's rename action lives
//! in `session_picker_rename` (#3724); the `tm ls` orchestrator that decides
//! between this picker and static output lives in `session_ls_connector`
//! (#4965) — all three extracted to keep this file under the 500-SLOC
//! production cap.
//!
//! Test: `parse_picker_choice` unit tests live in `tests_behavior_c_tests.rs`
//! (re-exported through `guided`);
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
    /// Launch a brand-new session, optionally with an operator-supplied name
    /// hint (#4965). `None` — the bare-Enter, `[N]` and bare-`n` paths — lets
    /// the daemon derive the name from the repo as it always has. `Some(leaf)`
    /// — typed as `n <name>` — carries the name ALREADY sanitized to the
    /// daemon's own kebab-case leaf form via
    /// [`trusty_common::session_naming::leaf_slug_from_hint`], never the raw
    /// typed string: the daemon's `name_hint` path runs the hint through
    /// `Path::file_name()`, which would silently collapse `feature/auth` and
    /// `hotfix/auth` to the same `auth`. Sanitizing here makes that step a
    /// no-op. Never empty — an unusable name resolves to
    /// [`Self::UnusableName`] instead.
    LaunchNew(Option<String>),
    /// User chose to quit without action.
    Quit,
    /// Input was not recognised; the caller quits cleanly.
    Unrecognised,
    /// `n <name>` was typed with a name that sanitizes to nothing — `n !!!`,
    /// `n ---` (#4965). Carries the raw text so the driver can quote it back.
    ///
    /// Why this is NOT `LaunchNew(None)`: falling back to the unnamed path
    /// would spawn a session in the daemon's shared `local` leaf namespace —
    /// a name LESS identifiable than the repo-derived default, produced by a
    /// command the operator typed specifically to make it MORE identifiable.
    /// A spawn is not undoable enough to guess at; the driver reprints the
    /// menu instead.
    UnusableName(String),
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
    /// Bulk-delete every session whose NAME matches a glob (#5533). Entered as
    /// `d <glob>` — e.g. `d tm-test-*`, `d *-01 --dry-run`. Reachable only when
    /// the remainder carries a glob metacharacter, so a mistyped `delete` still
    /// resolves to `Unrecognised` rather than a bulk destructive action. The
    /// driver previews the full match set and requires the match COUNT typed
    /// back before deleting anything, and never deletes a running session.
    DeleteGlob(super::picker_delete_glob::GlobDeleteRequest),
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
    /// Re-print the current session list in place (issue #3863). Entered as
    /// `ls` or `list` (case-insensitive). Not a selection — the driver takes
    /// no daemon action beyond the re-fetch it already runs after every
    /// dispatched choice, then redisplays the (refreshed) menu on the next
    /// loop iteration.
    ListSessions,
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
    pub(crate) term: Option<SessionFilter>,
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
    let raw = req.send().await?.error_for_status()?.text().await?;
    // #5007: every listing surface — `tm ls`, `tm ls --json`, the interactive
    // picker, the guided flow — comes through here, so this is the one place a
    // degraded store has to be announced to be announced everywhere.
    warn_if_store_degraded(&raw);
    Ok(raw)
}

/// Announce a degraded store on STDERR, above whatever the caller prints.
///
/// Why (#5027 review): the stream this writes to is the whole contract —
/// `tm ls --json` puts the response body on stdout, so a banner on stdout would
/// make the JSON unparseable for every consumer. Nothing asserted it, and
/// changing `eprintln!` to `println!` killed no test. Emitting from its own
/// function is what lets one be written.
/// What: writes [`store_degradation_banner`] to stderr, or nothing when the
/// listing is healthy.
/// Test: `store_banner_goes_to_stderr_so_json_stays_machine_readable`.
pub(crate) fn warn_if_store_degraded(raw: &str) {
    if let Some(banner) = store_degradation_banner(raw) {
        eprintln!("{banner}");
    }
}

/// The warning to print above a listing the daemon served from stale memory.
///
/// Why (#5007): on 2026-08-06 `tm ls` printed a perfectly normal fleet while
/// `sessions.json` was corrupt and EVERY write to it was failing. The list is
/// produced by a deliberate fallback to the daemon's last-known in-memory set,
/// and that fallback is worth keeping — a transient read error must not report
/// an empty fleet. What was wrong is that it was silent. This turns the same
/// fallback into a visible one.
/// What: `None` for a healthy or unparseable response (a body this client
/// cannot read is not evidence of store corruption, and the caller already
/// handles it). Otherwise a stderr banner carrying the daemon's own message —
/// which for corruption names the file, the byte offset, and the repair
/// command — so the JSON on stdout stays machine-readable.
/// Test: `store_banner_is_absent_for_a_healthy_listing`,
/// `store_banner_names_corruption_and_the_repair_command`,
/// `store_banner_marks_a_transient_read_failure_as_stale_not_corrupt`.
pub(crate) fn store_degradation_banner(raw: &str) -> Option<String> {
    let health = serde_json::from_str::<ManagedListResponse>(raw)
        .ok()?
        .store_health?;
    let headline = if health.corrupt {
        "tm: WARNING — the session store is CORRUPT. This list is the daemon's last-known \
         in-memory copy; every write to the store is failing."
    } else {
        "tm: WARNING — the session store could not be read. This list is the daemon's \
         last-known in-memory copy and may be stale."
    };
    Some(format!("{headline}\ntm:   {}", health.message))
}

/// Deserialize a raw managed-session list body — every row, unscoped.
///
/// Why: the listing-time auto-prune must see the FULL fleet, including the dead
/// rows the default view hides (#4994). Splitting the deserialize away from the
/// display scoping ([`scope_for_display`]) is what lets the pipeline run in the
/// only order that works: parse → prune → scope. Before #4994 the scoping ran
/// inside this function, upstream of the prune, which was harmless only because
/// nothing it dropped was ever prunable.
/// What: deserializes `ManagedListResponse` and hands back its `sessions`
/// verbatim, in the daemon's ascending-slot order.
/// Test: `scope_for_display_all_keeps_tombstone_in_slot_order` (via the pair),
/// `session_ls_prunes_dead_records_on_piped_invocation`.
pub(crate) fn parse_managed_sessions(raw: &str) -> anyhow::Result<Vec<ManagedSessionSummary>> {
    Ok(serde_json::from_str::<ManagedListResponse>(raw)?.sessions)
}

/// Apply the default-vs-`--all` display scoping to an already-pruned list.
///
/// Why: the static table and the picker share one filtering/sorting policy so
/// the two views never diverge (#1809/#1841).
/// What: when `all` is false, drops decommissioned records, soft-deleted
/// records, and (#4994) records the listing-time sweep classified dead, via
/// [`filter_live_sessions`] (a `"deleted"` slot tombstone, #3034, is NOT a
/// decommissioned record and always passes through this branch too — see
/// [`super::managed::is_live_session_state`]'s doc).
///
/// 🔴 `dead_ids` must be the set the sweep that just ran over THIS list
/// produced — never the wire's `unresumable` flag re-derived here. See
/// [`super::managed::is_live_session_state`] for the live session that
/// distinction keeps visible.
/// When `all` is true, keeps every row — live, decommissioned, dead, AND
/// tombstoned — but stable-sorts ONLY `"decommissioned"` records to the end.
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
/// session used to be. `[`render_session_table`](super::managed_render::render_session_table)
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
/// `scope_for_display_all_keeps_tombstone_in_slot_order` in
/// `commands/session_tests.rs` covers the sink-to-bottom exclusion this doc
/// describes.
pub(crate) fn scope_for_display(
    sessions: Vec<ManagedSessionSummary>,
    all: bool,
    dead_ids: &std::collections::HashSet<String>,
) -> Vec<ManagedSessionSummary> {
    if !all {
        return filter_live_sessions(sessions, dead_ids);
    }
    // Sink ONLY soft-retired "decommissioned" records — never "deleted"
    // slot tombstones, which must stay in their original slot position
    // (see this function's doc for the full reasoning).
    let mut sessions = sessions;
    sessions.sort_by_key(|sess| u8::from(sess.state == "decommissioned"));
    sessions
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

/// Which columns a filter term is matched against.
///
/// Why: `tm ls <term>` and `tm f <pattern>` share one filter/sort/render path
/// but ask different questions. `tm ls` matches everything the operator can see
/// on the row, which is the right default when they are hunting and only half
/// remember where the string lives. `tm f` is the narrow tool: "show me the
/// sessions CALLED this", and a task description mentioning the word must not
/// pad the answer. The distinction is data on the filter, not a second
/// filtering function, so neither behaviour can drift from the other.
/// What: [`Visible`](FilterScope::Visible) matches `id`, `name`, `source_id`,
/// `state`, and `task`; [`Name`](FilterScope::Name) matches `name` only.
/// Test: `filter_sessions_by_name_ignores_non_name_columns`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum FilterScope {
    /// Every column the table renders (`tm ls <term>`, #3483).
    Visible,
    /// The `NAME` column only (`tm f <pattern>`).
    Name,
}

/// A case-insensitive substring filter plus the columns it applies to.
///
/// Why: see [`FilterScope`] — the scope has to ride along with the term through
/// `run_ls_connector` → `session_ls` → the picker's re-fetch loop, so the
/// picker keeps filtering the way the invoking command asked.
/// What: an owned lowercase-compared needle and its [`FilterScope`]. Construct
/// via [`SessionFilter::visible`] or [`SessionFilter::name`].
/// Test: `filter_sessions_by_term_*`, `filter_sessions_by_name_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct SessionFilter {
    /// The needle, already lowercased at construction.
    needle_lower: String,
    /// Which columns [`SessionFilter::matches`] reads.
    scope: FilterScope,
}

impl SessionFilter {
    /// Filter over every visible column — the `tm ls` grammar's behaviour.
    pub(crate) fn visible(term: impl AsRef<str>) -> Self {
        Self::new(term, FilterScope::Visible)
    }

    /// Filter over the `NAME` column only — `tm f <pattern>`.
    pub(crate) fn name(term: impl AsRef<str>) -> Self {
        Self::new(term, FilterScope::Name)
    }

    fn new(term: impl AsRef<str>, scope: FilterScope) -> Self {
        Self {
            needle_lower: term.as_ref().to_lowercase(),
            scope,
        }
    }

    /// Does `s` match this filter?
    ///
    /// Why: a `None`/absent optional column must be skipped rather than treated
    /// as an empty string, which every term would trivially "contain".
    /// What: case-insensitive `contains` over the columns [`Self::scope`]
    /// selects.
    /// Test: `filter_sessions_by_term_*`, `filter_sessions_by_name_*`.
    pub(crate) fn matches(&self, s: &ManagedSessionSummary) -> bool {
        let fields: &[Option<&str>] = match self.scope {
            FilterScope::Visible => &[
                Some(s.id.as_str()),
                Some(s.name.as_str()),
                s.source_id.as_deref(),
                Some(s.state.as_str()),
                s.task.as_deref(),
            ],
            FilterScope::Name => &[Some(s.name.as_str())],
        };
        fields
            .iter()
            .flatten()
            .any(|field| field.to_lowercase().contains(&self.needle_lower))
    }
}

/// Apply a [`SessionFilter`] to a session list (#3483).
///
/// Why: the static table and the interactive picker must filter identically, so
/// both call this rather than open-coding the predicate.
/// What: keeps the sessions [`SessionFilter::matches`] accepts. `filter = None`
/// is a no-op (returns `sessions` unchanged).
/// Test: `filter_sessions_by_term_matches_name`,
/// `filter_sessions_by_term_matches_task`,
/// `filter_sessions_by_term_matches_source_id`,
/// `filter_sessions_by_term_is_case_insensitive`,
/// `filter_sessions_by_term_no_match_returns_empty`,
/// `filter_sessions_by_term_none_is_noop`,
/// `filter_sessions_by_name_ignores_non_name_columns`.
pub(crate) fn filter_sessions_by_term(
    sessions: Vec<ManagedSessionSummary>,
    filter: Option<&SessionFilter>,
) -> Vec<ManagedSessionSummary> {
    let Some(filter) = filter else {
        return sessions;
    };
    sessions.into_iter().filter(|s| filter.matches(s)).collect()
}

/// The attached→active→everything-else group a session belongs in (owner
/// request 2026-07-29).
///
/// Why: Bob's ask — the listing should group attached sessions first, then
/// active ones, then the rest (stopped/errored/provisioning/etc.) — ABOVE
/// whatever `recent`/`alpha` secondary order the operator picked, so a
/// session they're actively connected to never scrolls below a merely-recent
/// stopped one.
/// What: `0` when `s.attached` (a client is connected RIGHT NOW — the
/// strongest signal, mirrors [`session_picker_render::state_color`]'s own
/// precedence); `1` for `state == "active"` (not attached); `2` for every
/// other state. Lower sorts first.
/// Test: `sort_sessions_recent_groups_attached_before_active_before_stopped`,
/// `sort_sessions_alpha_groups_attached_before_active_before_stopped`.
fn group_rank(s: &ManagedSessionSummary) -> u8 {
    if s.attached {
        0
    } else if s.state == "active" {
        1
    } else {
        2
    }
}

/// Sort `sessions` in place per [`SessionSortArg`] (#3483), grouped
/// attached→active→everything-else (owner request 2026-07-29).
///
/// Why: shared by the static table (`tm ls` / `tm sessions ls`) and the
/// interactive picker so both views order sessions identically.
/// What: primary key is [`group_rank`] (attached, then active, then the
/// rest); within each group, `Recent` sorts descending by [`recency_key`]
/// (most recent first) and `Alpha` sorts ascending, case-insensitively, by
/// `name`. Both use the stable `sort_by`, so equal keys preserve the
/// daemon's original relative order.
/// Test: `sort_sessions_recent_orders_by_last_activity`,
/// `sort_sessions_recent_falls_back_to_created_at`,
/// `sort_sessions_alpha_orders_by_name_case_insensitive`,
/// `sort_sessions_recent_groups_attached_before_active_before_stopped`,
/// `sort_sessions_alpha_groups_attached_before_active_before_stopped`.
pub(crate) fn sort_sessions(sessions: &mut [ManagedSessionSummary], sort: SessionSortArg) {
    match sort {
        SessionSortArg::Recent => {
            sessions.sort_by(|a, b| {
                group_rank(a)
                    .cmp(&group_rank(b))
                    .then_with(|| recency_key(b).cmp(recency_key(a)))
            });
        }
        SessionSortArg::Alpha => {
            sessions.sort_by(|a, b| {
                group_rank(a)
                    .cmp(&group_rank(b))
                    .then_with(|| a.name.to_lowercase().cmp(&b.name.to_lowercase()))
            });
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
/// What: [`fetch_managed_raw`], then [`parse_managed_sessions`], then
/// [`super::session_picker_prune::prune_and_report`] — every session confirmed
/// dead on TWO consecutive listings is cleared from the registry (capped per
/// call) instead of sitting in the list forever printing "use [d<N>] to remove
/// the record" — and only THEN [`scope_for_display`]. That order is
/// load-bearing (#4994): the default view now hides dead rows, so scoping first
/// would hand the prune a list with its own targets already removed.
///
/// #4702: the prune used to be behind an `allow_auto_prune` opt-in that only
/// the TTY-gated call sites ever passed. That inconsistency is the defect —
/// piped/scripted/`--json` listings never pruned, so dead records accumulated
/// without bound.
///
/// Removing the opt-in is only defensible because the prune is now genuinely
/// non-destructive, and #4728 is why that qualifier is load-bearing rather than
/// decorative: `record_only` did NOT gate the runtime teardown until that fix,
/// so the very same sweep could SIGTERM and `kill_session` a live pane. What
/// makes running this on every listing safe is the conjunction of three pinned
/// guarantees, not the flag's name:
///   * no filesystem writes —
///     `decommission_record_only_never_removes_existing_workspace`;
///   * no runtime teardown —
///     `decommission_record_only_never_touches_the_runtime`;
///   * every request actually takes that route —
///     `auto_prune_always_requests_record_only_never_full_teardown`.
///
/// Test: HTTP path in `tests/session_manager_mvp.rs`; the parse/filter seam is
/// unit-tested via `scope_for_display`; the auto-prune seam by
/// `auto_prune_*` in `tests_behavior_d_tests.rs`.
pub(crate) async fn fetch_live_sessions(
    client: &reqwest::Client,
    url: &str,
    source_id: Option<&str>,
    all: bool,
) -> anyhow::Result<Vec<ManagedSessionSummary>> {
    let raw = fetch_managed_raw(client, url, source_id).await?;
    let fetched = parse_managed_sessions(&raw)?;
    // `!all` is exactly whether this call will drop the dead rows, which is what
    // decides the banner's "hidden" suffix (#4994).
    let listing = super::session_picker_prune::prune_and_report(client, url, fetched, !all).await;
    Ok(scope_for_display(listing.sessions, all, &listing.dead_ids))
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
pub(crate) fn decide_for_index(
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

/// Recognise the `n` launch-new command and return its raw argument (#4965).
///
/// Why the token is matched EXACTLY rather than stripped like `d<N>`/`r<N>`:
/// those prefixes are safe only because their remainder must still parse as a
/// live slot number, so `d2` is unambiguous while `delete` falls through to
/// `Unrecognised`. `n` has no such gate — an unbounded `strip_prefix(['n','N'])`
/// makes EVERY n-initial word a spawn with no confirmation, and the menu line
/// itself reads "launch new session", so an operator typing `new` gets a real
/// cloned, spawned, attached session named `tm-ew-NN`. `n` is also already the
/// "no" answer in this tool's own `[y/N]` delete confirm. So: the token before
/// the first whitespace must be exactly `n`/`N`, and a name requires a
/// separator — the no-space `nauth` form is deliberately NOT accepted.
///
/// What: `None` when `choice` is not the `n` command at all (`new`, `no`,
/// `nn`, `n1`, `nauth`). `Some("")` for a bare `n`/`N`, or one whose remainder
/// is only whitespace. `Some(name)` — trimmed, possibly multi-word, NOT yet
/// sanitized — for `n <name>`. `choice` must already be trimmed.
/// Test: `guided_picker_n_grammar_is_bounded`,
/// `guided_picker_n_launches_new_unnamed`,
/// `guided_picker_n_with_argument_carries_name_hint`.
fn launch_new_argument(choice: &str) -> Option<&str> {
    let (token, rest) = match choice.split_once(char::is_whitespace) {
        Some((token, rest)) => (token, rest.trim()),
        None => (choice, ""),
    };
    token.eq_ignore_ascii_case("n").then_some(rest)
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
///   • `"ls"` / `"list"` (case-insensitive, #3863) → `ListSessions` — re-print
///     the current list in place, never a selection
///   • empty / whitespace, `sessions` empty → `LaunchNew(None)`
///   • empty / whitespace, `sessions` non-empty → [`decide_for_index`] on
///     position 0, gated by `first_needs_restart`
///   • `N` matching some `sessions[i].slot` → [`decide_for_index`] on `i`
///     (an EXPLICIT numeric choice never applies the restart-confirm gate —
///     it always dispatches directly, confirm or not, per #2148)
///   • `N` == `(highest displayed slot) + 1` (or `1` when `sessions` is
///     empty) → `LaunchNew(None)`
///   • exactly `n` / `N` (or one whose remainder is only whitespace) →
///     `LaunchNew(None)` — a slot-independent alias for the numeric
///     launch-new choice (#4965)
///   • `n <name>` — whitespace separator REQUIRED — → `LaunchNew(Some(leaf))`,
///     where `leaf` is the name already kebab-cased by
///     [`trusty_common::session_naming::leaf_slug_from_hint`], or
///     `UnusableName(raw)` when nothing alphanumeric survives. `new`, `no`,
///     `nn`, `n1`, `nauth` are `Unrecognised` — see [`launch_new_argument`]
///   • `d<N>` / `d <N>` matching a LIVE `sessions[i].slot` → `Delete(i)`
///     (#2304); the driver still runs a confirm/force-confirm prompt before
///     deleting. Matching a TOMBSTONED slot → `SlotDeleted(i)` (#3034 — no
///     double-delete, no silent no-op).
///   • `d <glob>` whose remainder is not a slot number but DOES contain a glob
///     metacharacter (`*`, `?`, `[`) → `DeleteGlob` (#5533) — a bulk delete by
///     session name, previewed and count-confirmed by the driver. A remainder
///     with no metacharacter (`delete`, an unknown name, an out-of-range
///     number) stays `Unrecognised`, so a typo can never start a bulk action.
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
/// `guided_picker_launch_new_uses_highest_slot_with_gaps`,
/// `guided_picker_ls_returns_list_sessions`,
/// `guided_picker_list_returns_list_sessions`,
/// `guided_picker_n_launches_new_unnamed`,
/// `guided_picker_n_with_argument_carries_name_hint`,
/// `guided_picker_n_grammar_is_bounded`,
/// `guided_picker_n_sanitizes_the_name_cli_side`,
/// `guided_picker_n_does_not_shadow_other_commands`.
pub(crate) fn parse_picker_choice(
    line: &str,
    sessions: &[ManagedSessionSummary],
    first_needs_restart: bool,
) -> PickerDecision {
    let choice = line.trim();
    if choice.eq_ignore_ascii_case("q") {
        return PickerDecision::Quit;
    }
    // #3863: `ls` / `list` re-print the current list in place — checked
    // before the rename (`r<N>`)/delete (`d<N>`) prefix branches and the
    // bare-Enter/numeric branches below since neither is a valid session
    // slot number nor shares a prefix with them.
    if choice.eq_ignore_ascii_case("ls") || choice.eq_ignore_ascii_case("list") {
        return PickerDecision::ListSessions;
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
        if let Some(idx) = rest
            .trim()
            .parse::<u32>()
            .ok()
            .and_then(|n| find_slot(sessions, n))
        {
            return match sessions[idx].deleted {
                true => PickerDecision::SlotDeleted(idx),
                false => PickerDecision::Delete(idx),
            };
        }
        // #5533: not a slot number — accept the bulk `d <glob>` form. The
        // remainder must carry a glob metacharacter, which is what keeps a
        // mistyped `delete` falling through to `Unrecognised` (see
        // `picker_delete_glob::parse_glob_delete`).
        return match super::picker_delete_glob::parse_glob_delete(rest) {
            Some(req) => PickerDecision::DeleteGlob(req),
            None => PickerDecision::Unrecognised,
        };
    }
    // #4965: `n` / `n <name>` launches a new session — see
    // [`launch_new_argument`] for the grammar and why it is exact-match rather
    // than a `d<N>`/`r<N>`-style prefix strip.
    if let Some(name) = launch_new_argument(choice) {
        if name.is_empty() {
            return PickerDecision::LaunchNew(None);
        }
        // Sanitize HERE, not daemon-side. `resolve_session_name` feeds a
        // `name_hint` to `leaf_slug_from_dir`, whose `Path::file_name()` step
        // drops everything before the last `/` — `n feature/auth` and
        // `n hotfix/auth` would both become `tm-auth-NN`. Slugifying with the
        // same function and the same cap makes the daemon's pass a no-op, so
        // what the operator sees here is the leaf the session actually gets.
        let slug = trusty_common::session_naming::leaf_slug_from_hint(name);
        return match slug.is_empty() {
            true => PickerDecision::UnusableName(name.to_string()),
            false => PickerDecision::LaunchNew(Some(slug)),
        };
    }
    if choice.is_empty() {
        if sessions.is_empty() {
            return PickerDecision::LaunchNew(None);
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
            return PickerDecision::LaunchNew(None);
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
///   • `LaunchNew(name_hint)` → [`super::guided_launch::launch_new_session_and_attach`]
///     when the scope carries a `repo_url`, forwarding the already-sanitized
///     `n <name>` leaf (#4965) when one was typed — and printing a one-line
///     note first when that leaf differs from the typed text; otherwise prints
///     an actionable hint and redisplays the menu (fleet-wide `tm ls` has no
///     single launch target);
///   • `UnusableName(typed)` (#4965) → print "no usable name characters" and
///     redisplay the SAME menu — never a spawn under a fallback name;
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
///   • `ListSessions` (#3863) → no daemon action beyond the unconditional
///     re-fetch that already runs at the bottom of every loop iteration —
///     the next iteration reprints the (refreshed) menu, which is exactly
///     "re-print the list";
///   • `Unrecognised` (#3863) → print a one-line hint of the accepted input
///     tokens and redisplay the SAME menu — this used to quit the picker
///     outright, which was the wrong failure mode for a plain typo;
///   • `Quit` / EOF → print notice and return `Ok`.
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
        // #4965: the `[key] description` legend is built by
        // `session_picker_render::command_legend` — column-aligned in BOTH
        // menus (the populated one used to be ragged), and pure so its wording
        // is testable without driving this loop.
        if sessions.is_empty() {
            for l in super::session_picker_render::command_legend(None) {
                eprintln!("{l}");
            }
        } else {
            if stale_slots {
                // #4230: `tm restart` is a hard error where launchd owns the
                // daemon, so name the verb that works on THIS host.
                eprintln!(
                    "tm: warning — the running daemon appears to predate stable session \
                     numbering (issue #3034); the numbers below are positional for this \
                     listing only — run `{}` to restore permanent numbering.",
                    crate::commands::launchd_probe::daemon_restart_command()
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
            // #4965: `n` is the slot-independent alias for the `[{new_idx}]`
            // launch-new row — that number shifts as sessions come and go, so
            // it is nothing an operator can memorize, and it cannot carry a
            // name.
            for l in super::session_picker_render::command_legend(Some(new_idx)) {
                eprintln!("{l}");
            }
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
                // #4965: the hint also disowns `n` as "no"; it is the launch-new alias.
                eprintln!(
                    "tm: [Enter] does NOT restart — {}",
                    super::session_picker_render::restart_confirm_hint(first_num)
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
            PickerDecision::LaunchNew(name_hint) => match scope.repo_url.as_deref() {
                Some(repo) => {
                    // #4965: the name is kebab-cased before it is sent, so
                    // `n My Auth Fix!` produces `tm-my-auth-fix-NN`. Say so
                    // when the result differs from what was typed — a spawn
                    // is not cheap to undo, and a silently rewritten name is
                    // exactly the surprise this command exists to remove.
                    // `launch_new_argument` is the SAME parser
                    // `parse_picker_choice` used, so the raw text cannot drift
                    // from the grammar; it returns `None` for the bare-Enter
                    // and numeric launch paths, which have nothing to compare.
                    if let (Some(raw), Some(slug)) =
                        (launch_new_argument(line.trim()), name_hint.as_deref())
                        && raw != slug
                    {
                        eprintln!("tm: naming it '{slug}' (from '{raw}').");
                    }
                    let outcome = super::guided_launch::launch_new_session_and_attach(
                        client,
                        url,
                        repo,
                        name_hint.as_deref(),
                    )
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
                    "tm: {}",
                    super::session_picker_render::restart_confirm_hint(shown_slot(&sessions, i))
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
            // #5533: bulk delete by name glob. The preview, the count-confirm
            // prompt, and the running-session guard live in
            // `picker_delete_glob`; deletions route through the same
            // `delete_managed_then_local` the single-slot arm above uses. Every
            // non-destructive outcome (no match, nothing deletable, dry run,
            // bad pattern, cancel) returns 0 and falls through to the re-fetch.
            PickerDecision::DeleteGlob(ref req) => {
                super::picker_delete_glob::confirm_and_delete_glob(client, url, &sessions, req)
                    .await?;
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
            // #3863: `ls`/`list` re-print the current list in place — no
            // daemon action of its own; simply fall through to the
            // unconditional re-fetch below so the next loop iteration
            // redisplays a freshly-fetched menu.
            PickerDecision::ListSessions => {}
            // #3863: an unrecognised choice used to quit the picker outright
            // (a startling failure mode for a plain typo). Print a one-line
            // hint of the accepted tokens and redisplay the SAME menu
            // instead — no daemon round-trip.
            PickerDecision::Unrecognised => {
                eprintln!(
                    "tm: unrecognised choice '{}' — accepted: 1..{new_idx}, n [<name>], \
                     ls, d<N>, d <glob>, r<N> <name>, q",
                    line.trim()
                );
                continue;
            }
            // #4965: `n <name>` whose name sanitizes to nothing. Spawning
            // anyway would put the session in the daemon's shared `local`
            // leaf namespace — less identifiable than the default the
            // operator was trying to improve on. No daemon round-trip;
            // redisplay the SAME menu.
            PickerDecision::UnusableName(typed) => {
                eprintln!(
                    "tm: '{typed}' has no usable name characters — try `n <letters>` \
                     (e.g. n auth-refactor)."
                );
                continue;
            }
        }

        // Detached or session ended — re-fetch the list before redisplaying.
        // #1809: the shared fetch path applies the same live-only tombstone filter.
        // Issue TBD: re-apply the scope's filter/sort so the redisplayed menu
        // never drifts from the one the operator picked against.
        sessions = fetch_live_sessions(client, url, scope.source_id.as_deref(), false).await?;
        sessions = filter_sessions_by_term(sessions, scope.term.as_ref());
        sort_sessions(&mut sessions, scope.sort);
    }
    Ok(())
}

#[cfg(test)]
#[path = "session_picker_tests.rs"]
mod tests;
