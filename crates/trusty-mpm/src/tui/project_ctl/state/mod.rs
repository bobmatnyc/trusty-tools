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
//! Sessions pane simply re-reads the already-polled map. [`ConfirmKind`] /
//! [`PendingConfirm`] are the shared confirmation-gate mechanism DOC-35 §5.2
//! requires before `k` (kill, when Active) or `d` (decommission, always) may
//! execute.
//! Test: `super::tests` covers focus cycling, selection clamp/reset, the
//! notice/repoll flags, and the confirm-gate request/clear/override rules.
//!
//! **Split (#2120, pre-emptive 500-SLOC cap avoidance)**: the two modal view
//! types ([`DeliverableView`] and the config form) plus their
//! `ProjectCtlState` mutator methods live in the sibling [`modals`] submodule,
//! re-exported here (`pub use modals::*`) so every existing
//! `super::state::DeliverableView`-style reference elsewhere in this crate
//! keeps working unchanged.

use std::collections::BTreeMap;

use crate::deliverable::Deliverable;
use crate::project::Project;
use crate::tui::coordinator::nav::ListNav;

mod modals;
pub use modals::{
    ConfigFormField, ConfigFormFocus, ConfigFormTags, ConfigFormView, DeliverableView,
};

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
/// Test: `poll::rows::tests::project_to_row_*`.
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
/// Test: `poll::rows::tests::session_to_row_*`,
/// `poll::rows::tests::live_session_rows_*` (#2476 — decommissioned rows are
/// filtered before this type is ever constructed for a tombstoned session).
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
    /// The Deliverable this session is bound to, if any (DOC-35 §10.6,
    /// #2379). `Some` drives the Sessions-pane deliverable glyph (#2383);
    /// whether the id resolves against [`ProjectCtlState::deliverables`] (a
    /// live link) or not (a dangling ref — the Deliverable was deleted or
    /// belongs to a project mismatch) is resolved at render time, not stored
    /// here, so a poll-driven change in the deliverable set is picked up on
    /// the very next frame without re-deriving this row.
    pub deliverable_id: Option<String>,
}

/// Live per-session activity fetched from `GET .../{id}/activity` (DOC-35
/// §5.4, #2119).
///
/// Why: the Activity pane skeleton (#2118) rendered only the already-polled
/// STATIC [`SessionRow`] fields; this issue wires the pane to the live
/// `/activity` endpoint instead. `session_id` is carried alongside the fetched
/// fields (rather than trusting the caller to only ever read this when it is
/// current) so [`ProjectCtlState::activity_for_selected`] can detect a fetch
/// that has not yet caught up with a just-changed selection and fall back
/// rather than showing one session's live data under another's header.
/// `stale` marks a fetch that failed while the daemon was unreachable — the
/// last-known data is KEPT rather than discarded (the issue's "graceful
/// daemon-down behavior: stale-data indicator... recover when the daemon
/// returns" requirement), and clears automatically the next time a fetch for
/// the same session succeeds.
/// What: the four DOC-35 §5.4 fields (`state`, `summary`, `pending_decision`,
/// `proposed_default`) plus a short `raw_pane_tail` (the mockup's "last 3
/// lines of raw_pane") and the `stale` flag.
/// Test: `poll::rows::tests::activity_from_response_*`,
/// `panes::activity::tests::activity_lines_*`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ActivityInfo {
    /// The session this activity snapshot belongs to.
    pub session_id: String,
    /// Activity state word (`working`/`idle`/`blocked_on_permission`/
    /// `errored`/`done`/`unknown`).
    pub state: String,
    /// Human-readable summary of what the session is doing.
    pub summary: String,
    /// A pending decision question, if surfaced.
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
    /// The last few lines of the session's raw tmux pane, oldest first.
    pub raw_pane_tail: Vec<String>,
    /// True when this snapshot is carried over from a prior successful fetch
    /// because the daemon was unreachable on the most recent poll.
    pub stale: bool,
}

