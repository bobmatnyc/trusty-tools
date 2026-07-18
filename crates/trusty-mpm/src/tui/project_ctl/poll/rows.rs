//! Pure DTO → pane-row projections for the `tm projects` TUI poll pipeline.
//!
//! Why: split out of `poll.rs` (#2476) to keep that file under the 500-SLOC
//! production cap. [`super::fetch_projects_and_sessions`] and
//! [`super::refresh_activity`] both need to turn a daemon wire DTO into the
//! render-ready shape its pane consumes; keeping that projection logic here,
//! with zero async and zero [`crate::client::DaemonClient`] dependency,
//! means every rule is directly `#[test]`-able against hand-built DTOs — no
//! dead-loopback client, no tokio runtime.
//! What: [`project_to_row`], [`live_session_rows`] (drops decommissioned
//! sessions, #2476), [`session_to_row`], [`activity_from_response`], and the
//! small [`tail_lines`] helper the last of those uses.
//! Test: `tests` covers every function directly against hand-built DTOs.

use crate::client::{FleetProjectGroupWire, ManagedActivityResponse, ManagedSessionSummary};
use crate::project::Project;
use crate::tui::coordinator::rows::session_short_id;
use crate::tui::project_ctl::state::{ActivityInfo, ProjectRow, SessionRow};

/// How many trailing lines of a session's raw tmux pane the Activity pane
/// previews (DOC-35 §5.1 mockup: "last 3 lines of raw_pane").
const RAW_PANE_TAIL_LINES: usize = 3;

/// Project one registry [`Project`] (plus its matching fleet group, if any)
/// into a [`ProjectRow`].
///
/// Why: the Projects pane needs the aggregate-state glyph + live session
/// count (DOC-35 §5), not the raw registry/fleet DTOs.
/// What: `live_count` is the number of sessions in `group` whose `state` is
/// `"active"` or `"provisioning"`; `total_count` is `group.sessions.len()`.
/// A project with no matching fleet group (never spawned a session) gets
/// `0`/`0`.
/// Test: `project_to_row_counts_live_and_total`,
/// `project_to_row_missing_group_is_zeroed`.
pub(crate) fn project_to_row(p: &Project, group: Option<&FleetProjectGroupWire>) -> ProjectRow {
    let (live_count, total_count) = match group {
        Some(g) => (
            g.sessions
                .iter()
                .filter(|s| matches!(s.state.as_str(), "active" | "provisioning"))
                .count(),
            g.sessions.len(),
        ),
        None => (0, 0),
    };
    ProjectRow {
        name: p.name.clone(),
        repo_url: p.repo_url.clone(),
        live_count,
        total_count,
    }
}

/// Project one fleet group's sessions into Sessions-pane rows, dropping
/// decommissioned (tombstoned) ones (#2476).
///
/// Why: decommissioning a session (via `tm sessions decommission`, the TUI's
/// own `d` action, or the idle reaper) flips the session record's `state` to
/// `"decommissioned"` in the daemon's session store, but does NOT delete the
/// record — that is a separate, explicit `session_delete`/prune step. Since
/// [`super::fetch_projects_and_sessions`] rebuilds `sessions_by_project` from
/// scratch off the fleet response on every poll tick, an unfiltered
/// projection would let a decommissioned session's row persist in the
/// Sessions pane — and inflate its `(N)` header count — forever, even though
/// the session it names is gone for good and its live-refresh "removal"
/// never has anywhere else to happen. The pane header counts *sessions*, so
/// a permanently-visible tombstone row is the bug, not a feature (issue
/// #2476's own analysis, confirmed here: decommissioned rows were reachable
/// through [`session_to_row`]/[`crate::tui::project_ctl::panes::sessions::state_glyph`]
/// but no caller ever dropped them). Filtering here — once, at the
/// projection step — means every downstream consumer (`sessions_nav`'s
/// selection model, the Sessions pane render, the `(N)` header) sees only
/// live rows and never has to special-case a dead one.
/// What: keeps every session whose `state` is not TERMINAL (`decommissioned`
/// OR `deleted`, #2012) and projects each survivor via [`session_to_row`]. The
/// terminal check is driven off [`ManagedSessionState::is_terminal`] (via
/// [`ManagedSessionState::from_wire`]) — the SAME enum source of truth
/// `managed::is_live_session_state` uses — so a new terminal variant (like the
/// `--deleted--` tombstone) can never silently slip past a hardcoded
/// `== "decommissioned"` string and clutter the pane / inflate its `(N)` count.
/// A session later replaced by a new session that reuses its `name` (different
/// `id`) can never ghost or duplicate: this rebuilds the row list wholesale
/// from the daemon's current fleet snapshot on every tick rather than diffing
/// against the previous tick's rows, so the terminal original (filtered out)
/// and the new session (kept, its own row) never coexist.
/// Test: `live_session_rows_drops_decommissioned`,
/// `live_session_rows_drops_deleted`,
/// `live_session_rows_keeps_live_states`,
/// `live_session_rows_same_name_replacement_is_clean`.
pub(crate) fn live_session_rows(sessions: Vec<ManagedSessionSummary>) -> Vec<SessionRow> {
    sessions
        .into_iter()
        .filter(|s| {
            !crate::session_manager::ManagedSessionState::from_wire(&s.state)
                .is_some_and(|st| st.is_terminal())
        })
        .map(session_to_row)
        .collect()
}

