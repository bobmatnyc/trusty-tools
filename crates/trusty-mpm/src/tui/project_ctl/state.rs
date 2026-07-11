//! Application state for the `tm projects` 4-pane TUI skeleton (#2118).
//!
//! Why: the render loop and the key dispatcher both read and mutate one piece
//! of state — the registered-project list, the sessions grouped per project,
//! which pane has focus, and the current row selections. Holding it in a
//! single struct with pure, unit-testable mutators keeps the terminal glue
//! thin and the selection logic verifiable without a terminal, mirroring
//! `tui::coordinator::state::CoordinatorState`.
//! What: [`ProjectCtlState`] plus its row types ([`ProjectRow`],
//! [`SessionRow`]) and the [`Pane`] focus enum. Sessions are keyed by owning
//! project name in [`ProjectCtlState::sessions_by_project`] so switching the
//! Projects-pane selection never requires a new daemon round trip — the
//! Sessions pane simply re-reads the already-polled map.
//! Test: `super::tests` covers focus cycling, selection clamp/reset, and the
//! notice/repoll flags.

use std::collections::BTreeMap;

use crate::tui::coordinator::nav::ListNav;

/// Which of the three navigable panes currently has keyboard focus.
///
/// Why: Tab/Shift+Tab cycle focus Projects → Sessions → Activity (DOC-35 §5);
/// a typed enum keeps the cycle exhaustive and the render path's highlight
/// branch unambiguous. The Activity pane is a skeleton in this issue (#2118)
/// — it renders the focused session's static fields only — but it is still a
/// stop in the focus cycle per the spec's keybinding table.
/// What: three variants in cycle order; [`Pane::next`] / [`Pane::prev`] wrap.
/// Test: `pane_cycle_wraps_forward_and_backward`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Pane {
    /// The left-column project list (25% width).
    #[default]
    Projects,
    /// The right-column session list for the selected project (75% width).
    Sessions,
    /// The bottom activity strip for the focused session.
    Activity,
}

impl Pane {
    /// Advance to the next pane in the cycle, wrapping after Activity.
    pub fn next(self) -> Self {
        match self {
            Pane::Projects => Pane::Sessions,
            Pane::Sessions => Pane::Activity,
            Pane::Activity => Pane::Projects,
        }
    }

    /// Step back to the previous pane in the cycle, wrapping before Projects.
    pub fn prev(self) -> Self {
        match self {
            Pane::Projects => Pane::Activity,
            Pane::Sessions => Pane::Projects,
            Pane::Activity => Pane::Sessions,
        }
    }
}

/// One registered project as rendered by the Projects pane.
///
/// Why: the pane shows an aggregate-state glyph plus a live session count
/// (DOC-35 §5) rather than the raw registry record; a small projection keeps
/// the render layer free of the wire DTOs.
/// What: the registry name/repo URL plus `live_count` (sessions currently
/// `active` or `provisioning`) and `total_count` (every session bound to the
/// project, any state).
/// Test: `poll::tests::project_to_row_*`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProjectRow {
    /// Registry key / short project name.
    pub name: String,
    /// Full repository URL.
    pub repo_url: String,
    /// Sessions currently `active` or `provisioning` — the "aggregate-state
    /// glyph, live session count" the spec calls for.
    pub live_count: usize,
    /// Every session bound to this project, any lifecycle state.
    pub total_count: usize,
}

/// One managed session as rendered by the Sessions pane.
///
/// Why: the Sessions pane needs a numbered, compact row per session; a small
/// projection off [`crate::client::ManagedSessionSummary`] keeps the render
/// layer decoupled from the wire DTO. `pending_decision` / `proposed_default`
/// are carried through UNCHANGED from the session record (they are static
/// fields on the record itself, not a live `/activity` poll) so the Activity
/// pane skeleton can render them without calling the `/activity` endpoint —
/// that live wiring is deferred to #2119.
/// What: id/short-id/name/branch/task plus the lifecycle `state` word and the
/// two static activity fields.
/// Test: `poll::tests::session_to_row_*`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct SessionRow {
    /// Full session id (UUID string).
    pub id: String,
    /// Short (8-hex) session id shown in the list.
    pub short_id: String,
    /// tmux session name.
    pub name: String,
    /// Git branch or ref checked out, if known.
    pub branch: Option<String>,
    /// Task description, if known.
    pub task: Option<String>,
    /// Lifecycle state word (`active`, `provisioning`, `stopped`, `errored`,
    /// `decommissioned`).
    pub state: String,
    /// A pending decision question, if surfaced on the record.
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
}

