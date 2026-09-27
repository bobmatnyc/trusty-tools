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
//! What: [`DaemonState::reconcile_builder_stop_markers`] reads that sidecar for
//! every live builder lease and cancels the record whose sidecar says the user
//! stopped it. Two callers bound the release: the 60 s delegation sweep, and the
//! builder-slot claim route, which runs it before it counts holders.
//!
//! # A stopped agent can come back
//!
//! The user, or the PM through `SendMessage`, can resume a stopped agent, and it
//! goes on building in the slot directory it was given. So a stop frees the
//! slot's CAPACITY at once but not its INDEX: the release tags the record with
//! [`StopRelease`], and [`stop_quarantine_holds`] keeps that index out of the
//! next assignment until the lease TTL. A `TaskStop` release carries the same
//! tag. Evidence of a resume clears the tag and re-arms the record to
//! `Running`: the sidecar flipping to `stoppedByUser: false`, or a tool call the
//! subagent itself makes ([`DaemonState::rearm_on_agent_activity`]).
//!
//! # Fail direction
//!
//! Closed. An absent sidecar, one with no marker, one naming a different
//! dispatch, and one that cannot be read or parsed all keep the lease; the last
//! is logged at `warn` with the path. The TTL still bounds every lease this
//! module cannot end.
//! Test: `builder_slot_release_tests`.

use std::io::Read;
use std::os::unix::fs::OpenOptionsExt;
use std::path::{Component, Path, PathBuf};

use chrono::{DateTime, Utc};
use serde_json::Value;

use crate::core::agent::{Delegation, DelegationId, DelegationStatus, StopRelease};
use crate::core::dispatch_isolation::agent_is_builder;

use super::builder_slots::BUILDER_LEASE_TTL_SECS;
use super::core::DaemonState;

/// The largest sidecar this module reads, in bytes.
///
/// Why: the sidecar holds a dozen short fields; a file far past that is not one
/// Claude Code wrote, and reading it whole on the claim path would cost time.
const MAX_SIDECAR_BYTES: u64 = 64 * 1024;

/// How old a stop must be before the stopped agent's own tool call counts as a
/// resume, in seconds (#8548).
///
/// Why: the agent's last `PreToolUse` hook runs in its own process, and its
/// POST (a 2 s budget) can land just after the stop released the lease. That
/// call predates the stop, so it is not evidence of a resume.
const REARM_GRACE_SECS: i64 = 10;

/// What one agent's sidecar says about a user stop.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum StopMarker {
    /// The user stopped this dispatch's agent, and has not resumed it.
    Stopped,
    /// The sidecar says `stoppedByUser: false`: the user resumed the agent.
    Resumed,
    /// No sidecar, or a sidecar without a boolean marker.
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
/// What: `Ok(Absent)` for a missing file or a marker that is not a boolean;
/// `Ok(OtherDispatch)` on a `toolUseId` mismatch; `Ok(Resumed)` for `false`;
/// `Err` naming the fault for a file that is not a regular file, cannot be
/// read, is over [`MAX_SIDECAR_BYTES`], or is not a JSON object. #8548: the
/// open is `O_NONBLOCK` and the opened handle must be a regular file, so a FIFO
/// planted at the path cannot hang the claim route.
/// Test: `a_user_stopped_builder_releases_its_slot_8548`,
/// `a_fifo_sidecar_is_refused_without_blocking_8548`,
/// `each_unreadable_sidecar_keeps_the_lease_8548`,
/// `a_stop_marker_for_another_dispatch_keeps_the_lease_8548`.
pub(crate) fn read_stop_marker(
    path: &Path,
    tool_use_id: Option<&str>,
) -> Result<StopMarker, String> {
    let file = match std::fs::OpenOptions::new()
        .read(true)
        .custom_flags(libc::O_NONBLOCK)
        .open(path)
    {
        Ok(file) => file,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(StopMarker::Absent),
        Err(e) => return Err(format!("open failed: {e}")),
    };
    let kind = file.metadata().map_err(|e| format!("stat failed: {e}"))?;
    if !kind.is_file() {
        return Err("not a regular file".to_string());
    }
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
    let Some(stopped) = meta.get("stoppedByUser").and_then(Value::as_bool) else {
        return Ok(StopMarker::Absent);
    };
    let named = meta.get("toolUseId").and_then(Value::as_str);
    if let (Some(lease), Some(named)) = (tool_use_id, named)
        && lease != named
    {
        return Ok(StopMarker::OtherDispatch);
    }
    if !stopped {
        return Ok(StopMarker::Resumed);
    }
    if named.is_none() {
        // #8548 critic: say when the match rests on the agent id alone.
        tracing::debug!(
            path = %path.display(),
            "builder slot: stop marker carries no toolUseId — matched on the agent id alone (#8548)"
        );
    }
    Ok(StopMarker::Stopped)
}

