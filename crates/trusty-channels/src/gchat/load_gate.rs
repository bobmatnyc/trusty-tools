//! The routes-file load gate (#9448 E1): `routes.toml` takes effect only when
//! the bytes parsed are exactly the bytes committed at `HEAD`.
//!
//! Why: gchat keeps its HEAD-only gate until S3 applies Db1 to gchat-mcp
//! (#8454 G1). The git logic moved to [`crate::policy::gate`] so the policy
//! loader shares its hardening.
//! What: [`check_committed`] runs the HEAD-only gate and reports a refusal
//! as [`RouteError::Gate`].
//! Test: `load_gate_refuses_untracked_modified_and_staged`,
//! `load_gate_refuses_edits_hidden_by_assume_unchanged_or_skip_worktree`,
//! `load_gate_checks_the_bytes_read_not_the_file_after`.

use std::path::Path;

use crate::gchat::error::RouteError;
use crate::gchat::routes::routes_path;
use crate::policy::gate::check_committed_at_head;

/// Refuse unless `bytes` equal the routes file committed at `HEAD`.
///
/// Why: see the module doc. The caller passes the bytes it will parse, so
/// the check and the parse see the same content (#9448 review).
/// What: any gate refusal becomes [`RouteError::Gate`] naming the path,
/// with the gate's reason.
/// Test: `load_gate_refuses_untracked_modified_and_staged`,
/// `load_gate_checks_the_bytes_read_not_the_file_after`.
pub fn check_committed(project_dir: &Path, bytes: &[u8]) -> Result<(), RouteError> {
    let path = routes_path(project_dir);
    check_committed_at_head(&path, bytes).map_err(|reason| RouteError::Gate { path, reason })
}
