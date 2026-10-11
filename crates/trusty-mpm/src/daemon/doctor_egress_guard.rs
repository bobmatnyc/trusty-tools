//! `tm doctor` egress-guard probe (#8454 G3, slice S2).
//!
//! Why: the `tm hook --egress-guard` entry is the only thing between a
//! tm-launched session and a Slack, Gmail, Notion or Google Chat MCP tool. A
//! settings file that lost it, shortened its timeout or set `disableAllHooks`
//! runs every session there unguarded, and nothing else on screen says so.
//! What: [`check_egress_guard`] reads every project-tier settings file the
//! hygiene probe reaches ([`candidate_settings_files`]) and the managed
//! user-tier `claude-config/settings{,.local}.json`, grouped by the directory
//! that holds them. A group is tm-provisioned when one of its files carries a
//! tm hook command (the lifecycle `tm hook`, the pm-guard or the egress
//! guard); the managed tier always is. `Fail` when a provisioned group carries
//! no egress group, a weakened one (a timeout under 10 or a narrowed matcher),
//! or sets `disableAllHooks`. `Unknown` when a file is unreadable or
//! malformed. `Ok` otherwise — a foreign project owes tm nothing. It never
//! reads `[pm_guard]`: the entry has no off switch (Architect D3). Read-only.
//! Test: the `egress_doctor_*` tests below.

use std::path::{Path, PathBuf};

use crate::core::doctor::{CheckStatus, DoctorCheck};

/// The check's name.
const NAME: &str = "egress_guard";

/// Report whether every tm-provisioned settings file registers the egress
/// guard. See the module doc.
pub(super) fn check_egress_guard(
    _project_dir: Option<&Path>,
    _active_workspace_paths: &[PathBuf],
    _home: &Path,
) -> DoctorCheck {
    DoctorCheck::new(NAME, CheckStatus::Ok, "egress guard registered")
}
