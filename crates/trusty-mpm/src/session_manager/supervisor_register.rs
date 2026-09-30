//! Registering the Architect's sessions with the daemon (#8942 phase 3).
//!
//! Why: `tm fleet init` starts the Architect outside the daemon, so without a
//! record `tm ls` never shows it and only the name floor protects it. A
//! record of kind `supervisor` makes it visible and gives it every lifecycle
//! refusal. Anyone may call the route, so a record that makes a session
//! unkillable is written only for a `claude` the launch record binds to the
//! project.
//! What: [`SupervisorRegistration`] is the request, [`RegistrationReport`]
//! the answer, [`RegisterError`] the refusal. [`SessionManager::register_supervisor`]
//! verifies the binding through a [`BindingVerifier`], then, in one store
//! write, upserts the Architect's record under
//! [`ManagedSessionId::for_supervisor`], a `supervisor_aux` record for each
//! helper named after the Architect whose pane runs in the Architect
//! directory provably without `claude`, and marks `deleted`, record-only, every other
//! live record with one of those tmux names and every other non-terminal
//! Architect record.
//! Test: `supervisor_register_tests.rs`.

use std::path::{Path, PathBuf};

use chrono::Utc;
use serde::{Deserialize, Serialize};
use tracing::{info, warn};

use super::manager::{ManagedError, SessionManager};
use super::record::{ManagedSessionId, ManagedSessionState, SessionRecord};
use super::session_kind::SessionKind;
use super::supervisor_floor::SUPERVISOR_ROLE;
use crate::core::architect_session::{POLL_SUFFIX, poll_session_name, validate_session_name};
use crate::core::process::PaneClaude;

/// The `-collector` helper's id role.
pub const COLLECTOR_ROLE: &str = "-collector";

/// The collector's session for Architect session `name`.
pub fn collector_session_name(name: &str) -> String {
    format!("{name}{COLLECTOR_ROLE}")
}

/// `POST /api/v1/sessions/managed/supervisor` body (#8942).
///
/// What: the Architect directory, its tmux session, and which helpers to
/// register. A helper name must be the one derived from `session`
/// ([`poll_session_name`], [`collector_session_name`]); any other is a 400.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SupervisorRegistration {
    /// The Architect project directory; absolute.
    pub dir: PathBuf,
    /// The Architect's tmux session.
    pub session: String,
    /// The poller's tmux session, when one should be registered.
    #[serde(default)]
    pub poll_session: Option<String>,
    /// The collector's tmux session, when one should be registered.
    #[serde(default)]
    pub collector_session: Option<String>,
}

/// One record the registration wrote (#8942).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegisteredSession {
    /// The stable record id.
    pub id: String,
    /// The tmux session it names.
    pub tmux_name: String,
    /// `supervisor` or `supervisor_aux`.
    pub kind: SessionKind,
}

/// What a registration did (#8942).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RegistrationReport {
    /// The records written; the Architect's comes first.
    pub registered: Vec<RegisteredSession>,
    /// Helpers not registered, each with the reason.
    #[serde(default)]
    pub skipped: Vec<String>,
    /// Ids now `deleted`: other live records with a registered name, and
    /// every other non-terminal Architect record.
    #[serde(default)]
    pub replaced: Vec<String>,
}

