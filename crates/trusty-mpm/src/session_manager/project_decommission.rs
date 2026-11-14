//! Decommission a whole project: stop its sessions, keep everything on disk
//! (#10988).
//!
//! Why: Bob's ruling (epic #10987) — a decommissioned project keeps its
//! workspaces and indexes, its sessions stop, and it shows decommissioned until
//! a new session runs. `decommission_with_root` removes workspaces and must not
//! grow a flag (its own doc), so this is a separate function that shares no
//! removal path with it.
//! What: [`SessionManager::decommission_project`] stops each live session bound
//! to the project, shuts its search index and palace down through
//! [`IndexShutdown`] (#10989: park and unload, data kept), then stamps the
//! registry row. It removes no directory and deletes no index data.
//! Test: `project_decommission_tests.rs`, `index_shutdown_tests.rs`;
//! `decommission_keeps_index_data_when_deletion_is_allowed` in
//! `search_gc_guard_tests`.

use chrono::Utc;
use serde::{Deserialize, Serialize};
use thiserror::Error;
use tracing::{info, warn};

use super::architect::{ProtectedVerb, refusal, refuses_verbs};
use super::index_shutdown::{IndexShutdown, IndexShutdownReport, ShutdownTarget};
use super::manager::SessionManager;
use super::record::{ManagedSessionState, SessionRecord};
use crate::project::{Project, ProjectRegistry, ProjectStoreError, resolve_session_project};

/// Why a project decommission did not complete (#10988).
#[derive(Debug, Error)]
pub enum ProjectDecommissionError {
    /// The registry read or write failed, or the project is unknown.
    #[error(transparent)]
    Registry(#[from] ProjectStoreError),
    /// A bound session is the Architect's; nothing was stopped (#8942).
    #[error("{0}")]
    Protected(String),
    /// At least one session did not stop; the project stays active.
    ///
    /// The sessions in `stopped` did stop — this is reported, not undone.
    #[error(
        "project '{name}' was not decommissioned: {} session(s) failed to stop ({}); \
         stopped: [{}]",
        failed.len(),
        failed.iter().map(|(id, e)| format!("{id}: {e}")).collect::<Vec<_>>().join("; "),
        stopped.join(", ")
    )]
    PartialStop {
        /// The project name.
        name: String,
        /// Ids of the sessions that did stop.
        stopped: Vec<String>,
        /// `(session id, error)` for each session that did not.
        failed: Vec<(String, String)>,
    },
    /// Every stop succeeded but the registry stamp failed; the project stays
    /// active (`decommissioned_at` unset) while the sessions in `stopped` are
    /// already down.
    // #10988: a bare `Registry` here lost the stopped list (fail-open).
    #[error(
        "project '{name}' was not decommissioned: its sessions stopped but the registry \
         write failed ({source}); stopped: [{}]",
        stopped.join(", ")
    )]
    StampFailed {
        /// The project name.
        name: String,
        /// Ids of the sessions that did stop.
        stopped: Vec<String>,
        /// The registry write error.
        source: ProjectStoreError,
    },
    /// Every stop succeeded but the search index or the palace may still be
    /// loaded; the project stays active (#10989). A retry re-runs the
    /// shutdown, which is idempotent.
    #[error(
        "project '{name}' was not decommissioned: its sessions stopped but its indexes \
         may still be loaded ({}); stopped: [{}]",
        still_loaded.join("; "),
        stopped.join(", ")
    )]
    IndexShutdown {
        /// The project name.
        name: String,
        /// Ids of the sessions that did stop.
        stopped: Vec<String>,
        /// One line per item that may still be loaded, with its error.
        still_loaded: Vec<String>,
        /// The whole shutdown report.
        report: IndexShutdownReport,
    },
}

/// What a completed project decommission did (#10988).
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ProjectDecommissionReport {
    /// The registry row after the stamp.
    pub project: Project,
    /// Ids of the sessions this call stopped.
    pub stopped: Vec<String>,
    /// Ids of bound sessions that were already not running, left as they were.
    pub left_as_is: Vec<String>,
    /// What the search-index and palace shutdown did (#10989). `None` only
    /// from a daemon that predates the shutdown.
    #[serde(default)]
    pub index_shutdown: Option<IndexShutdownReport>,
}

impl SessionManager {
    /// Decommission project `name`: stop its live sessions, keep its data.
    ///
    /// Why: see the module doc. A partial stop must never read as success, so
    /// the registry is stamped only after every stop succeeded.
    /// What: binds sessions to the project the way the fleet view does
    /// ([`resolve_session_project`]) and skips terminal records. Refuses with
    /// [`ProjectDecommissionError::Protected`] before any stop when a bound
    /// record is the Architect's. Stops each `Active`/`Provisioning` session
    /// with [`Self::stop`] (workspace intact, record resumable). Any failure
    /// returns [`ProjectDecommissionError::PartialStop`] and leaves the project
    /// active. Otherwise stamps `decommissioned_at` via
    /// [`ProjectRegistry::mark_decommissioned`]; a stamp failure returns
    /// [`ProjectDecommissionError::StampFailed`] with the stopped ids. No
    /// workspace removal and no index delete; the index shutdown runs through
    /// [`IndexShutdown::acquire`] (see [`Self::decommission_project_with`]).
    /// Test: `decommission_project_stops_live_sessions_and_keeps_the_workspace`,
    /// `decommission_project_refuses_an_architect_session`,
    /// `decommission_project_failed_stop_leaves_the_project_active`,
    /// `decommission_project_stamp_failure_reports_the_stopped_sessions`,
    /// `decommission_project_unknown_name_errors`.
    pub async fn decommission_project(
        &self,
        registry: &ProjectRegistry,
        name: &str,
    ) -> Result<ProjectDecommissionReport, ProjectDecommissionError> {
        self.decommission_project_with(registry, name, IndexShutdown::acquire())
            .await
    }