/// Everything the `tm projects` TUI renders and mutates this frame.
///
/// Why: a single owned struct (mirroring `CoordinatorState`) keeps the
/// data/render split clean — the event loop and the poller mutate it, the
/// layout reads it.
/// What: the project list + its navigation, the per-project session map + its
/// navigation, the focused pane, daemon reachability, a transient notice
/// (toast) line, and the repoll/exit flags.
/// Test: `super::tests` covers focus cycling, selection, and the flags.
#[derive(Debug, Clone, Default)]
pub struct ProjectCtlState {
    /// Registered projects, in registry order.
    pub projects: Vec<ProjectRow>,
    /// Projects-pane selection + scroll offset.
    pub projects_nav: ListNav,
    /// Every project's sessions, keyed by project name — populated by one
    /// `GET /api/v1/sessions/managed/fleet` poll per tick rather than a
    /// per-project round trip.
    pub sessions_by_project: BTreeMap<String, Vec<SessionRow>>,
    /// Sessions-pane selection + scroll offset (scoped to the currently
    /// selected project).
    pub sessions_nav: ListNav,
    /// Which pane currently has keyboard focus.
    pub focus: Pane,
    /// Whether the daemon answered its last health probe.
    pub daemon_reachable: bool,
    /// A transient notice (toast) shown in the action bar, e.g. the result of
    /// a launch/kill/resume/decommission/attach/config action.
    pub notice: Option<String>,
    /// Set when a mutating action just succeeded and the operator should see
    /// the fleet refreshed without waiting for the next timer tick.
    ///
    /// `pub(crate)`, not private: sibling modules within this crate build
    /// [`ProjectCtlState`] via struct-update syntax (`..Default::default()`)
    /// in their own test seeds, which requires every field to be visible at
    /// the construction site even when not explicitly listed. External
    /// crates still cannot name or set it directly — only
    /// [`Self::request_repoll`] / [`Self::take_repoll`] mutate it outside
    /// this crate.
    pub(crate) needs_repoll: bool,
    /// Set when the operator asks to quit; the event loop exits on the next tick.
    pub should_exit: bool,
}

impl ProjectCtlState {
    /// The currently selected project row, if any.
    ///
    /// Why: the Sessions/Activity panes and the `l`/`c` actions all need to
    /// know which project is selected.
    /// What: `projects[projects_nav.selected()]`, or `None` on an empty list.
    /// Test: `selected_project_reads_nav_index`.
    pub fn selected_project(&self) -> Option<&ProjectRow> {
        self.projects.get(self.projects_nav.selected())
    }

    /// The currently selected project's name, if any.
    pub fn selected_project_name(&self) -> Option<&str> {
        self.selected_project().map(|p| p.name.as_str())
    }

    /// The session list for the currently selected project (may be empty).
    ///
    /// Why: the Sessions pane and the `k`/`r`/`d`/`a` actions read this rather
    /// than re-deriving it from `sessions_by_project` at every call site.
    /// What: `sessions_by_project[selected project name]`, or `&[]` when no
    /// project is selected or the project has no sessions.
    /// Test: `current_sessions_follows_selected_project`.
    pub fn current_sessions(&self) -> &[SessionRow] {
        self.selected_project_name()
            .and_then(|name| self.sessions_by_project.get(name))
            .map(Vec::as_slice)
            .unwrap_or(&[])
    }

    /// The currently selected session row, if any.
    pub fn selected_session(&self) -> Option<&SessionRow> {
        self.current_sessions().get(self.sessions_nav.selected())
    }

    /// Re-sync the Sessions-pane navigation to a freshly selected project.
    ///
    /// Why: switching WHICH project is selected in the Projects pane means the
    /// Sessions pane now shows an unrelated list; carrying over the old scroll
    /// position would highlight an arbitrary, unrelated row. Resetting to the
    /// top on an explicit project switch (but NOT on every poll — see
    /// [`super::poll::project_ctl_poll_daemon`], which preserves the Sessions
    /// selection across a refresh) matches the operator's expectation.
    /// What: replaces `sessions_nav` with a fresh default, then syncs its
    /// length to `current_sessions().len()`.
    /// Test: `project_switch_resets_session_selection`.
    pub fn on_project_selection_changed(&mut self) {
        self.sessions_nav = ListNav::default();
        self.sessions_nav.sync_len(self.current_sessions().len());
    }