/// Does this stop-released builder record still keep its slot index taken?
///
/// Why: see the module doc — the agent may be resumed into the same directory.
/// What: `true` for a [`DelegationStatus::Cancelled`] record carrying a
/// [`StopRelease`] tag, whose owner is not confirmed dead, inside
/// [`BUILDER_LEASE_TTL_SECS`] of its start — the clock a live lease ends on.
/// Test: `a_quarantined_index_is_reusable_after_the_ttl_8548`.
pub(crate) fn stop_quarantine_holds(
    d: &Delegation,
    owner_alive: Option<bool>,
    now: DateTime<Utc>,
) -> bool {
    if d.stop_release.is_none()
        || d.status != DelegationStatus::Cancelled
        || owner_alive == Some(false)
    {
        return false;
    }
    let started = d.started_at.unwrap_or(d.created_at);
    (now - started).num_seconds() < BUILDER_LEASE_TTL_SECS
}

/// One builder record whose sidecar can be probed.
pub(super) struct Candidate {
    pub(super) id: DelegationId,
    pub(super) agent_id: String,
    pub(super) tool_use_id: Option<String>,
    pub(super) marker: PathBuf,
    /// A user stop already released this record; only a resume acts on it.
    pub(super) released: bool,
}

impl DaemonState {
    /// Release every builder lease the user stopped, and re-arm every one the
    /// user resumed (#8548).
    ///
    /// Why: see the module doc — neither a user stop nor a user resume reaches
    /// the daemon through a hook.
    /// What: snapshots the builder records that know their `agent_id` and
    /// transcript and are either live or [`stop_quarantine_holds`] under a
    /// [`StopRelease::UserStop`] tag, reads each sidecar with no lock held, and
    /// then, under the dispatch-record lock, cancels a live record whose marker
    /// is [`StopMarker::Stopped`] or re-arms a released one whose marker is
    /// [`StopMarker::Resumed`]. Each write re-checks that the record is still
    /// the one read: same state, same `agent_id`, same `tool_use_id`.
    /// Test: `a_user_stopped_builder_releases_its_slot_8548`,
    /// `a_resume_marker_rearms_the_released_lease_8548`,
    /// `a_live_builder_keeps_its_slot_8548`,
    /// `a_stop_marker_for_another_dispatch_keeps_the_lease_8548`.
    pub(crate) fn reconcile_builder_stop_markers(&self) {
        let now = Utc::now();
        let candidates: Vec<Candidate> = self
            .delegations
            .iter()
            .filter_map(|entry| {
                let d = entry.value();
                let released = d.stop_release == Some(StopRelease::UserStop)
                    && stop_quarantine_holds(d, None, now);
                if !(d.status.is_live() || released) || !agent_is_builder(&d.agent) {
                    return None;
                }
                let agent_id = d.agent_id.clone()?;
                let marker = stop_marker_path(d.transcript_path.as_deref()?, &agent_id)?;
                Some(Candidate {
                    id: d.id,
                    agent_id,
                    tool_use_id: d.tool_use_id.clone(),
                    marker,
                    released,
                })
            })
            .collect();
        for c in candidates {
            match (
                read_stop_marker(&c.marker, c.tool_use_id.as_deref()),
                c.released,
            ) {
                (Ok(StopMarker::Stopped), false) => {
                    if self.cancel_if_still_held(&c) {
                        tracing::info!(
                            agent_id = %c.agent_id,
                            "builder slot: the user stopped this agent — releasing its slot (#8548)"
                        );
                    }
                }
                (Ok(StopMarker::Resumed), true) => {
                    let rearmed = self.rearm_if(c.id, |d| {
                        d.stop_release == Some(StopRelease::UserStop)
                            && d.agent_id.as_deref() == Some(c.agent_id.as_str())
                            && d.tool_use_id == c.tool_use_id
                    });
                    if rearmed {
                        tracing::info!(
                            agent_id = %c.agent_id,
                            "builder slot: the user resumed this agent — its lease is held again (#8548)"
                        );
                    }
                }
                (Ok(StopMarker::OtherDispatch), _) => tracing::warn!(
                    agent_id = %c.agent_id,
                    path = %c.marker.display(),
                    "builder slot: stop marker names another dispatch — lease kept (#8548)"
                ),
                (Err(e), _) => tracing::warn!(
                    agent_id = %c.agent_id,
                    path = %c.marker.display(),
                    "builder slot: stop marker unreadable ({e}) — lease kept (#8548)"
                ),
                _ => {}
            }
        }
    }

