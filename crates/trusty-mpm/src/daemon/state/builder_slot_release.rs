//! Release the builder slot of a subagent the USER stopped (#8548).
//!
//! Why: a builder's slot lease is its delegation record (see
//! [`super::builder_slots`]), and that record ends on a `SubagentStop`, a
//! `TaskStop` the PM issues, the dispatching session's death, or the 45-minute
//! TTL. A stop the user makes from the harness — the task panel, the SDK
//! `stop_task`, the remote bridge — produces none of the first three: the agent
//! is aborted, so it emits no `SubagentStop`; the PM made no `TaskStop` call, so
//! no `PostToolUse` names it; and the PM is still alive. The slot therefore
//! stayed held until the TTL, and on a machine sized for one builder that
//! blocked every dispatch for up to 45 minutes.
//!
//! The evidence the hooks do not carry is on disk. On every user-sourced stop
//! Claude Code writes `"stoppedByUser": true` into the agent's metadata sidecar,
//! `<transcript dir>/<session>/subagents/agent-<agent_id>.meta.json`, and writes
//! `false` there when the user resumes the agent (observed in Claude Code
//! 2.1.283). The sidecar is named by the agent's own id and carries the
//! `toolUseId` of the dispatch that spawned it, so it identifies one lease.
//!
//! What: [`DaemonState::release_user_stopped_builders`] reads that sidecar for
//! every live builder lease and cancels the record whose sidecar says the user
//! stopped it. Two callers bound the release: the 60 s delegation sweep, and the
//! builder-slot claim route, which runs it before it counts holders.
//!
//! # Fail direction
//!
//! Closed. An absent sidecar, one with no marker, one naming a different
//! dispatch, and one that cannot be read or parsed all keep the lease; the last
//! is logged at `warn` with the path. The TTL still bounds every lease this
//! module cannot end.
//! Test: `builder_slot_release_tests`.

use std::io::Read;
use std::path::{Component, Path, PathBuf};

use serde_json::Value;

use crate::core::agent::DelegationStatus;
use crate::core::dispatch_isolation::agent_is_builder;

use super::core::DaemonState;

/// The largest sidecar this module reads, in bytes.
///
/// Why: the sidecar holds a dozen short fields; a file far past that is not one
/// Claude Code wrote, and reading it whole on the claim path would cost time.
const MAX_SIDECAR_BYTES: u64 = 64 * 1024;

/// What one agent's sidecar says about a user stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StopMarker {
    /// The user stopped this dispatch's agent, and has not resumed it.
    Stopped,
    /// No sidecar, or a sidecar without a `true` marker.
    Absent,
    /// The sidecar's `toolUseId` names a different dispatch than the lease.
    OtherDispatch,
}

/// The sidecar path for `agent_id`, derived from the dispatcher's transcript.
///
/// Why: the transcript path came from a hook payload, so the derivation refuses
/// any shape that could aim the read outside that transcript's directory.
/// What: `<dir>/<stem>/subagents/agent-<agent_id>.meta.json` for an absolute
/// `transcript` with no `..` component and an ASCII-alphanumeric `agent_id`;
/// `None` otherwise.
/// Test: `a_traversing_transcript_path_reads_no_sidecar_8548`.
pub(crate) fn stop_marker_path(transcript: &Path, agent_id: &str) -> Option<PathBuf> {
    if agent_id.is_empty() || !agent_id.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return None;
    }
    if !transcript.is_absolute() || transcript.components().any(|c| c == Component::ParentDir) {
        return None;
    }
    let stem = transcript.file_stem()?;
    Some(
        transcript
            .parent()?
            .join(stem)
            .join("subagents")
            .join(format!("agent-{agent_id}.meta.json")),
    )
}

