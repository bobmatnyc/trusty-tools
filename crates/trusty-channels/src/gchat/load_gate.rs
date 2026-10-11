//! The routes-file load gate (#9448 E1, #8454 Db1): `routes.toml` takes
//! effect only when the bytes parsed are the bytes the default branch's
//! commit holds, and `HEAD` is on that branch.
//!
//! Why: #8454 G1: Db1 applies to gchat-mcp too. gchat keeps its own loader
//! (S3 Q1) but shares the policy loader's gate, [`crate::policy::gate`].
//! What: [`check_committed`] runs [`check_default_branch`] and reports a
//! refusal as [`RouteError::Gate`] with the gate's typed reason.
//! Test: `feature_branch_refuses_load_and_every_send`,
//! `detached_head_at_the_default_tip_refuses_load_and_every_send`,
//! `untracked_and_dirty_arms_refuse_load_and_every_send`,
//! `load_gate_checks_the_bytes_read_not_the_file_after`.

use std::path::Path;

use crate::gchat::error::RouteError;
use crate::gchat::routes::routes_path;
use crate::policy::check_default_branch;

/// Refuse unless `project_dir` is its repo's top level, `HEAD` is on the
/// default branch, and `bytes` equal the routes file at that branch's commit.
///
/// Why: see the module doc. The caller passes the bytes it will parse, so
/// the check and the parse see the same content (#9448 review).
/// What: the default branch is `origin/HEAD`'s target, else exactly one of
/// `main` and `master` (G2); a detached `HEAD` is refused even at the
/// default tip (G3). Any refusal becomes [`RouteError::Gate`] naming the
/// routes-file path.
/// Test: `feature_branch_refuses_load_and_every_send`,
/// `both_main_and_master_without_origin_head_refuse_load_and_every_send`,
/// `gchat_gate_applies_db1`.
pub fn check_committed(project_dir: &Path, bytes: &[u8]) -> Result<(), RouteError> {
    // #8454 S3a: Db1 replaces the HEAD-only check.
    check_default_branch(project_dir, bytes)
        .map(drop)
        .map_err(|reason| RouteError::Gate {
            path: routes_path(project_dir),
            reason,
        })
}