    /// Cancel `c`'s record if it is still the live lease `c` was read from,
    /// tagging it [`StopRelease::UserStop`].
    ///
    /// Test: `each_unreadable_sidecar_keeps_the_lease_8548`.
    pub(super) fn cancel_if_still_held(&self, c: &Candidate) -> bool {
        let _record = self.dispatch_record_guard();
        let mut cancelled = false;
        self.mutate_delegation(c.id, |d| {
            if d.status.is_live()
                && d.agent_id.as_deref() == Some(c.agent_id.as_str())
                && d.tool_use_id == c.tool_use_id
            {
                d.status = DelegationStatus::Cancelled;
                d.ended_at = Some(Utc::now());
                d.stop_release = Some(StopRelease::UserStop);
                cancelled = true;
            }
        });
        cancelled
    }

    /// Cancel the record a `TaskStop` ended; a builder's is tagged
    /// [`StopRelease::TaskStop`] so its slot index is not reissued (#8548).
    ///
    /// Test: `a_resumed_task_stopped_builder_keeps_its_slot_index_8548`.
    pub(crate) fn cancel_task_stopped(&self, id: DelegationId) -> bool {
        self.mutate_delegation(id, |d| {
            d.status = DelegationStatus::Cancelled;
            d.ended_at = Some(Utc::now());
            if agent_is_builder(&d.agent) {
                d.stop_release = Some(StopRelease::TaskStop);
            }
        })
    }

    /// A tool call the subagent itself made re-arms its stop-released lease
    /// (#8548).
    ///
    /// Why: a stopped agent makes no tool call, so one that does was resumed —
    /// by the user or by the PM's `SendMessage` — and is building again.
    /// What: re-arms record `id` to `Running` when it carries a
    /// [`StopRelease`] tag and the stop is at least [`REARM_GRACE_SECS`] old.
    /// The untagged case returns before any lock, keeping the hook path cheap.
    /// Test: `a_resumed_task_stopped_builder_keeps_its_slot_index_8548`.
    pub(crate) fn rearm_on_agent_activity(&self, id: DelegationId) -> bool {
        let tagged = self
            .delegations
            .get(&id.0)
            .is_some_and(|d| d.stop_release.is_some());
        if !tagged {
            return false;
        }
        let now = Utc::now();
        let rearmed = self.rearm_if(id, |d| {
            d.ended_at
                .is_none_or(|t| (now - t).num_seconds() >= REARM_GRACE_SECS)
        });
        if rearmed {
            tracing::info!(
                delegation = ?id,
                "builder slot: a stopped agent made a tool call — its lease is held again (#8548)"
            );
        }
        rearmed
    }

    /// Re-arm a stop-released record to `Running` when `resumed` accepts it.
    fn rearm_if(&self, id: DelegationId, resumed: impl FnOnce(&Delegation) -> bool) -> bool {
        let _record = self.dispatch_record_guard();
        let mut rearmed = false;
        self.mutate_delegation(id, |d| {
            if d.status == DelegationStatus::Cancelled && d.stop_release.is_some() && resumed(d) {
                d.status = DelegationStatus::Running;
                d.ended_at = None;
                d.stop_release = None;
                rearmed = true;
            }
        });
        rearmed
    }
}
