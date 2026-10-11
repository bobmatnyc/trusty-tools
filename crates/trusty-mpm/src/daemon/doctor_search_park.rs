//! `tm doctor --fix` parks the duplicates `search_project_duplicate_index`
//! flags (#9664). WIP stub: the red-first commit.

use std::path::Path;

use crate::core::doctor_repair::{RepairMode, RepairStep};

use super::doctor_search_project::FanoutLimits;

/// Stub.
pub fn repair_duplicate_indexes(_project_dir: &Path, _mode: RepairMode) -> Vec<RepairStep> {
    Vec::new()
}

/// Stub.
pub(super) async fn repair_duplicate_indexes_at(
    _socket: &Path,
    _project_dir: &Path,
    _limits: FanoutLimits,
    _mode: RepairMode,
) -> Vec<RepairStep> {
    Vec::new()
}

#[cfg(test)]
#[path = "doctor_search_park_tests.rs"]
mod tests;