/// Read one sidecar's stop marker, checked against the lease's dispatch.
///
/// Why: the marker releases a slot, so the reader answers `Stopped` only on an
/// explicit `true` for THIS dispatch — the lease's `tool_use_id` must match the
/// sidecar's `toolUseId` when both are known.
/// What: `Ok(Absent)` for a missing file or a marker that is not `true`;
/// `Ok(OtherDispatch)` on a `toolUseId` mismatch; `Err` naming the fault for a
/// file that cannot be read, is over [`MAX_SIDECAR_BYTES`], or is not a JSON
/// object.
/// Test: `a_user_stopped_builder_releases_its_slot_8548`,
/// `an_unreadable_stop_marker_keeps_the_lease_8548`,
/// `a_stop_marker_for_another_dispatch_keeps_the_lease_8548`.
pub(crate) fn read_stop_marker(
    path: &Path,
    tool_use_id: Option<&str>,
) -> Result<StopMarker, String> {
    let file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StopMarker::Absent),
        Err(e) => return Err(format!("open failed: {e}")),
    };
    let mut raw = String::new();
    file.take(MAX_SIDECAR_BYTES + 1)
        .read_to_string(&mut raw)
        .map_err(|e| format!("read failed: {e}"))?;
    if raw.len() as u64 > MAX_SIDECAR_BYTES {
        return Err(format!("over the {MAX_SIDECAR_BYTES}-byte sidecar cap"));
    }
    let value: Value = serde_json::from_str(&raw).map_err(|e| format!("not JSON: {e}"))?;
    let Some(meta) = value.as_object() else {
        return Err("not a JSON object".to_string());
    };
    if meta.get("stoppedByUser").and_then(Value::as_bool) != Some(true) {
        return Ok(StopMarker::Absent);
    }
    let named = meta.get("toolUseId").and_then(Value::as_str);
    if let (Some(lease), Some(named)) = (tool_use_id, named)
        && lease != named
    {
        return Ok(StopMarker::OtherDispatch);
    }
    Ok(StopMarker::Stopped)
}

/// One live builder lease whose sidecar can be probed.
struct Candidate {
    id: crate::core::agent::DelegationId,
    agent_id: String,
    tool_use_id: Option<String>,
    marker: PathBuf,
}

impl DaemonState {
    /// Cancel every live builder lease whose agent the user stopped (#8548).
    ///
    /// Why: see the module doc — a user stop reaches the daemon through no hook,
    /// so without this the slot stays held until the 45-minute TTL.
    /// What: snapshots the live builder records that know their `agent_id` and
    /// transcript, reads each sidecar with no lock held, and for a
    /// [`StopMarker::Stopped`] answer re-checks the record under the
    /// dispatch-record lock: still live, same `agent_id`, same `tool_use_id`.
    /// Only then is it marked [`DelegationStatus::Cancelled`], so a record the
    /// tracker changed while the file was read is never released. Returns how
    /// many leases it released.
    /// Test: `a_user_stopped_builder_releases_its_slot_8548`,
    /// `a_live_builder_keeps_its_slot_8548`,
    /// `an_unreadable_stop_marker_keeps_the_lease_8548`,
    /// `a_stop_marker_for_another_dispatch_keeps_the_lease_8548`.
    pub(crate) fn release_user_stopped_builders(&self) -> usize {
        let candidates: Vec<Candidate> = self
            .delegations
            .iter()
            .filter_map(|entry| {
                let d = entry.value();
                if !d.status.is_live() || !agent_is_builder(&d.agent) {
                    return None;
                }
                let agent_id = d.agent_id.clone()?;
                let marker = stop_marker_path(d.transcript_path.as_deref()?, &agent_id)?;
                Some(Candidate {
                    id: d.id,
                    agent_id,
                    tool_use_id: d.tool_use_id.clone(),
                    marker,
                })
            })
            .collect();
        let mut released = 0;
        for c in candidates {
            match read_stop_marker(&c.marker, c.tool_use_id.as_deref()) {
                Ok(StopMarker::Stopped) => {
                    if self.cancel_if_still_held(&c) {
                        tracing::info!(
                            agent_id = %c.agent_id,
                            "builder slot: the user stopped this agent — releasing its slot (#8548)"
                        );
                        released += 1;
                    }
                }
                Ok(StopMarker::Absent) => {}
                Ok(StopMarker::OtherDispatch) => tracing::warn!(
                    agent_id = %c.agent_id,
                    path = %c.marker.display(),
                    "builder slot: stop marker names another dispatch — lease kept (#8548)"
                ),
                Err(e) => tracing::warn!(
                    agent_id = %c.agent_id,
                    path = %c.marker.display(),
                    "builder slot: stop marker unreadable ({e}) — lease kept (#8548)"
                ),
            }
        }
        released
    }

    /// Cancel `c`'s record if it is still the live lease `c` was read from.
    fn cancel_if_still_held(&self, c: &Candidate) -> bool {
        let _record = self.dispatch_record_guard();
        let mut cancelled = false;
        self.mutate_delegation(c.id, |d| {
            if d.status.is_live()
                && d.agent_id.as_deref() == Some(c.agent_id.as_str())
                && d.tool_use_id == c.tool_use_id
            {
                d.status = DelegationStatus::Cancelled;
                d.ended_at = Some(chrono::Utc::now());
                cancelled = true;
            }
        });
        cancelled
    }
}
