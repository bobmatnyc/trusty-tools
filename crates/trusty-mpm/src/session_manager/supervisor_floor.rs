//! The kill-by-name floor that keeps the Architect's panes alive (#8942).
//!
//! Why: every daemon path that ends a session reaches the pane by tmux NAME.
//! A stale record that happens to carry the Architect's name (#8935) was
//! enough to kill the live Architect. The two primitives that act on a name —
//! `TmuxDriver::kill_session` and `ManagedTmuxDriver::signal_terminate` — ask
//! this floor first, so no caller can reach a protected pane.
//! What: [`SupervisorFloor`] answers a [`KillVerdict`] for a tmux name from two
//! sources: the Architect launch sidecars (`recorded_session_names`) and the
//! session store's protected-kind records. Anything it cannot read refuses.
//! Test: `supervisor_floor_tests.rs`.

use std::path::{Path, PathBuf};

use crate::core::architect_session::{POLL_SUFFIX, recorded_session_names};

use super::session_kind::SessionKind;
use super::store_integrity;

/// The suffixes of the Architect's helper sessions (`<name>-poll`, …).
const AUX_SUFFIXES: [&str; 2] = [POLL_SUFFIX, super::supervisor_register::COLLECTOR_ROLE];

/// The role string [`SidecarRole`] gives the Architect's own session.
pub const SUPERVISOR_ROLE: &str = "supervisor";

/// What an Architect launch sidecar makes of one tmux name (#8942).
///
/// What: the record kind, and the role [`super::ManagedSessionId::for_supervisor`]
/// keys the stable id on — [`SUPERVISOR_ROLE`] for the Architect, the helper
/// suffix (`-poll`, `-collector`) for a helper.
/// Test: `a_sidecar_names_the_architect_and_its_helpers`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SidecarRole {
    /// `Supervisor` or `SupervisorAux`.
    pub kind: SessionKind,
    /// The id role; see the type doc.
    pub role: &'static str,
}

/// Whether a kill-by-name may proceed (#8942).
///
/// Why: "protected" and "cannot tell" are both refusals, but the log must say
/// which one happened.
/// What: `Permit`, `Protected(why)`, or `Undeterminable(why)`.
/// Test: `supervisor_floor_tests.rs`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KillVerdict {
    /// No source names this session as the Architect's.
    Permit,
    /// A sidecar or a protected-kind record names this session.
    Protected(String),
    /// A source could not be read, so the name cannot be cleared.
    Undeterminable(String),
}

/// Where the floor reads its protected names from.
#[derive(Debug, Clone)]
enum FloorSource {
    /// Sidecars under `root/architect-launch/`, records in `store`.
    Root { root: PathBuf, store: PathBuf },
    /// No absolute home directory: nothing can be cleared.
    NoHome,
    /// Unit-test builds only: [`SupervisorFloor::host`] must not read the
    /// operator's real `~/.trusty-mpm` from a test. Tests of the floor itself
    /// build one with [`SupervisorFloor::at_root`].
    #[cfg(test)]
    Unguarded,
}

/// The protected-name check for one framework root (#8942).
///
/// Why: see the module doc. It is a value, not a global, so a test can point
/// it at a scratch root and a production driver at the host root.
/// What: holds where to read the sidecars and the store; [`Self::verdict`]
/// reads both on every call, so a sidecar written after the daemon started
/// still counts.
/// Test: `supervisor_floor_tests.rs`.
#[derive(Debug, Clone)]
pub struct SupervisorFloor {
    source: FloorSource,
}