/// Project one fleet [`ManagedSessionSummary`] into a [`SessionRow`].
///
/// Why: the Sessions pane needs a numbered, compact row per session; the
/// fleet poll is one call for every session regardless of count, so this
/// projection stays over the STATIC record fields even after #2119 — only the
/// one selected session's activity gets the extra live `/activity` fetch (see
/// [`super::refresh_activity`]), never the whole list.
/// What: derives the 8-hex short id via [`session_short_id`] and copies the
/// rest through unchanged, including `deliverable_id` (DOC-35 §10.6, #2383 —
/// drives the Sessions-pane deliverable glyph) and `unresumable` (#2595 —
/// gates the `r` resume key).
/// Test: `session_to_row_derives_short_id_and_copies_fields`,
/// `session_to_row_carries_deliverable_id`,
/// `session_to_row_carries_unresumable_flag`.
pub(crate) fn session_to_row(s: ManagedSessionSummary) -> SessionRow {
    let short_id = session_short_id(&s.id);
    SessionRow {
        id: s.id,
        short_id,
        name: s.name,
        branch: s.branch,
        task: s.task,
        state: s.state,
        pending_decision: s.pending_decision,
        proposed_default: s.proposed_default,
        deliverable_id: s.deliverable_id,
        unresumable: s.unresumable,
    }
}

/// Project a [`ManagedActivityResponse`] into an [`ActivityInfo`] for `session_id`.
///
/// Why: the Activity pane needs a lean, render-ready shape (DOC-35 §5.4) with
/// a bounded pane-tail preview rather than the full wire response.
/// What: copies `state`/`summary`/`pending_decision`/`proposed_default`
/// through unchanged, tags the snapshot with `session_id`, sets `stale` to
/// `false` (a just-succeeded fetch is by definition fresh), and derives
/// `raw_pane_tail` as the last [`RAW_PANE_TAIL_LINES`] non-empty lines of
/// `raw_pane` via [`tail_lines`].
/// Test: `activity_from_response_derives_fields_and_tail`,
/// `activity_from_response_handles_short_pane`.
pub(crate) fn activity_from_response(
    session_id: String,
    resp: ManagedActivityResponse,
) -> ActivityInfo {
    ActivityInfo {
        session_id,
        state: resp.state,
        summary: resp.summary,
        pending_decision: resp.pending_decision,
        proposed_default: resp.proposed_default,
        raw_pane_tail: tail_lines(&resp.raw_pane, RAW_PANE_TAIL_LINES),
        stale: false,
    }
}

