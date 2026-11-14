//! Undo a decommission's index park when the project starts again (#10989).
//!
//! Why: decommission parks the project's trusty-search index with reason
//! `decommissioned` ([`super::index_shutdown`]). A session started for the
//! project afterwards needs that index back — loaded, watched and resolvable —
//! without an operator running `trusty-search project unpark`. The palace
//! needs no step here: trusty-memory reopens an unloaded palace on its next
//! use.
//! What: [`IndexRestore`] reads `search.project.map`, picks the project's
//! parked rows whose reason is `decommissioned` (a park for any other reason
//! is left alone) and sends `search.project.unpark` for each. An unpark the
//! daemon does not confirm is [`RestoreState::Failed`];
//! [`restore_for_session`] turns that into the spawn's error, so a spawn never
//! reports a still-parked index as live.
//! Test: `index_restore_tests.rs`.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};
use trusty_common::search_rpc;

use super::index_shutdown::{CALL_TIMEOUT, is_true, listening, repo_identity};
use super::manager::SessionManager;
use super::record::SessionRecord;
use crate::project::{ProjectRegistry, resolve_session_project};

/// Read the project map (pins and parked rows).
pub(crate) const METHOD_MAP: &str = "search.project.map";
/// Unpark one index: clear its row, reload it, restart its watcher.
pub(crate) const METHOD_UNPARK: &str = "search.project.unpark";
/// The park reason decommission writes; the only one this module undoes.
pub(crate) const REASON_DECOMMISSIONED: &str = "decommissioned";

/// What the restore did for one session's project (#10989).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum RestoreState {
    /// Every decommission-parked index of the project is unparked.
    Unparked {
        /// The unparked index ids.
        ids: Vec<String>,
    },
    /// No index of the project is parked for a decommission.
    NothingParked,
    /// trusty-search is not running, so there was nothing to ask.
    NotRunning,
    /// No call was made: this is a test process.
    Skipped {
        /// Why no call was made.
        reason: String,
    },
    /// The restore failed; the named indexes may still be parked.
    Failed {
        /// The indexes that may still be parked (empty when the map was unread).
        ids: Vec<String>,
        /// What failed.
        error: String,
    },
}

/// Whose parked rows the restore undoes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RestoreTarget {
    /// The project's `owner/repo` identity: a row's `key` when it has one.
    pub identity: Option<String>,
    /// The session checkout: a row whose `root_path` contains it matches.
    pub root: Option<PathBuf>,
}

impl RestoreTarget {
    /// Whether map row `row` is this project's decommission park.
    fn owns(&self, row: &Value) -> bool {
        if row.get("reason").and_then(Value::as_str) != Some(REASON_DECOMMISSIONED) {
            return false;
        }
        let key = row.get("key").and_then(Value::as_str);
        let root = row.get("root_path").and_then(Value::as_str).map(Path::new);
        (self.identity.is_some() && key == self.identity.as_deref())
            || self
                .root
                .as_deref()
                .zip(root)
                .is_some_and(|(session, index)| session.starts_with(index))
    }
}

/// The capability to unpark a project's decommission-parked index.
///
/// Why: as [`super::index_shutdown::IndexShutdown`] — a private constructor
/// so a test process never unparks the operator's live index (#4743).
pub struct IndexRestore {
    search: Option<PathBuf>,
}

impl IndexRestore {
    /// Acquire the capability, or `Err(reason)` in a test process.
    pub fn acquire() -> Result<Self, String> {
        if trusty_common::running_under_test_harness() {
            return Err(format!(
                "test process: no trusty-search call made (set {} to override)",
                trusty_common::test_harness::ALLOW_PRODUCTION_ENV
            ));
        }
        Ok(Self {
            search: search_rpc::search_socket().ok(),
        })
    }

    /// The capability against an explicit socket, for tests.
    #[cfg(test)]
    pub(crate) fn at(search: Option<PathBuf>) -> Self {
        Self { search }
    }