impl SupervisorFloor {
    /// The floor over the operator's `~/.trusty-mpm`.
    ///
    /// Why: `tm fleet init` writes the sidecars under the home root, and the
    /// daemon's store lives there too; the pane guard reads the same root.
    /// What: [`Self::from_home`] of `dirs::home_dir()`. In the lib's own
    /// `cfg(test)` build it is unguarded instead, so no unit test reads the
    /// operator's real state. The `tm` bin tests and `tests/*.rs` link the lib
    /// without `cfg(test)`, so there it reads `$HOME`: a test there that
    /// reaches a kill must point `$HOME` at a scratch directory.
    /// Test: `the_host_floor_reads_the_sidecars_under_home` (`tests/`);
    /// `a_missing_or_relative_home_is_undeterminable` covers `from_home`.
    pub fn host() -> Self {
        #[cfg(test)]
        {
            Self {
                source: FloorSource::Unguarded,
            }
        }
        #[cfg(not(test))]
        {
            Self::from_home(dirs::home_dir())
        }
    }

    /// The floor for a home directory, or [`FloorSource::NoHome`] when there
    /// is none or it is relative.
    ///
    /// Why: `FrameworkPaths` falls back to `.` without a home; a floor rooted
    /// at the working directory would clear every name, so no home refuses.
    /// What: `home/.trusty-mpm` when `home` is absolute.
    /// Test: `a_missing_or_relative_home_is_undeterminable`.
    pub fn from_home(home: Option<PathBuf>) -> Self {
        match home.filter(|h| h.is_absolute()) {
            Some(home) => Self::at_root(&home.join(".trusty-mpm")),
            None => Self {
                source: FloorSource::NoHome,
            },
        }
    }

    /// The floor over framework root `root`.
    ///
    /// What: sidecars under `root/architect-launch/`, the store at
    /// `root/session-manager/sessions.json`.
    /// Test: every test in `supervisor_floor_tests.rs`.
    pub fn at_root(root: &Path) -> Self {
        Self {
            source: FloorSource::Root {
                root: root.to_path_buf(),
                store: root.join("session-manager").join("sessions.json"),
            },
        }
    }

    /// The floor a session manager with store directory `data_dir` kills under.
    ///
    /// Why: the manager's teardown gate must read the same sidecars and store
    /// as the host floor in production, and a test manager's scratch
    /// directory, never the operator's home, in a test.
    /// What: the store is `data_dir/sessions.json`. The sidecar root is
    /// `data_dir`'s parent when `data_dir` is named `session-manager` (the
    /// daemon's `<framework root>/session-manager`), else `data_dir` itself.
    /// Test: `for_data_dir_reads_the_framework_root_above_the_store`.
    pub fn for_data_dir(data_dir: &Path) -> Self {
        let root = match data_dir.parent() {
            Some(parent) if data_dir.file_name() == Some("session-manager".as_ref()) => parent,
            _ => data_dir,
        };
        Self {
            source: FloorSource::Root {
                root: root.to_path_buf(),
                store: data_dir.join("sessions.json"),
            },
        }
    }

    /// Every session name an Architect launch sidecar records.
    ///
    /// Why: boot reconcile reads the list once per pass, then asks
    /// [`Self::sidecar_role`] per pane.
    /// What: `recorded_session_names` of the root; `Err` when it fails or
    /// there is no home. An unguarded test floor records nothing.
    /// Test: `a_sidecar_names_the_architect_and_its_helpers`.
    pub fn sidecar_names(&self) -> Result<Vec<String>, String> {
        match &self.source {
            FloorSource::Root { root, .. } => {
                recorded_session_names(root).map_err(|why| format!("architect sidecars: {why}"))
            }
            FloorSource::NoHome => Err("no absolute home directory".into()),
            #[cfg(test)]
            FloorSource::Unguarded => Ok(Vec::new()),
        }
    }

    /// The role `sidecars` give tmux session `name`, if any.
    ///
    /// What: `Supervisor` when a sidecar records `name`; `SupervisorAux` when
    /// `name` is a recorded name plus `-poll` or `-collector`; else `None`.
    /// Test: `a_sidecar_names_the_architect_and_its_helpers`.
    pub fn sidecar_role(sidecars: &[String], name: &str) -> Option<SidecarRole> {
        for recorded in sidecars {
            if name == recorded {
                return Some(SidecarRole {
                    kind: SessionKind::Supervisor,
                    role: SUPERVISOR_ROLE,
                });
            }
            let suffix = name.strip_prefix(recorded.as_str());
            if let Some(aux) = AUX_SUFFIXES.iter().find(|s| suffix == Some(**s)) {
                return Some(SidecarRole {
                    kind: SessionKind::SupervisorAux,
                    role: aux,
                });
            }
        }
        None
    }

