//! Best-effort `search.project.ensure` at project registration (#10531).
//!
//! Why: `project_register` wrote a registry record and created no search index,
//! so the first session to launch minted one under whatever id it derived, and
//! nothing refused a checkout nested inside another index's root. Registration
//! is bookkeeping and must never fail or wait on the search daemon.
//! What: [`spawn_ensure_project_index`] resolves the project's local checkout
//! and, on a detached task, asks the daemon to find-or-create and pin its
//! index. [`ensure_call`] is the one bounded call and classifies the answer as
//! an [`EnsureOutcome`]; every outcome is logged and none is raised. A daemon
//! that is down leaves the project with no index, which `tm doctor` reports as
//! `search_project_no_index`. A host-state-gated or test process sends nothing.
//! Test: `search_project_ensure_tests.rs`.

use std::path::Path;
use std::sync::Arc;
use std::time::Duration;

use serde_json::Value;

use crate::daemon::search_rpc::{self, CODE_CONFLICT, SearchRpcError};
use crate::daemon::state::DaemonState;
use crate::project::Project;

/// Bound on the one `search.project.ensure` call.
///
/// A create can wait on the embedder gate, so this is longer than a probe.
const ENSURE_TIMEOUT: Duration = Duration::from_secs(30);

/// What one `search.project.ensure` call came to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum EnsureOutcome {
    /// The daemon answered with an index.
    Ensured {
        /// The project's index id.
        index_id: String,
        /// `true` when this call registered it.
        created: bool,
    },
    /// The daemon refused; for an overlap, `overlapping_index_id` names the
    /// index in the way.
    Refused {
        /// The daemon's JSON-RPC code.
        code: i64,
        /// The daemon's message.
        message: String,
        /// `data.overlapping_index_id`, when the refusal carried one.
        overlapping_index_id: Option<String>,
    },
    /// No answer: the daemon is down, timed out or broke the frame.
    Unreachable(String),
}

/// Make the one bounded call and classify it.
///
/// Why: separated from the spawn so the wire and the classification are
/// testable against a mock socket.
/// What: sends `{project_key, name, root}`; a result without `index_id` is
/// `Unreachable` ("answered without an index id"), never `Ensured`.
/// Test: `ensure_call_sends_the_wire_shape_and_reads_the_index_id`,
/// `an_overlap_refusal_names_the_overlapping_index`,
/// `a_dead_socket_is_unreachable`.
pub(crate) async fn ensure_call(
    socket: &Path,
    project_key: &str,
    name: &str,
    root: &Path,
    timeout: Duration,
) -> EnsureOutcome {
    let params = trusty_common::search_rpc::ensure_project_params(project_key, name, root, None);
    match search_rpc::call_at(
        socket,
        trusty_common::search_rpc::METHOD_PROJECT_ENSURE,
        params,
        timeout,
    )
    .await
    {
        Ok(body) => match body.get("index_id").and_then(Value::as_str) {
            Some(id) => EnsureOutcome::Ensured {
                index_id: id.to_string(),
                created: body
                    .get("created")
                    .and_then(Value::as_bool)
                    .unwrap_or(false),
            },
            None => EnsureOutcome::Unreachable("answered without an index id".to_string()),
        },
        Err(e) => match e.downcast_ref::<SearchRpcError>() {
            Some(rpc) => EnsureOutcome::Refused {
                code: rpc.code,
                message: rpc.message.clone(),
                overlapping_index_id: rpc
                    .data
                    .as_ref()
                    .and_then(|d| d.get("overlapping_index_id"))
                    .and_then(Value::as_str)
                    .map(str::to_string),
            },
            None => EnsureOutcome::Unreachable(format!("{e:#}")),
        },
    }
}

/// Log an outcome at the level its meaning earns. Never raises.
fn report(project: &str, outcome: &EnsureOutcome) {
    match outcome {
        EnsureOutcome::Ensured { index_id, created } => tracing::info!(
            project,
            index_id,
            created,
            "#10531: trusty-search index ensured at project registration"
        ),
        EnsureOutcome::Refused {
            code,
            message,
            overlapping_index_id,
        } => tracing::warn!(
            project,
            code,
            conflict = *code == CODE_CONFLICT,
            overlapping_index_id = overlapping_index_id.as_deref().unwrap_or(""),
            "#10531: trusty-search refused the project's index: {message}"
        ),
        EnsureOutcome::Unreachable(why) => tracing::info!(
            project,
            "#10531: trusty-search did not answer ({why}); the project has no index until \
             it does — `tm doctor` reports search_project_no_index"
        ),
    }
}

/// Ask the search daemon for `project`'s index, without waiting for it.
///
/// Why: see the module doc; both registration transports call this through
/// [`crate::daemon::project_adoption::adopt_pre_existing_worktrees`]'s sibling
/// hook, so neither can diverge.
/// What: returns at once. Skips, with nothing sent, when the process is a test
/// harness, the host-state gate refuses this framework root, the record has no
/// `project_key`, or the project has no checkout on disk. Otherwise spawns one
/// task that dials the search socket if it exists and calls [`ensure_call`].
/// Test: `plan_is_empty_without_a_key_or_a_checkout`.
pub(crate) fn spawn_ensure_project_index(state: &Arc<DaemonState>, project: &Project) {
    if trusty_common::running_under_test_harness() {
        return;
    }
    if let Some(reason) = crate::daemon::host_state_refusal(state) {
        tracing::debug!("#10531: skipped search.project.ensure — {reason}");
        return;
    }
    let Some(root) = plan(project) else {
        return;
    };
    let (name, key) = (
        project.name.clone(),
        project.project_key.clone().unwrap_or_default(),
    );
    tokio::spawn(async move {
        let outcome = match search_rpc::search_socket() {
            Ok(socket) if socket.exists() => {
                ensure_call(&socket, &key, &name, &root, ENSURE_TIMEOUT).await
            }
            Ok(socket) => EnsureOutcome::Unreachable(format!("no socket at {}", socket.display())),
            Err(e) => EnsureOutcome::Unreachable(format!("{e:#}")),
        };
        report(&name, &outcome);
    });
}

/// The checkout to ensure, or `None` when there is nothing to send.
///
/// Test: `plan_is_empty_without_a_key_or_a_checkout`.
pub(crate) fn plan(project: &Project) -> Option<std::path::PathBuf> {
    project
        .project_key
        .as_deref()
        .filter(|k| !k.trim().is_empty())?;
    crate::project::local_checkout_for(project).filter(|p| p.is_dir())
}

#[cfg(test)]
#[path = "search_project_ensure_tests.rs"]
mod search_project_ensure_tests;