/// Which destructive verb a [`PendingConfirm`] is gating.
///
/// Why: DOC-35 §5.2 requires a confirmation gate before `k` (kill) executes
/// on an Active session, and unconditionally before `d` (decommission)
/// executes at all — a single keypress must never fire either. Sharing ONE
/// confirmation mechanism for both (rather than two parallel modal types)
/// keeps the gate/render/dispatch plumbing in one place.
/// What: `Kill` and `Decommission` map 1:1 to [`super::events::PendingAction::Kill`]
/// / [`super::events::PendingAction::Decommission`] once confirmed.
/// Test: `super::events::tests` covers both confirm flows.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmKind {
    /// Confirming `k` (kill / runtime-stop) on an Active session.
    Kill,
    /// Confirming `d` (decommission) — always gated, any state (terminal action).
    Decommission,
}

/// A destructive action awaiting operator confirmation (DOC-35 §5.2).
///
/// Why: holds everything [`super::panes::actions_bar`] needs to render the
/// confirm prompt and everything [`super::events::handle_key`] needs to
/// resolve it (confirm → the real [`super::events::PendingAction`]; cancel →
/// discard) without re-deriving either from the current selection, which may
/// have moved on by the time the operator answers.
/// What: the verb ([`ConfirmKind`]), the target session's id (for dispatch),
/// and a human-readable label (name or short id, for the prompt text).
/// Test: `super::events::tests`, `super::panes::actions_bar::tests`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingConfirm {
    /// Which verb is being confirmed.
    pub kind: ConfirmKind,
    /// The target session's full id — threaded into the eventual
    /// [`super::events::PendingAction`] on confirmation.
    pub session_id: String,
    /// A human-readable label for the target session, shown in the prompt.
    pub session_label: String,
}