/// The last `n` non-empty, trimmed lines of `text`, in original order.
///
/// Why: a raw tmux pane capture is typically newline-padded and much longer
/// than the Activity pane's fixed 4-row height can show; a small pure helper
/// keeps the "which lines" rule unit-testable independent of rendering.
/// What: splits on `\n`, trims each line, drops empty ones, then keeps the
/// last `n` (or fewer, if `text` has fewer than `n` non-empty lines).
/// Test: `tail_lines_keeps_last_n_non_empty_lines`,
/// `tail_lines_handles_fewer_lines_than_requested`.
fn tail_lines(text: &str, n: usize) -> Vec<String> {
    let non_empty: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    let start = non_empty.len().saturating_sub(n);
    non_empty[start..].iter().map(|l| l.to_string()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn project(name: &str) -> Project {
        Project {
            name: name.to_string(),
            repo_url: format!("https://github.com/acme/{name}"),
            default_branch: "main".to_string(),
            stack_hint: None,
            tags: vec![],
            description: None,
            gh_user: None,
            gh_account: None,
            github: None,
            commit_name: None,
            commit_email: None,
        }
    }

    fn summary(id: &str, state: &str) -> ManagedSessionSummary {
        ManagedSessionSummary {
            id: id.to_string(),
            name: format!("s-{id}"),
            state: state.to_string(),
            persisted_state: None,
            workspace_path: None,
            repo_url: None,
            branch: Some("main".to_string()),
            created_at: None,
            last_activity_at: None,
            pending_decision: None,
            proposed_default: None,
            source_id: None,
            task: Some("do the thing".to_string()),
            cwd: None,
            claude_session_id: None,
            deliverable_id: None,
            pane_id: None,
            injection_status: None,
            unresumable: false,
            stale_assets: false,
attached: false,
slot: 0,
deleted: false,
        }
    }

    #[test]
    fn project_to_row_counts_live_and_total() {
        let p = project("widget");
        let group = FleetProjectGroupWire {
            project_name: "widget".to_string(),
            repo_url: p.repo_url.clone(),
            sessions: vec![
                summary("a1111111", "active"),
                summary("b2222222", "provisioning"),
                summary("c3333333", "stopped"),
            ],
        };
        let row = project_to_row(&p, Some(&group));
        assert_eq!(row.live_count, 2);
        assert_eq!(row.total_count, 3);
        assert_eq!(row.name, "widget");
    }

    #[test]
    fn project_to_row_missing_group_is_zeroed() {
        let p = project("widget");
        let row = project_to_row(&p, None);
        assert_eq!(row.live_count, 0);
        assert_eq!(row.total_count, 0);
    }

    #[test]
    fn session_to_row_derives_short_id_and_copies_fields() {
        let row = session_to_row(summary("4f9ca1b2ffff", "active"));
        assert_eq!(row.short_id, "4f9ca1b2");
        assert_eq!(row.id, "4f9ca1b2ffff");
        assert_eq!(row.state, "active");
        assert_eq!(row.branch.as_deref(), Some("main"));
        assert_eq!(row.task.as_deref(), Some("do the thing"));
    }

    #[test]
    fn session_to_row_carries_deliverable_id() {
        let mut s = summary("4f9ca1b2ffff", "active");
        s.deliverable_id = Some("11111111-1111-1111-1111-111111111111".to_string());
        let row = session_to_row(s);
        assert_eq!(
            row.deliverable_id.as_deref(),
            Some("11111111-1111-1111-1111-111111111111")
        );

        let none_row = session_to_row(summary("aaaaaaaabbbb", "active"));
        assert!(none_row.deliverable_id.is_none());
    }

    /// #2595: the `unresumable` flag must survive the DTO → row projection —
    /// this is what lets `events::request_resume` refuse a dead session.
    #[test]
    fn session_to_row_carries_unresumable_flag() {
        let mut s = summary("dead0001", "stopped");
        s.unresumable = true;
        let row = session_to_row(s);
        assert!(row.unresumable, "unresumable must copy through as true");

        let healthy_row = session_to_row(summary("healthy1", "stopped"));
        assert!(
            !healthy_row.unresumable,
            "a healthy session must copy through as false"
        );
    }

    // ---- #2476: decommissioned sessions never reach the Sessions pane ----

    #[test]
    fn live_session_rows_drops_decommissioned() {
        // (a) a session present, then decommissioned, drops out of the row
        // list — and with it, the Sessions pane's `(N)` header count.
        let sessions = vec![
            summary("a1111111", "active"),
            summary("b2222222", "decommissioned"),
        ];
        let rows = live_session_rows(sessions);
        assert_eq!(
            rows.len(),
            1,
            "decommissioned row must be dropped: {rows:?}"
        );
        assert_eq!(rows[0].id, "a1111111");
    }

    #[test]
    fn live_session_rows_drops_deleted() {
        // #2012: a soft-deleted (`--deleted--`) session is TERMINAL and must be
        // dropped from the Sessions pane exactly like a decommissioned one — a
        // permanently-visible tombstone row would clutter the pane and inflate
        // its `(N)` header count.
        let sessions = vec![
            summary("a1111111", "active"),
            summary("d2222222", "deleted"),
        ];
        let rows = live_session_rows(sessions);
        assert_eq!(rows.len(), 1, "deleted row must be dropped: {rows:?}");
        assert_eq!(rows[0].id, "a1111111");
    }

    #[test]
    fn live_session_rows_keeps_live_states() {
        // (b) the add path: every non-decommissioned state still projects a
        // row (active/provisioning/stopped/errored all render distinct
        // glyphs — see panes::sessions::state_glyph — none of that changes).
        let sessions = vec![
            summary("a1111111", "active"),
            summary("b2222222", "provisioning"),
            summary("c3333333", "stopped"),
            summary("d4444444", "errored"),
        ];
        let rows = live_session_rows(sessions);
        assert_eq!(rows.len(), 4);
        assert_eq!(
            rows.iter().map(|r| r.state.as_str()).collect::<Vec<_>>(),
            vec!["active", "provisioning", "stopped", "errored"]
        );
    }

    #[test]
    fn live_session_rows_same_name_replacement_is_clean() {
        // (c) a decommissioned session and a newer, differently-id'd session
        // that reuses its `name` must never ghost (old row lingering) or
        // duplicate (both rows shown) — only the live one survives.
        let mut old = summary("a1111111", "decommissioned");
        old.name = "worker".to_string();
        let mut replacement = summary("b2222222", "active");
        replacement.name = "worker".to_string();

        let rows = live_session_rows(vec![old, replacement]);
        assert_eq!(
            rows.len(),
            1,
            "expected exactly one surviving row: {rows:?}"
        );
        assert_eq!(rows[0].id, "b2222222");
        assert_eq!(rows[0].name, "worker");
    }

    fn activity_response(raw_pane: &str) -> ManagedActivityResponse {
        ManagedActivityResponse {
            raw_pane: raw_pane.to_string(),
            runtime_active: true,
            pane_stale: false,
            state: "working".to_string(),
            summary: "running tests".to_string(),
            confidence: 0.9,
            cache_hit: false,
            input_tokens: 0,
            output_tokens: 0,
            latency_ms: 12,
            total_input_tokens: 0,
            total_output_tokens: 0,
            classification: None,
            pending_decision: Some("write to ci.yml?".to_string()),
            proposed_default: Some("yes".to_string()),
        }
    }

    #[test]
    fn activity_from_response_derives_fields_and_tail() {
        let resp = activity_response("line1\nline2\nline3\nline4\n");
        let info = activity_from_response("sess-1".to_string(), resp);
        assert_eq!(info.session_id, "sess-1");
        assert_eq!(info.state, "working");
        assert_eq!(info.summary, "running tests");
        assert_eq!(info.pending_decision.as_deref(), Some("write to ci.yml?"));
        assert_eq!(info.proposed_default.as_deref(), Some("yes"));
        assert_eq!(info.raw_pane_tail, vec!["line2", "line3", "line4"]);
        assert!(!info.stale);
    }

    #[test]
    fn activity_from_response_handles_short_pane() {
        let resp = activity_response("only one line");
        let info = activity_from_response("sess-1".to_string(), resp);
        assert_eq!(info.raw_pane_tail, vec!["only one line"]);
    }

    #[test]
    fn tail_lines_keeps_last_n_non_empty_lines() {
        let text = "a\nb\n\nc\nd\n";
        assert_eq!(tail_lines(text, 2), vec!["c", "d"]);
    }

    #[test]
    fn tail_lines_handles_fewer_lines_than_requested() {
        assert_eq!(tail_lines("only", 3), vec!["only"]);
        assert_eq!(tail_lines("", 3), Vec::<String>::new());
    }
}