/// Why the daemon refused a registration (#8942).
#[derive(Debug, thiserror::Error)]
pub enum RegisterError {
    /// The request itself is malformed (400).
    #[error("invalid Architect registration: {0}")]
    Invalid(String),
    /// The `claude` in the session is not the project's bound Architect (403).
    #[error("not registered: {0}")]
    Unbound(String),
    /// The store could not be written (500).
    #[error(transparent)]
    Managed(#[from] ManagedError),
}

/// Whether the `claude` in tmux session `session` is the Architect tm
/// launched for `dir` (#8942).
///
/// Why: a seam, so the manager's tests never reach tmux or the process table;
/// the daemon's implementation asks `check_session_binding`.
pub trait BindingVerifier: Send + Sync {
    /// `Ok(())` only for a bound Architect; any doubt is `Err(reason)`.
    fn verify(&self, dir: &Path, session: &str) -> Result<(), String>;
}

impl SessionManager {
    /// Register the Architect's sessions for `reg.dir` (#8942); see the module doc.
    ///
    /// Why: design §2/§4 — one live row per Architect, under a stable id, and
    /// only for a verified binding (fail closed: no record, no protection
    /// granted, on any doubt).
    /// What: validates the request (absolute dir, valid names, helper names
    /// derived from the Architect's), runs `verifier` (an `Err` is
    /// [`RegisterError::Unbound`] and writes nothing), then builds the records
    /// and writes them and the replacements in one `upsert_many`. A helper is
    /// registered only when the driver reports its pane's cwd as `reg.dir`
    /// and `pane_claude` proves no `claude` runs in the pane, the pane's own
    /// process included; `Present`, `Unknown` or another cwd lists it in
    /// `skipped`.
    /// Test: `register_supervisor_refuses_an_unbound_session`,
    /// `relaunching_the_architect_replaces_its_record`,
    /// `a_helper_not_named_after_the_architect_is_refused`,
    /// `a_helper_whose_pane_runs_claude_is_not_registered`,
    /// `a_helper_whose_pane_process_is_claude_is_not_registered`,
    /// `a_helper_whose_pane_pid_cannot_be_read_is_not_registered`,
    /// `registration_deletes_every_stale_architect_record`,
    /// `a_helper_whose_pane_cannot_be_placed_is_not_registered`,
    /// `register_supervisor_refuses_a_relative_dir_or_a_bad_name`.
    pub async fn register_supervisor(
        &self,
        reg: &SupervisorRegistration,
        verifier: &dyn BindingVerifier,
    ) -> Result<RegistrationReport, RegisterError> {
        let dir = validated_dir(reg)?;
        verifier
            .verify(&dir, &reg.session)
            .map_err(RegisterError::Unbound)?;
        let mut wanted = vec![(
            reg.session.clone(),
            SessionKind::Supervisor,
            SUPERVISOR_ROLE,
        )];
        let mut report = RegistrationReport::default();
        let helpers = [
            (&reg.poll_session, POLL_SUFFIX),
            (&reg.collector_session, COLLECTOR_ROLE),
        ];
        for (name, role) in helpers {
            let Some(name) = name else { continue };
            match self.tmux.get_pane_cwd(name) {
                // #8942 critic HIGH: a pane running `claude` is a PM or an
                // Architect, never a helper; "cannot tell" skips it too.
                Some(cwd) if same_dir(&cwd, &dir) => match self.tmux.pane_claude(name) {
                    PaneClaude::Absent => {
                        wanted.push((name.clone(), SessionKind::SupervisorAux, role));
                    }
                    PaneClaude::Present => report.skipped.push(format!(
                        "{name}: a `claude` runs in its pane, so it is no helper"
                    )),
                    PaneClaude::Unknown => report.skipped.push(format!(
                        "{name}: whether a `claude` runs in its pane cannot be read"
                    )),
                },
                Some(cwd) => report.skipped.push(format!(
                    "{name}: its pane runs in {}, not in {}",
                    cwd.display(),
                    dir.display()
                )),
                None => report.skipped.push(format!(
                    "{name}: its pane's directory cannot be read (not running?)"
                )),
            }
        }
        let mut guard = self.store.write().await;
        let existing = guard.all().await.map_err(ManagedError::from)?;
        let now = Utc::now();
        let mut writes = Vec::new();
        for (name, kind, role) in &wanted {
            let id = ManagedSessionId::for_supervisor(&dir, role);
            let prior = existing.iter().find(|r| r.id == id);
            writes.push(supervisor_record(
                id,
                name,
                &dir,
                *kind,
                prior,
                self.tmux.get_pane_id(name),
            ));
            report.registered.push(RegisteredSession {
                id: id.to_string(),
                tmux_name: name.clone(),
                kind: *kind,
            });
        }
        let ids: Vec<ManagedSessionId> = writes.iter().map(|r| r.id).collect();
        for other in &existing {
            if other.state.is_terminal() || ids.contains(&other.id) {
                continue;
            }
            let live = matches!(
                other.state,
                ManagedSessionState::Active | ManagedSessionState::Provisioning
            );
            // #8942 critic MEDIUM: one Architect per user, so every other
            // Architect record is stale, and a stopped one would pin its name.
            let stale_architect = matches!(
                other.kind,
                SessionKind::Supervisor | SessionKind::SupervisorAux
            );
            if stale_architect || (live && wanted.iter().any(|w| w.0 == other.tmux_name)) {
                let mut replaced = other.clone();
                replaced.set_lifecycle_state(ManagedSessionState::Deleted, now);
                warn!(id = %other.id, name = %other.tmux_name, "#8942: the Architect's registration marked this record deleted (record only)");
                report.replaced.push(other.id.to_string());
                writes.push(replaced);
            }
        }
        guard
            .upsert_many(writes)
            .await
            .map_err(ManagedError::from)?;
        drop(guard);
        self.bump_residency_generation();
        info!(dir = %dir.display(), session = %reg.session, "#8942: registered the Architect's sessions");
        Ok(report)
    }
}

/// `reg.dir`, canonical when it can be, after the request checks.
fn validated_dir(reg: &SupervisorRegistration) -> Result<PathBuf, RegisterError> {
    if !reg.dir.is_absolute() {
        return Err(RegisterError::Invalid(format!(
            "dir {} is not absolute",
            reg.dir.display()
        )));
    }
    validate_session_name(&reg.session).map_err(RegisterError::Invalid)?;
    // #8942 critic HIGH: a helper name is derived from the Architect's by the
    // rule `tm fleet init` uses; any other name could shield an unrelated pane.
    let helpers = [
        (&reg.poll_session, poll_session_name(&reg.session)),
        (&reg.collector_session, collector_session_name(&reg.session)),
    ];
    for (sent, expected) in helpers {
        if let Some(sent) = sent.as_ref().filter(|sent| **sent != expected) {
            return Err(RegisterError::Invalid(format!(
                "helper session {sent:?} is not {expected:?}, the name derived from {:?}",
                reg.session
            )));
        }
    }
    Ok(std::fs::canonicalize(&reg.dir).unwrap_or_else(|_| reg.dir.clone()))
}

/// Whether `a` and `b` name the same directory once canonicalized.
fn same_dir(a: &Path, b: &Path) -> bool {
    let canon = |p: &Path| std::fs::canonicalize(p).unwrap_or_else(|_| p.to_path_buf());
    canon(a) == canon(b)
}

/// The record for one of the Architect's sessions, keeping `prior`'s history.
fn supervisor_record(
    id: ManagedSessionId,
    name: &str,
    dir: &Path,
    kind: SessionKind,
    prior: Option<&SessionRecord>,
    pane_id: Option<String>,
) -> SessionRecord {
    let task = match kind {
        SessionKind::Supervisor => "the Architect (tm fleet init)",
        _ => "the Architect's helper (tm fleet init)",
    };
    SessionRecord {
        id,
        tmux_name: name.to_owned(),
        cwd: dir.to_path_buf(),
        task: task.to_owned(),
        state: ManagedSessionState::Active,
        created_at: prior.map_or_else(Utc::now, |p| p.created_at),
        last_activity_at: prior.and_then(|p| p.last_activity_at),
        workspace_path: Some(dir.to_path_buf()),
        repo_url: None,
        branch: None,
        pending_decision: None,
        proposed_default: None,
        correlation: Default::default(),
        runtime: crate::runtime::RuntimeKind::default(),
        ephemeral: false,
        // #1511: tm did not provision the directory; nothing may delete it.
        workspace_owned: false,
        source_id: None,
        claude_session_id: prior.and_then(|p| p.claude_session_id.clone()),
        scrollback_path: None,
        last_cwd: None,
        deliverable_id: None,
        pane_id,
        injection_status: Default::default(),
        worktree_owner: None,
        terminal_at: None,
        stop_cause: None,
        kind,
    }
}

#[cfg(test)]
#[path = "supervisor_register_tests.rs"]
mod tests;