    /// Whether tmux session `name` may be killed or signalled.
    ///
    /// Why: #8942, fail closed — "cannot tell" never becomes "not the
    /// Architect".
    /// What: `Protected` when a sidecar records `name`, or `name` is a
    /// sidecar name plus `-poll`/`-collector`, or a non-terminal store record
    /// with a protected kind (`Unknown` included) carries `name`.
    /// `Undeterminable` when the sidecar directory, a sidecar or the store does
    /// not read or parse, or there is no home. A missing directory or store
    /// file is empty, not an error. Otherwise `Permit`.
    /// Test: `a_sidecar_name_is_protected`, `the_architects_helper_sessions_are_protected`,
    /// `a_protected_kind_record_is_protected`,
    /// `a_record_with_an_unknown_kind_is_never_torn_down`,
    /// `kill_by_name_fails_closed_when_architect_sidecars_cannot_be_read`,
    /// `an_unreadable_store_is_undeterminable`, `an_unrelated_name_is_permitted`.
    pub fn verdict(&self, name: &str) -> KillVerdict {
        let store = match &self.source {
            FloorSource::Root { store, .. } => store,
            FloorSource::NoHome => {
                return KillVerdict::Undeterminable("no absolute home directory".into());
            }
            #[cfg(test)]
            FloorSource::Unguarded => return KillVerdict::Permit,
        };
        let sidecars = match self.sidecar_names() {
            Ok(names) => names,
            Err(why) => return KillVerdict::Undeterminable(why),
        };
        if let Some(role) = Self::sidecar_role(&sidecars, name) {
            return KillVerdict::Protected(format!("an architect sidecar makes `{name}` {role:?}"));
        }
        match protected_record(store, name) {
            Ok(Some(why)) => KillVerdict::Protected(why),
            Ok(None) => KillVerdict::Permit,
            Err(why) => KillVerdict::Undeterminable(format!("session store: {why}")),
        }
    }

    /// The refusal for killing `name`, logged; `None` when the kill may run.
    ///
    /// Why: both primitives need the same verdict-plus-warning in one line.
    /// What: [`Self::verdict`]; on anything but `Permit`, a `warn!` naming
    /// `caller`, the session and the reason, and `Some(reason)`.
    /// Test: `kill_by_name_fails_closed_when_architect_sidecars_cannot_be_read`.
    pub fn refuse(&self, name: &str, caller: &str) -> Option<String> {
        let why = match self.verdict(name) {
            KillVerdict::Permit => return None,
            KillVerdict::Protected(why) => format!("protected Architect session: {why}"),
            KillVerdict::Undeterminable(why) => format!("cannot rule out the Architect: {why}"),
        };
        tracing::warn!(
            caller,
            session = name,
            "#8942: refused to kill or signal — {why}"
        );
        Some(format!("#8942: {caller} refused `{name}`: {why}"))
    }
}

/// The reason a live protected-kind record in `store` carries `name`, if one
/// does. A missing store is `Ok(None)`; an unreadable or unparseable one is
/// `Err`.
fn protected_record(store: &Path, name: &str) -> Result<Option<String>, String> {
    let raw = match std::fs::read_to_string(store) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(format!("{}: {e}", store.display())),
        Ok(raw) => raw,
    };
    let data = store_integrity::validate(store, &raw).map_err(|e| e.diagnostic())?;
    Ok(data
        .sessions
        .values()
        .find(|r| r.tmux_name == name && r.kind.is_protected() && !r.state.is_terminal())
        .map(|r| format!("record {} has kind {:?}", r.id, r.kind)))
}

#[cfg(test)]
#[path = "supervisor_floor_tests.rs"]
mod tests;