/// The resolution of a session's `deliverable_id` against the currently
/// known Deliverable set (DOC-35 §10.6, #2383).
///
/// Why: a two-state (resolved/dangling) model conflates "confirmed the
/// Deliverable no longer exists" with "we simply don't know yet" — e.g. the
/// very first poll after launch, or a transient `list_deliverables` failure
/// on an otherwise-healthy daemon. Rendering the latter as a red "dangling"
/// glyph is a false signal (adversarial review finding on #2383's initial
/// PR): an operator seeing it would reasonably conclude the Deliverable was
/// deleted, when in fact the daemon just missed one HTTP round trip. A third,
/// explicit `Unknown` state lets the render layer show NO glyph (rather than
/// guess) until a fetch actually succeeds or actually confirms absence.
/// What: `Unknown` when [`ProjectCtlState::deliverables`] is `None` (nothing
/// fetched yet, or the poll deliberately kept stale-but-unclassified data —
/// see that field's own doc). `Resolved`/`Dangling` when `Some(list)` either
/// does or does not contain a matching id.
/// Test: `super::tests::deliverable_link_state_*`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DeliverableLinkState {
    /// No successfully fetched Deliverable set exists yet for the current
    /// project — render no glyph, never a dangling one.
    Unknown,
    /// The id resolves against the last successfully fetched set.
    Resolved,
    /// The id does NOT resolve against the last successfully fetched set —
    /// a confirmed dangling reference.
    Dangling,
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
    /// A destructive action (kill-on-Active / decommission) awaiting
    /// operator confirmation (DOC-35 §5.2). While `Some`, [`handle_key`] in
    /// `events.rs` routes EVERY key through the confirm/cancel gate instead
    /// of normal dispatch — mirroring the spec's "`q`/`Ctrl-C` — not in a
    /// modal" / "`Esc` — any modal/form — cancel" contract.
    ///
    /// [`handle_key`]: super::events::handle_key
    pub pending_confirm: Option<PendingConfirm>,
    /// Live activity for the currently selected session (DOC-35 §5.4, #2119),
    /// refreshed on the same poll cadence as `projects`/`sessions_by_project`.
    /// `None` before the first successful fetch, when no session is selected,
    /// or once the selection moves off the session this snapshot belongs to.
    /// Read it through [`Self::activity_for_selected`], not directly, so a
    /// mismatched (stale-selection) snapshot is never rendered under the
    /// wrong session's header.
    pub activity: Option<ActivityInfo>,
    /// The currently selected project's Deliverables, refreshed on the same
    /// poll cadence as `projects`/`sessions_by_project` (DOC-35 §10.6,
    /// #2383). `None` means UNKNOWN — no project selected, the selection just
    /// changed (not yet fetched for the new project), or every fetch so far
    /// has failed; it does NOT mean "confirmed no Deliverables." `Some(list)`
    /// — even a `Some(vec![])` — means a fetch for the current project has
    /// succeeded at least once; a transient fetch failure after that point
    /// leaves the last-known-good `Some(list)` in place rather than
    /// overwriting it (mirrors [`ActivityInfo`]'s stale-keep pattern via
    /// [`refresh_activity`]: `poll::refresh_activity`). This distinction
    /// matters because [`Self::deliverable_link_state`] must never render a
    /// bound session's link as "confirmed dangling" just because ONE poll's
    /// `list_deliverables` call happened to time out — see the review finding
    /// this fixed. This is the ONE additional per-tick call the Sessions-pane
    /// deliverable glyph adds to the poll loop — see
    /// `poll::project_ctl_poll_daemon`'s doc for the exact budget accounting.
    /// Also backs [`DeliverableView::deliverables`] when the view is open for
    /// this same project, so opening it never triggers a second fetch.
    pub deliverables: Option<Vec<Deliverable>>,
    /// A read-only Deliverable/Milestone view awaiting display (DOC-35 §10.8
    /// `show`, #2383). While `Some`, [`handle_key`] in `events.rs` routes
    /// EVERY key through the view's own scroll/close handling instead of
    /// normal dispatch — the identical "modal captures all input, `Esc`
    /// closes" discipline [`PendingConfirm`] already establishes.
    ///
    /// [`handle_key`]: super::events::handle_key
    pub deliverable_view: Option<DeliverableView>,
    /// The full registry-B [`Project`] record for every currently known
    /// project, keyed by name (DOC-35 §6, #2120) — refreshed on the same poll
    /// cadence as `projects`/`sessions_by_project`, from the SAME
    /// `registry_list_projects` response `poll.rs` already fetches to build
    /// [`ProjectRow`]s (no extra HTTP call). [`ProjectRow`] only carries the
    /// aggregate-glyph fields the Projects pane renders; the config form
    /// needs the full record (`default_branch`/`description`/`tags`/
    /// `stack_hint`/`gh_user`) to seed its baseline values, so it is kept
    /// here rather than widening `ProjectRow` (which many call sites and
    /// tests construct with only its four existing fields).
    pub projects_full: BTreeMap<String, Project>,
    /// A deterministic config-edit form awaiting operator input (DOC-35 §6,
    /// #2120), opened by `c` in the Projects pane. While `Some`,
    /// [`handle_key`] in `events.rs` routes EVERY key through the form's own
    /// focus-cycle/edit/submit/close handling instead of normal dispatch —
    /// the identical "modal captures all input, `Esc` closes" discipline
    /// [`PendingConfirm`]/[`DeliverableView`] already establish. Mutually
    /// exclusive with both in practice: `c` is only ever evaluated by
    /// `handle_key` when neither `pending_confirm` nor `deliverable_view` is
    /// `Some` (both take priority and return early), so this form can never
    /// open while either other modal is already showing.
    ///
    /// [`handle_key`]: super::events::handle_key
    pub config_form: Option<ConfigFormView>,
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

    /// The live [`ActivityInfo`] for the currently selected session, only
    /// when it matches that session's id.
    ///
    /// Why: [`Self::activity`] is refreshed once per poll for whichever
    /// session was selected AT THE TIME of that poll; if the operator moves
    /// the Sessions-pane selection between polls, a stale fetch for the
    /// PREVIOUS session must never render under the new session's header.
    /// Comparing ids on every read (rather than clearing `activity` eagerly
    /// on every selection change, which would need re-plumbing into every
    /// selection-mutating call site) keeps the id-freshness rule in exactly
    /// one place.
    /// What: `None` when no session is selected, no fetch has landed yet, or
    /// the last fetch's `session_id` no longer matches the current selection;
    /// otherwise `Some(&ActivityInfo)`.
    /// Test: `panes::activity::tests::activity_lines_falls_back_while_activity_is_stale_selection`.
    pub fn activity_for_selected(&self) -> Option<&ActivityInfo> {
        let selected_id = &self.selected_session()?.id;
        self.activity
            .as_ref()
            .filter(|a| &a.session_id == selected_id)
    }

    /// Re-sync the Sessions-pane navigation to a freshly selected project.
    ///
    /// Why: switching WHICH project is selected in the Projects pane means the
    /// Sessions pane now shows an unrelated list; carrying over the old scroll
    /// position would highlight an arbitrary, unrelated row. Resetting to the
    /// top on an explicit project switch (but NOT on every poll — see
    /// [`super::poll::project_ctl_poll_daemon`], which preserves the Sessions
    /// selection across a refresh) matches the operator's expectation.
    /// [`Self::deliverables`] is ALSO reset to `Unknown` (`None`) here — it
    /// belonged to the PREVIOUS project, and carrying it over would let a new
    /// project's session rows resolve against a stranger project's Deliverable
    /// set (harmless by UUID uniqueness, but semantically wrong and worth
    /// avoiding outright rather than relying on that coincidence).
    /// What: replaces `sessions_nav` with a fresh default, then syncs its
    /// length to `current_sessions().len()`; clears `deliverables` to `None`.
    /// Test: `project_switch_resets_session_selection`,
    /// `project_switch_resets_deliverables_to_unknown`.
    pub fn on_project_selection_changed(&mut self) {
        self.sessions_nav = ListNav::default();
        self.sessions_nav.sync_len(self.current_sessions().len());
        self.deliverables = None;
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

    /// Open the confirmation gate for a destructive action (DOC-35 §5.2).
    ///
    /// Why: the single seam [`super::events`]'s `k`/`d` handlers call instead
    /// of returning the real [`super::events::PendingAction`] directly — no
    /// destructive verb may execute on the keypress that requested it.
    /// What: sets [`Self::pending_confirm`], overriding any prior one (a new
    /// request always wins — there is only ever one target at a time since
    /// the modal captures all input while open).
    /// Test: `super::events::tests`.
    pub fn request_confirm(
        &mut self,
        kind: ConfirmKind,
        session_id: impl Into<String>,
        session_label: impl Into<String>,
    ) {
        self.pending_confirm = Some(PendingConfirm {
            kind,
            session_id: session_id.into(),
            session_label: session_label.into(),
        });
    }

    /// Close the confirmation gate without acting (`n`/`N`/Esc, or after `y`
    /// resolves it).
    pub fn clear_confirm(&mut self) {
        self.pending_confirm = None;
    }

    /// Resolve one session's `deliverable_id` against the currently fetched
    /// [`Self::deliverables`] (DOC-35 §10.6, #2383) — a THREE-state result,
    /// not a boolean, so a transient fetch failure is never rendered as a
    /// confirmed-dangling reference (see [`DeliverableLinkState`]'s own doc
    /// for why this distinction exists).
    ///
    /// Why: the Sessions-pane glyph (`panes::sessions`) needs this per row; a
    /// linear scan over the (small, single-project-scoped) list is simpler
    /// than maintaining a parallel `HashSet` and is only ever run once per
    /// render, not per poll.
    /// What: [`Self::deliverables`] is `None` → [`DeliverableLinkState::Unknown`].
    /// `Some(list)` and `id` matches an entry's `id.to_string()` →
    /// [`DeliverableLinkState::Resolved`]. `Some(list)` and no entry matches →
    /// [`DeliverableLinkState::Dangling`].
    /// Test: `deliverable_link_state_unknown_when_not_yet_fetched`,
    /// `deliverable_link_state_resolved_and_dangling`.
    pub fn deliverable_link_state(&self, id: &str) -> DeliverableLinkState {
        match &self.deliverables {
            None => DeliverableLinkState::Unknown,
            Some(list) => {
                if list.iter().any(|d| d.id.to_string() == id) {
                    DeliverableLinkState::Resolved
                } else {
                    DeliverableLinkState::Dangling
                }
            }
        }
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
            deliverable_id: None,
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
    fn project_switch_resets_deliverables_to_unknown() {
        let mut state = ProjectCtlState {
            deliverables: Some(vec![]),
            ..Default::default()
        };
        assert!(state.deliverables.is_some());
        state.on_project_selection_changed();
        assert!(
            state.deliverables.is_none(),
            "switching projects must reset deliverables to Unknown, not carry over the \
             previous project's (possibly stale) list"
        );
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

    #[test]
    fn confirm_request_set_and_clear() {
        let mut state = ProjectCtlState::default();
        assert!(state.pending_confirm.is_none());
        state.request_confirm(ConfirmKind::Decommission, "sess-1", "my-session");
        let confirm = state.pending_confirm.clone().expect("confirm requested");
        assert_eq!(confirm.kind, ConfirmKind::Decommission);
        assert_eq!(confirm.session_id, "sess-1");
        assert_eq!(confirm.session_label, "my-session");
        state.clear_confirm();
        assert!(state.pending_confirm.is_none());
    }

    fn activity(session_id: &str) -> ActivityInfo {
        ActivityInfo {
            session_id: session_id.to_string(),
            state: "working".to_string(),
            summary: "running tests".to_string(),
            pending_decision: None,
            proposed_default: None,
            raw_pane_tail: vec!["$ cargo test".to_string()],
            stale: false,
        }
    }

    #[test]
    fn activity_for_selected_matches_by_session_id() {
        let mut state = ProjectCtlState {
            projects: vec![row("a")],
            ..Default::default()
        };
        state
            .sessions_by_project
            .insert("a".to_string(), vec![session("sess-1")]);
        state.projects_nav.sync_len(state.projects.len());
        state.sessions_nav.sync_len(state.current_sessions().len());
        state.activity = Some(activity("sess-1"));
        assert_eq!(
            state.activity_for_selected().unwrap().summary,
            "running tests"
        );
    }

    #[test]
    fn activity_for_selected_is_none_when_it_targets_a_different_session() {
        let mut state = ProjectCtlState {
            projects: vec![row("a")],
            ..Default::default()
        };
        state
            .sessions_by_project
            .insert("a".to_string(), vec![session("sess-1")]);
        state.projects_nav.sync_len(state.projects.len());
        state.sessions_nav.sync_len(state.current_sessions().len());
        // Simulates a fetch that landed for a session the selection has since
        // moved off of.
        state.activity = Some(activity("some-other-session"));
        assert!(state.activity_for_selected().is_none());
    }

    #[test]
    fn activity_for_selected_is_none_with_no_selection() {
        let state = ProjectCtlState {
            activity: Some(activity("sess-1")),
            ..Default::default()
        };
        assert!(state.activity_for_selected().is_none());
    }

    #[test]
    fn confirm_request_overrides_a_prior_one() {
        let mut state = ProjectCtlState::default();
        state.request_confirm(ConfirmKind::Kill, "sess-1", "one");
        state.request_confirm(ConfirmKind::Decommission, "sess-2", "two");
        let confirm = state.pending_confirm.expect("confirm requested");
        assert_eq!(confirm.kind, ConfirmKind::Decommission);
        assert_eq!(confirm.session_id, "sess-2");
    }

    // ---- DOC-35 §10.6 DeliverableLinkState (#2383 review fix) -------------

    fn sample_deliverable(id: crate::deliverable::DeliverableId) -> Deliverable {
        Deliverable {
            id,
            project_name: "widget".to_string(),
            name: "OAuth2 flow".to_string(),
            description: String::new(),
            kind: crate::deliverable::DeliverableKind::Feature,
            ticket_ref: None,
            spec_ref: None,
            status: crate::deliverable::DeliverableStatus::InProgress,
            estimated_effort: crate::deliverable::EstimationTier::M,
            created_at: chrono::Utc::now(),
            target_date: None,
        }
    }

    #[test]
    fn deliverable_link_state_unknown_when_not_yet_fetched() {
        let state = ProjectCtlState::default();
        assert!(state.deliverables.is_none());
        assert_eq!(
            state.deliverable_link_state("any-id"),
            DeliverableLinkState::Unknown,
            "before any successful fetch (or after one keeps failing), the link state \
             must be Unknown, never Dangling"
        );
    }

    #[test]
    fn deliverable_link_state_resolved_and_dangling() {
        let id = crate::deliverable::DeliverableId::new();
        let state = ProjectCtlState {
            deliverables: Some(vec![sample_deliverable(id)]),
            ..Default::default()
        };
        assert_eq!(
            state.deliverable_link_state(&id.to_string()),
            DeliverableLinkState::Resolved
        );
        assert_eq!(
            state.deliverable_link_state("00000000-0000-0000-0000-000000000000"),
            DeliverableLinkState::Dangling,
            "a Some(list) that does not contain the id is a CONFIRMED dangling ref"
        );
    }
}
