//! [`SessionSummary`] — the per-session wire shape shared by the list/get
//! managed-session endpoints.
//!
//! Why: split out of `mod.rs` (which sits at its 500-SLOC production cap) so
//! adding the `stale_assets` field (issue #2444) has somewhere to land without
//! needing a broader `mod.rs` refactor; mirrors the `doctor_output_style.rs` /
//! `doctor_fs_checks.rs` split precedent used elsewhere for the same reason.
//! What: the flat, string-typed summary every managed-session list/get/mutate
//! handler returns.
//! Test: list/get handler tests in `tests/session_manager_mvp.rs`; the
//! `stale_assets` field specifically is covered by
//! `checked_summaries_flags_stale_assets_only_for_relevant_states` in
//! `super::tests`.

use serde::Serialize;

/// Per-session summary for the list endpoint.
///
/// Why: the list endpoint returns less detail than the single-session endpoint;
/// keeping a summary type avoids serializing the full record in list responses.
/// What: id, name, state, workspace_path, repo_url, branch, timestamps,
/// task, cwd, pending_decision, proposed_default, source_id.
/// Test: list handler test.
#[derive(Debug, Serialize)]
pub struct SessionSummary {
    /// Managed session id.
    pub id: String,
    /// tmux session name.
    pub name: String,
    /// Lifecycle state (#3302: reconciled against LIVE tmux for DISPLAY by
    /// [`super::summary::reconcile_live_state`] when this summary came from
    /// the list/get endpoints — an `Active`/`Stopped` record whose tmux
    /// session is absent/present is shown as `stopped`/`active` regardless of
    /// what is actually persisted). Use [`Self::persisted_state`], not this
    /// field, for any RESUME/RESTART decision — see that field's doc (#3531).
    pub state: String,
    /// The RAW, un-reconciled lifecycle state exactly as persisted in the
    /// store — set once at [`super::summary::record_to_summary`] and never
    /// touched by the later display-only tmux reconciliation that may
    /// overwrite [`Self::state`] (#3531).
    ///
    /// Why: the daemon's own `/resume` endpoint
    /// ([`crate::session_manager::SessionManager::resume`]) validates a
    /// restart against the PERSISTED state only — it has no idea a caller's
    /// copy of `state` was display-reconciled. Before this field existed, a
    /// zombie session (record still `Active`/`Provisioning` but its tmux
    /// pane gone) had its displayed `state` collapsed to `stopped` by
    /// [`super::summary::reconcile_live_state`] — indistinguishable, from the
    /// CLI's point of view, from a session that is GENUINELY `Stopped`. The
    /// CLI's own zombie auto-reconcile (#2001,
    /// `bin/tm/commands/guided_resume::plan_resume`) then misclassified the
    /// zombie as a plain `Restart` instead of `ReconcileThenRestart`, and the
    /// daemon's `/resume` rejected it with a 409 ("cannot resume a session in
    /// state 'active'") — the exact dead-end #3531 reported. Restart/resume
    /// decisions must key off THIS field so they always agree with what the
    /// daemon's own state-gate will actually check.
    /// What: identical to `state` for every summary NOT produced by
    /// `list_managed_sessions`/`get_managed_session` (nothing reconciles it);
    /// for those two, it is the value `state` held BEFORE reconciliation.
    /// Test: `reconcile_live_state_leaves_persisted_state_untouched` in
    /// `super::tests`.
    pub persisted_state: String,
    /// Provisioned workspace path.
    pub workspace_path: Option<String>,
    /// Repository URL.
    pub repo_url: Option<String>,
    /// Git branch or ref.
    pub branch: Option<String>,
    /// Creation timestamp (RFC 3339).
    pub created_at: String,
    /// Last activity timestamp (RFC 3339), if any.
    pub last_activity_at: Option<String>,
    /// A pending decision question, if surfaced.
    pub pending_decision: Option<String>,
    /// Proposed default answer to the pending decision.
    pub proposed_default: Option<String>,
    /// Source project identity (`owner/repo`) for in-project sessions (#1707).
    ///
    /// Why: the in-project spawn path records the GitHub identity so callers can
    /// filter sessions by project and reconnect to existing ones.
    /// `None` for sessions not created via the in-project path.
    pub source_id: Option<String>,
    /// Task description for the session (additive; absent for legacy records).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub task: Option<String>,
    /// Working directory for the session (additive; absent for legacy records).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cwd: Option<String>,
    /// Captured Claude Code conversation id, if any (additive; #2023 C).
    ///
    /// Why: the bare-`tm` in-pane relaunch path (#2023 component C) needs the
    /// SAME `claude_session_id` the tmux-pane resume path uses for its
    /// `--resume <id>` existence-check-and-fallback logic (#2013) — exposing it
    /// on the wire lets the CLI build the identical command without a second,
    /// divergent lookup. `None` for sessions where no `SessionStart` capture has
    /// landed yet, or for legacy records predating the field.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub claude_session_id: Option<String>,
    /// The Deliverable this session is working on, if bound (DOC-35 §10.6,
    /// #2379; additive — absent for legacy records and sessions with no link).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub deliverable_id: Option<String>,
    /// The tmux `pane_id` of this session's original pane, if captured
    /// (additive; #2453 review finding 1, round 2 — absent for legacy
    /// records, or when the driver could not resolve one).
    ///
    /// Why: the bare-`tm` in-pane relaunch's nested-session guard needs this
    /// to confirm the operator's CURRENT pane is genuinely the one bound to
    /// this record before driving a destructive `exec` — see
    /// `SessionRecord::pane_id`'s doc for the full rationale (a session-name
    /// or process-env-var match alone is provably insufficient).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    /// Delivery status of the turnkey `--task` pane injection, if injection
    /// was ever attempted for this session (additive; #2364).
    ///
    /// Why: `SessionManager::inject_task_when_ready` was fire-and-forget
    /// before this field existed — callers had no way to poll whether an
    /// injected task was actually delivered. Exposing it lets `tm session
    /// info`/`tm sessions ls` surface `pending`/`success`/`failed_timeout`/
    /// `failed_session_died` instead of requiring a blind wait on `tm session
    /// activity`. `None` when injection was never attempted for this session
    /// (opted out, empty task, non-Claude-Code runtime, or a spawn that never
    /// reached `Active`) — see `session_manager::InjectionStatus::NotApplicable`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub injection_status: Option<String>,
    /// True when this session is a dead pick: it is `stopped`/`errored` AND no
    /// workdir candidate (`last_cwd`, `workspace_path`, `cwd`) exists on disk
    /// any more, so a resume is guaranteed to fail (#2595).
    ///
    /// Why: #2577/#2594 fixed the ERROR an operator sees after picking a
    /// GC-pruned session to restart (bare 500 → actionable 422); the deeper UX
    /// defect was that such a session was OFFERED as a restart option at all.
    /// Computing this once here — server-side, where the full record (`last_cwd`
    /// included) and filesystem access both live — lets every listing surface
    /// (the bare-`tm` guided default, the `tm ls` picker, and `tm sessions ls`,
    /// all of which read this same list endpoint) mark/exclude the session
    /// BEFORE the operator selects it, instead of only failing loudly after the
    /// fact. Always `false` for live/provisioning/decommissioned sessions —
    /// see `session_manager::resume_workdir::is_unresumable`.
    /// What: computed by [`super::list_managed_sessions`]/[`super::get_managed_session`]
    /// via `session_manager::resume_workdir::is_unresumable`; every other
    /// handler that builds a `SessionSummary` via `record_to_summary` leaves it
    /// at its `false` default (freshly spawned/reactivated/decommissioned
    /// sessions are never mid-flight through this predicate).
    /// Test: `list_marks_dead_stopped_session_unresumable`,
    /// `list_leaves_live_and_healthy_stopped_sessions_unmarked` in
    /// `super::tests`.
    #[serde(default)]
    pub unresumable: bool,
    /// True when this session's deployed `.claude/{agents,skills}` have
    /// drifted from the current bundled/catalog source (issue #2444).
    ///
    /// Why: `#2002`'s asset deployment is one-shot at launch — a long-lived
    /// session never re-syncs when the catalog/bundled agent or skill source
    /// changes underneath it, so it silently keeps running stale content
    /// (skills, agent rosters) with no signal anywhere that a refresh is due.
    /// Surfacing it here, alongside `unresumable`, lets every listing surface
    /// that already reads this endpoint (`tm sessions ls`, the picker) flag a
    /// session that needs `tm sessions sync-assets` run against it. Always
    /// `false` for `provisioning`/`decommissioned` sessions — see
    /// [`crate::core::session_assets::session_assets_stale`]'s gate in
    /// `checked_summaries` for exactly which states are probed.
    /// What: computed by `checked_summaries`/`record_to_summary_checked` via
    /// [`crate::core::session_assets::session_assets_stale`]; every other
    /// handler that builds a `SessionSummary` via `record_to_summary` leaves it
    /// at its `false` default.
    /// Test: `checked_summaries_flags_stale_assets_only_for_relevant_states` in
    /// `super::tests`.
    #[serde(default)]
    pub stale_assets: bool,
    /// True when a tmux client is currently ATTACHED to this session's tmux
    /// session (a live probe reconciled against real tmux, not the persisted
    /// `state`).
    ///
    /// Why: after a daemon restart (before the reconcile pass runs) a record's
    /// persisted `state` can read `stopped` even though its tmux session is
    /// alive and the operator is attached — the `tm ls` picker then wrongly
    /// showed EVERY session as `(stopped)` and offered `restart`. The list
    /// handler now reconciles the displayed `state` against live tmux and, when
    /// a client is attached, sets this flag so surfaces render `attached` and
    /// offer connect rather than a destructive restart.
    /// What: computed by [`super::list_managed_sessions`] from the live tmux
    /// attached-session set; `false` on every non-list summary (the default).
    /// Test: `reconcile_live_state_flips_stopped_to_active_when_alive` (unit);
    /// end-to-end coverage in the `session_lifecycle` integration suite.
    #[serde(default)]
    pub attached: bool,
}
