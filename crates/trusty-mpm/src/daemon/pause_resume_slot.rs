//! The `ws:<session>/resume` Tier C slot a pause writes (#9390).
//!
//! Red-test stub: the writer lands in the next commit.

use std::path::{Path, PathBuf};

use trusty_common::catchup::pause::{PauseSnapshotInput, PauseSnapshotOutcome};

/// Where the resume slot goes: the memory socket and an optional palace.
#[derive(Debug, Clone)]
pub(crate) struct ResumeSlotHost {
    /// The trusty-memory socket to dial.
    pub(crate) socket: PathBuf,
    /// A palace named explicitly; `None` resolves it from the project.
    pub(crate) palace: Option<String>,
}

impl ResumeSlotHost {
    /// The daemon's own socket, palace resolved from the project.
    pub(crate) fn resolve() -> Self {
        Self {
            socket: trusty_common::memory_rpc::resolve_memory_socket_or_unreachable(),
            palace: None,
        }
    }
}

/// Write the resume slot for one pause. Stub: writes nothing.
pub(crate) async fn record_resume_slot(
    _host: &ResumeSlotHost,
    _project_path: &Path,
    _input: &PauseSnapshotInput<'_>,
    _outcome: &PauseSnapshotOutcome,
) -> String {
    "skipped:unimplemented".to_string()
}

#[cfg(test)]
#[path = "pause_resume_slot_tests.rs"]
mod tests;