    /// Unpark every index of `target` that a decommission parked.
    ///
    /// What: no daemon is [`RestoreState::NotRunning`]; an unreadable map or
    /// any unpark not answered `unparked: true` is [`RestoreState::Failed`],
    /// naming every id that may still be parked.
    /// Test: `restore_unparks_only_the_projects_decommission_park`,
    /// `a_refused_unpark_is_a_failure_naming_the_index`.
    pub async fn restore(&self, target: &RestoreTarget) -> RestoreState {
        let socket = match listening(self.search.as_deref()).await {
            Ok(Some(socket)) => socket,
            Ok(None) => return RestoreState::NotRunning,
            // #10989 F4: an unreachable socket cannot tell that nothing is parked.
            Err(error) => {
                return RestoreState::Failed {
                    ids: Vec::new(),
                    error,
                };
            }
        };
        let map = match search_rpc::call_at(socket, METHOD_MAP, json!({}), CALL_TIMEOUT).await {
            Ok(map) => map,
            Err(e) => {
                return RestoreState::Failed {
                    ids: Vec::new(),
                    error: format!("{METHOD_MAP}: {e:#}"),
                };
            }
        };
        let ids: Vec<String> = map
            .get("parked")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter(|row| target.owns(row))
            .filter_map(|row| row.get("index_id").and_then(Value::as_str))
            .map(str::to_string)
            .collect();
        if ids.is_empty() {
            return RestoreState::NothingParked;
        }
        let mut failed = Vec::new();
        for id in &ids {
            let params = json!({ "index_id": id });
            match search_rpc::call_at(socket, METHOD_UNPARK, params, CALL_TIMEOUT).await {
                Ok(body) if is_true(&body, "unparked") => {}
                Ok(body) => failed.push((id.clone(), format!("unpark not confirmed: {body}"))),
                Err(e) => failed.push((id.clone(), format!("{e:#}"))),
            }
        }
        if failed.is_empty() {
            return RestoreState::Unparked { ids };
        }
        RestoreState::Failed {
            ids: failed.iter().map(|(id, _)| id.clone()).collect(),
            error: failed
                .iter()
                .map(|(id, e)| format!("{id}: {e}"))
                .collect::<Vec<_>>()
                .join("; "),
        }
    }
}

/// Undo `record`'s project's decommission park; `Err` names what stays parked.
///
/// Why: #10989 — every spawn and resume reaches this through
/// [`SessionManager::restore_parked_index`], so a project started again after
/// a decommission gets its index back, and an unpark that fails is the
/// route's error rather than a silent success.
/// What: the identity comes from the registry project `record` binds to (its
/// `project_key`, else its `repo_url`), else from `record.repo_url`; the root
/// is the record's workspace, else its cwd. `restore` `Err(reason)` (a test
/// process) is [`RestoreState::Skipped`]. A registry list failure and
/// [`RestoreState::Failed`] are `Err`.
/// Test: `decommission_then_spawn_unparks_the_index`,
/// `a_failed_unpark_fails_the_spawn_by_name`.
pub async fn restore_for_session(
    registry: &ProjectRegistry,
    record: &SessionRecord,
    restore: Result<IndexRestore, String>,
) -> Result<RestoreState, String> {
    let restore = match restore {
        Ok(cap) => cap,
        Err(reason) => return Ok(RestoreState::Skipped { reason }),
    };
    let projects = registry.list().await.map_err(|e| {
        format!(
            "session {} ({}) is running, but tm could not read the project registry to \
             unpark a decommissioned index (#10989): {e}",
            record.id, record.tmux_name
        )
    })?;
    let session_url = record.repo_url.as_deref().unwrap_or_default();
    let identity = match resolve_session_project(record, &projects) {
        Some(p) => repo_identity(p.project_key.as_deref(), &p.repo_url),
        None => repo_identity(None, session_url),
    };
    let target = RestoreTarget {
        identity,
        root: Some(
            record
                .workspace_path
                .clone()
                .unwrap_or_else(|| record.cwd.clone()),
        ),
    };
    match restore.restore(&target).await {
        RestoreState::Failed { ids, error } => Err(format!(
            "session {} ({}) is running, but its project's search index [{}] is still \
             parked from a decommission and was not reloaded (#10989): {error}; run \
             `trusty-search project unpark <index_id>`",
            record.id,
            record.tmux_name,
            ids.join(", ")
        )),
        RestoreState::NotRunning => {
            tracing::warn!(
                id = %record.id,
                "trusty-search is not running: a decommission park of this project, if any, \
                 stays until the next session start (#10989)"
            );
            Ok(RestoreState::NotRunning)
        }
        state => Ok(state),
    }
}

impl SessionManager {
    /// The one post-start restore `spawn_managed` and `resume_managed` call
    /// (#10989 F3): [`restore_for_session`] with this manager's capability.
    ///
    /// Test: `a_resume_after_a_decommission_unparks_the_index`.
    pub(crate) async fn restore_parked_index(
        &self,
        registry: &ProjectRegistry,
        record: &SessionRecord,
    ) -> Result<RestoreState, String> {
        #[cfg(test)]
        if let Some(socket) = self.index_restore_socket.get() {
            let cap = Ok(IndexRestore::at(Some(socket.clone())));
            return restore_for_session(registry, record, cap).await;
        }
        restore_for_session(registry, record, IndexRestore::acquire()).await
    }
}

#[cfg(test)]
#[path = "index_restore_tests.rs"]
pub(crate) mod tests;
