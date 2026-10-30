//! The Architect launch record for a managed launch of the supervisor session
//! (#9092).
//!
//! Why: the trust floor binds the Architect to a per-PID record
//! (`architect-launch/<pid>.architect`), and only `tm fleet init|relaunch`
//! wrote one. A `claude` that any other tm path started for the supervisor
//! session ran UNBOUND, and every Architect exemption was silently denied.
//! Every managed launch passes through `tm internal-spawn-disclaimed
//! --launch-spec`, which spawns the `claude` and so is the one process that
//! knows its PID. That shim asks [`binding_for`] whether the launch is the
//! supervisor session's, and records the PID it spawned.
//! What: [`binding_for`] reads the session store under the `~/.trusty-mpm`
//! root and answers `Some` for a record of kind `supervisor` whose workspace
//! is the launch's cwd. [`SpawnBinding::preflight`] proves the record
//! directory takes a write before anything is spawned, and
//! [`SpawnBinding::record`] writes the record and its session sidecar for the
//! spawned PID through [`architect_session::record_launch`], then reads the
//! name back. Every failure is `Err`; the shim refuses or stops the launch on
//! it and never lets the Architect run unbound.
//! Test: `architect_spawn_tests.rs`; the shim's
//! `a_supervisor_launch_records_the_claude_it_spawned_9092`.

use std::path::{Path, PathBuf};

use crate::core::architect_launch::ARCHITECT_DIR;
use crate::core::architect_session::{self, architect_session_name, validate_session_name};
use crate::core::twin_identity::{ArmingRecord, same_dir};

use super::session_kind::SessionKind;
use super::store_integrity;

/// The record a launch of the supervisor session must write.
///
/// Why: the shim decides before the spawn and writes after it; this carries
/// the decision across the spawn.
/// What: the `~/.trusty-mpm` root, the supervisor's tmux session name and the
/// project directory the `claude` runs in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpawnBinding {
    root: PathBuf,
    session: String,
    project_dir: PathBuf,
}

/// The binding a launch of managed session `session_id` in `cwd` needs.
///
/// Why: see the module doc. The spec names the session; only the store says
/// whether it is the supervisor's.
/// What: `Ok(None)` for an empty `session_id` (a CLI launch: `tm fleet`
/// records its own), a missing store, a session the store does not hold, and
/// any kind but `supervisor` other than `Unknown`. `Ok(Some)` for a
/// `supervisor` record whose workspace (`workspace_path`, else `cwd`) is
/// `cwd`. `Err`, so the launch is refused, when the store cannot be read or
/// parsed, the kind is `Unknown`, the workspace differs, or the tmux name
/// fails [`validate_session_name`].
/// Test: `a_supervisor_record_needs_a_binding`,
/// `an_ordinary_record_or_a_cli_launch_needs_none`,
/// `an_unreadable_store_or_unknown_kind_refuses`,
/// `a_supervisor_launch_in_another_directory_refuses`.
pub fn binding_for(
    root: &Path,
    session_id: &str,
    cwd: &Path,
) -> Result<Option<SpawnBinding>, String> {
    if session_id.trim().is_empty() {
        return Ok(None);
    }
    let store = root.join("session-manager").join("sessions.json");
    let raw = match std::fs::read_to_string(&store) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", store.display())),
        Ok(raw) => raw,
    };
    let data = store_integrity::validate(&store, &raw).map_err(|e| e.diagnostic())?;
    let Some(record) = data.sessions.get(session_id) else {
        return Ok(None);
    };
    match record.kind {
        SessionKind::Architect => {}
        SessionKind::Unknown => {
            return Err(format!(
                "session {session_id} has a kind this tm does not recognise, so it cannot rule \
                 out the Architect"
            ));
        }
        SessionKind::Ordinary | SessionKind::ArchitectAux => return Ok(None),
    }
    let workspace = record.workspace_path.as_ref().unwrap_or(&record.cwd);
    if !same_dir(cwd, workspace) {
        return Err(format!(
            "the launch runs in {}, not in supervisor session {}'s workspace {}",
            cwd.display(),
            record.tmux_name,
            workspace.display()
        ));
    }
    validate_session_name(&record.tmux_name)?;
    Ok(Some(SpawnBinding {
        root: root.to_path_buf(),
        session: record.tmux_name.clone(),
        project_dir: cwd.to_path_buf(),
    }))
}

impl SpawnBinding {
    /// The supervisor's tmux session name.
    pub fn session(&self) -> &str {
        &self.session
    }

    /// Prove the record directory takes a write, before any `claude` starts.
    ///
    /// Why: a record write that can only fail after the spawn would leave an
    /// unbound `claude` to stop; most failures (a read-only or missing
    /// directory) are visible before it.
    /// What: creates `root/architect-launch/` and a temporary file in it,
    /// which is removed on drop. Either failure is `Err` naming the path.
    /// Test: `an_unwritable_record_dir_fails_the_preflight`.
    pub fn preflight(&self) -> Result<(), String> {
        let dir = self.root.join(ARCHITECT_DIR);
        std::fs::create_dir_all(&dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        tempfile::NamedTempFile::new_in(&dir)
            .map(drop)
            .map_err(|e| format!("{}: {e}", dir.display()))
    }

    /// Record the `claude` at `pid` as the Architect.
    ///
    /// Why: the same write `tm fleet init|relaunch` makes, so the hook and
    /// `tm fleet status` bind this `claude` as they bind a fleet launch.
    /// What: [`architect_session::record_launch`] (the sidecar, then the
    /// mode-0600 record with `pid`'s start time and the project), then
    /// [`architect_session_name`] must read back this session. Any failure is
    /// `Err` naming the record path.
    /// Test: `a_supervisor_record_needs_a_binding`.
    pub fn record(&self, pid: u32) -> Result<ArmingRecord, String> {
        let path = crate::core::architect_launch::ARCHITECT_RECORDS.path(&self.root, pid);
        let fail = |why: String| {
            format!(
                "cannot write the Architect launch record {} for claude pid {pid}: {why}",
                path.display()
            )
        };
        let record =
            architect_session::record_launch(&self.root, pid, &self.project_dir, &self.session)
                .map_err(fail)?;
        match architect_session_name(&self.root, pid) {
            Ok(name) if name == self.session => Ok(record),
            Ok(name) => Err(fail(format!("it reads back session {name}"))),
            Err(why) => Err(fail(why.to_string())),
        }
    }
}

#[cfg(test)]
#[path = "architect_spawn_tests.rs"]
mod tests;