    /// Cycle focus forward (Projects → Sessions → Activity → …).
    pub fn cycle_focus_next(&mut self) {
        self.focus = self.focus.next();
    }

    /// Cycle focus backward (Activity → Sessions → Projects → …).
    pub fn cycle_focus_prev(&mut self) {
        self.focus = self.focus.prev();
    }

    /// Drill into a project's Sessions pane (Enter on the Projects pane).
    pub fn drill_into_sessions(&mut self) {
        self.focus = Pane::Sessions;
    }

    /// Set the transient action-bar notice.
    pub fn set_notice(&mut self, msg: impl Into<String>) {
        self.notice = Some(msg.into());
    }

    /// Clear the transient action-bar notice (Esc).
    pub fn clear_notice(&mut self) {
        self.notice = None;
    }

    /// Request an immediate daemon re-poll on the next run-loop tick.
    ///
    /// Why: after a mutating action (kill/resume/decommission) the operator
    /// expects the fleet to refresh now, not up to a whole `--interval-ms`
    /// later. Mirrors `CoordinatorState::request_repoll`.
    pub fn request_repoll(&mut self) {
        self.needs_repoll = true;
    }

    /// Read-and-clear the immediate-re-poll request.
    pub fn take_repoll(&mut self) -> bool {
        std::mem::take(&mut self.needs_repoll)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(name: &str) -> ProjectRow {
        ProjectRow {
            name: name.to_string(),
            repo_url: format!("https://github.com/acme/{name}"),
            live_count: 0,
            total_count: 0,
        }
    }

    fn session(id: &str) -> SessionRow {
        SessionRow {
            id: id.to_string(),
            short_id: id.chars().take(8).collect(),
            name: format!("session-{id}"),
            branch: Some("main".to_string()),
            task: Some("do the thing".to_string()),
            state: "active".to_string(),
            pending_decision: None,
            proposed_default: None,
        }
    }

    #[test]
    fn pane_cycle_wraps_forward_and_backward() {
        assert_eq!(Pane::Projects.next(), Pane::Sessions);
        assert_eq!(Pane::Sessions.next(), Pane::Activity);
        assert_eq!(Pane::Activity.next(), Pane::Projects);
        assert_eq!(Pane::Projects.prev(), Pane::Activity);
        assert_eq!(Pane::Activity.prev(), Pane::Sessions);
        assert_eq!(Pane::Sessions.prev(), Pane::Projects);
    }

    #[test]
    fn selected_project_reads_nav_index() {
        let mut state = ProjectCtlState {
            projects: vec![row("a"), row("b")],
            ..Default::default()
        };
        state.projects_nav.sync_len(state.projects.len());
        assert_eq!(state.selected_project().unwrap().name, "a");
        state.projects_nav.down();
        assert_eq!(state.selected_project().unwrap().name, "b");
    }

    #[test]
    fn current_sessions_follows_selected_project() {
        let mut state = ProjectCtlState {
            projects: vec![row("a"), row("b")],
            ..Default::default()
        };
        state
            .sessions_by_project
            .insert("a".to_string(), vec![session("aaaaaaaa1111")]);
        state.sessions_by_project.insert("b".to_string(), vec![]);
        state.projects_nav.sync_len(state.projects.len());
        assert_eq!(state.current_sessions().len(), 1);
        state.projects_nav.down();
        assert!(state.current_sessions().is_empty());
    }

    #[test]
    fn project_switch_resets_session_selection() {
        let mut state = ProjectCtlState {
            projects: vec![row("a"), row("b")],
            ..Default::default()
        };
        state.sessions_by_project.insert(
            "a".to_string(),
            vec![session("id1111111111"), session("id2222222222")],
        );
        state.projects_nav.sync_len(state.projects.len());
        state.sessions_nav.sync_len(state.current_sessions().len());
        state.sessions_nav.down();
        assert_eq!(state.sessions_nav.selected(), 1);
        state.on_project_selection_changed();
        assert_eq!(state.sessions_nav.selected(), 0);
    }

    #[test]
    fn notice_set_and_clear() {
        let mut state = ProjectCtlState::default();
        assert!(state.notice.is_none());
        state.set_notice("hello");
        assert_eq!(state.notice.as_deref(), Some("hello"));
        state.clear_notice();
        assert!(state.notice.is_none());
    }

    #[test]
    fn repoll_request_is_take_once() {
        let mut state = ProjectCtlState::default();
        assert!(!state.take_repoll());
        state.request_repoll();
        assert!(state.take_repoll());
        assert!(!state.take_repoll());
    }
}