    /// [`Self::decommission_project`] with an explicit shutdown capability.
    ///
    /// What: as [`Self::decommission_project`]; after every stop succeeded it
    /// runs `shutdown` (#10989) — `Err(reason)` reports both items skipped —
    /// and an item that may still be loaded returns
    /// [`ProjectDecommissionError::IndexShutdown`] without stamping.
    /// Test: `decommission_project_parks_and_unloads_and_never_deletes`,
    /// `decommission_project_failed_park_leaves_the_project_active`,
    /// `decommission_project_failed_unload_leaves_the_project_active`.
    pub async fn decommission_project_with(
        &self,
        registry: &ProjectRegistry,
        name: &str,
        shutdown: Result<IndexShutdown, String>,
    ) -> Result<ProjectDecommissionReport, ProjectDecommissionError> {
        let project = registry.get(name).await?;
        let all: Vec<SessionRecord> = self
            .list()
            .await
            .into_iter()
            .filter(|r| resolve_session_project(r, std::slice::from_ref(&project)).is_some())
            .collect();
        let bound: Vec<SessionRecord> = all
            .iter()
            .filter(|r| !r.state.is_terminal())
            .cloned()
            .collect();
        // #8942: refuse the whole verb before any stop, so nothing half-runs.
        if let Some(r) = bound.iter().find(|r| refuses_verbs(r.kind, &r.state)) {
            return Err(ProjectDecommissionError::Protected(refusal(
                &r.tmux_name,
                r.kind,
                ProtectedVerb::Decommission,
            )));
        }
        let mut stopped = Vec::new();
        let mut left_as_is = Vec::new();
        let mut failed = Vec::new();
        for r in &bound {
            if !matches!(
                r.state,
                ManagedSessionState::Active | ManagedSessionState::Provisioning
            ) {
                left_as_is.push(r.id.to_string());
                continue;
            }
            match self.stop(&r.id).await {
                Ok(_) => stopped.push(r.id.to_string()),
                Err(e) => {
                    warn!(project = %name, id = %r.id, "project decommission: stop failed: {e}");
                    failed.push((r.id.to_string(), e.to_string()));
                }
            }
        }
        // #10988 fail-open check: a failed stop never stamps the project.
        if !failed.is_empty() {
            return Err(ProjectDecommissionError::PartialStop {
                name: name.to_string(),
                stopped,
                failed,
            });
        }
        // #10989: shut the indexes down (data kept); one left loaded is no success.
        let index_shutdown = match &shutdown {
            Ok(cap) => {
                // Palace resolution reads the checkout's pin and git config.
                let held = project.clone();
                let target = tokio::task::spawn_blocking(move || shutdown_target(&held, &all))
                    .await
                    .map_err(|e| format!("shutdown target task failed: {e}"));
                match target {
                    Ok(target) => cap.shut_down(&target).await,
                    Err(e) => IndexShutdownReport::failed(&e),
                }
            }
            Err(why) => IndexShutdownReport::skipped(why),
        };
        let still_loaded = index_shutdown.still_loaded();
        if !still_loaded.is_empty() {
            warn!(project = %name, ?still_loaded, "project decommission: index shutdown failed");
            return Err(ProjectDecommissionError::IndexShutdown {
                name: name.to_string(),
                stopped,
                still_loaded,
                report: index_shutdown,
            });
        }
        // #10988: a stamp failure after the stops must still name what stopped.
        let project = match registry.mark_decommissioned(name, Utc::now()).await {
            Ok(p) => p,
            Err(source) => {
                warn!(project = %name, "project decommission: stamp failed after stops: {source}");
                return Err(ProjectDecommissionError::StampFailed {
                    name: name.to_string(),
                    stopped,
                    source,
                });
            }
        };
        info!(project = %name, stopped = stopped.len(), "project decommissioned (workspaces and index data kept, indexes unloaded)");
        Ok(ProjectDecommissionReport {
            project,
            stopped,
            left_as_is,
            index_shutdown: Some(index_shutdown),
        })
    }
}

/// What to shut down for `project`, given every session record bound to it.
///
/// What: the first bound record whose checkout (`workspace_path`, else `cwd`)
/// is on disk supplies the path; see [`ShutdownTarget::for_project`].
fn shutdown_target(project: &Project, records: &[SessionRecord]) -> ShutdownTarget {
    let root = records
        .iter()
        .map(|r| r.workspace_path.clone().unwrap_or_else(|| r.cwd.clone()))
        .find(|p| p.is_dir());
    ShutdownTarget::for_project(
        project.project_key.as_deref(),
        &project.repo_url,
        root.as_deref(),
    )
}

#[cfg(test)]
#[path = "project_decommission_tests.rs"]
mod tests;
