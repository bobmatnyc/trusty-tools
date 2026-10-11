//! What a catch-up reads from the host rather than from its arguments.
//!
//! Why: the public entry points load `~/.trusty-mpm/config.toml`, resolve the
//! live trusty-memory socket and (#9390) run real `gh`/`git`, so a test calling
//! them could reach the operator's daemon (seen as a flake) or the network.
//! Tests build an isolated host instead. Split out of `mcp_context.rs`, which
//! sits at its SLOC budget.
//! What: [`CatchupHost`] and its production constructor, plus
//! [`CatchupHost::verify_resume`], the one call the catch-up makes into the
//! resume verify pass.
//! Test: `isolated_catchup` in `mcp_context`'s tests builds every catch-up on
//! an isolated host; `resume_verify_tests.rs` drives the verify leg.

use std::path::{Path, PathBuf};

use serde_json::Value;

use crate::core::catchup::resolve::ResolvedSnapshot;
use crate::core::config::MpmConfig;
use crate::daemon::resume_verify::{ResumeVerifyHost, resume_verify};

/// The host side of one catch-up.
pub(crate) struct CatchupHost {
    /// The loaded `MpmConfig`.
    pub(crate) config: MpmConfig,
    /// The trusty-memory socket; an unserved path makes the palace leg empty.
    pub(crate) memory_socket: PathBuf,
    /// #9390: the runner, palace and bounds of the resume verify pass.
    pub(crate) resume_verify: ResumeVerifyHost,
}

impl CatchupHost {
    /// The operator's config, the live memory socket, real `gh` and `git`.
    pub(crate) fn resolve() -> Self {
        Self {
            config: MpmConfig::load_default(),
            memory_socket: trusty_common::memory_rpc::resolve_memory_socket_or_unreachable(),
            resume_verify: ResumeVerifyHost::system(),
        }
    }

    /// The `resume_verify` response field for the snapshot a catch-up resolved.
    /// Test: `a_resume_reads_the_live_values_and_reports_the_stale_ones`.
    pub(crate) async fn verify_resume(
        &self,
        project_dir: &Path,
        resolved: Option<&ResolvedSnapshot>,
    ) -> Value {
        let snapshot = resolved.map(|r| r.path.as_path());
        resume_verify(
            &self.resume_verify,
            &self.memory_socket,
            project_dir,
            snapshot,
        )
        .await
    }
}
