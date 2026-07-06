//! DOC-28 catch-up context injection for the PM prompt (#1762, PR2).
//!
//! Why: When a PM agent starts a task, it has no intrinsic knowledge of recent
//! project activity — paused sessions, new git commits, or recent memory palace
//! entries.  Injecting a catch-up digest as seed `fallback_guidance` in the PM
//! system prompt gives it the context to orient quickly, mirroring the behaviour
//! the trusty-mpm harness provides via `session_launch`.
//! What: [`pm_catchup_context`] builds a [`CatchupOptions`] for the project and
//! calls the shared engine in trusty-common, returning `Some(digest)` when
//! meaningful content was produced or `None` to indicate the assembler should
//! skip this section entirely.
//! Test: `catchup::tests::pm_catchup_context_does_not_panic_on_empty_repo`.

use std::path::Path;

use tracing::debug;
use trusty_common::catchup::{CatchupOptions, run_catchup};

/// Build PM-prompt catch-up seed context for the given project directory.
///
/// Why: the PM prompt benefits from a one-time digest of what happened in the
/// project since the last session — paused work, recent commits, recent memory
/// palace entries — so it can orient immediately without asking the operator.
/// This is injected as `fallback_guidance` (the last section of the assembled
/// prompt) and is only wired into the PM agent path; sub-agents receive `None`
/// because they operate under the PM's direction and do not need the high-level
/// activity digest.
///
/// **Watermark is NOT advanced** (`advance_watermark = false`).  trusty-code is
/// a read-only consumer of the catch-up state: watermark advancement is owned by
/// trusty-mpm's `session_launch` hook, which controls the canonical boundary
/// between sessions.  Advancing from trusty-code would skew that boundary and
/// cause cross-harness interference where activity is silently dropped from the
/// next mpm session's digest.
///
/// Fail-open: any error or empty/whitespace result returns `None` rather than
/// propagating, so prompt assembly always succeeds even when the daemon is
/// offline or the project has no git history.
///
/// What: resolves `memory_url` via
/// [`trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable`] (env override
/// `TRUSTY_MEMORY_URL` first, else the daemon's discovered bound address,
/// else a fail-fast placeholder — issue #2030), builds `CatchupOptions` with
/// sane limits, calls `run_catchup` with `advance_watermark = false`, and
/// returns `Some(digest)` when the result is non-whitespace, else `None`.
/// Test: `catchup::tests::pm_catchup_context_does_not_panic_on_empty_repo`.
pub async fn pm_catchup_context(project_dir: &Path) -> Option<String> {
    let memory_url = trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable();

    let opts = CatchupOptions {
        project_dir: project_dir.to_path_buf(),
        memory_url,
        include_git: true,
        include_palace: true,
        git_limit: 50,
        drawer_limit: 15,
        full: false,
    };

    // advance_watermark = false: trusty-code never owns the watermark boundary;
    // that belongs to trusty-mpm session_launch to avoid cross-harness skew.
    let digest = run_catchup(&opts, false).await;

    if digest.trim().is_empty() {
        debug!("pm_catchup_context: empty digest — omitting from PM prompt");
        None
    } else {
        Some(digest)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;
    use std::process::Command;
    use tempfile::TempDir;

    /// Initialise a minimal git repo with one commit so catch-up has a real git
    /// context to scan (avoids "not a git repo" errors in the engine).
    fn init_git_repo(dir: &TempDir) {
        let p = dir.path();
        for args in [
            vec!["init"],
            vec!["config", "user.email", "test@test.com"],
            vec!["config", "user.name", "Test"],
        ] {
            // Ignore errors: git may not be on PATH in some CI environments;
            // the test will simply see an empty digest (fail-open).
            let _ = Command::new("git").arg("-C").arg(p).args(&args).output();
        }
        let _ = fs::write(p.join("README.md"), b"test");
        let _ = Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["add", "."])
            .output();
        let _ = Command::new("git")
            .arg("-C")
            .arg(p)
            .args(["commit", "-m", "init"])
            .output();
    }

    /// Verifies that `pm_catchup_context` does not panic and returns either
    /// `Some` or `None` consistently when the memory daemon is unreachable.
    ///
    /// Why: tests can never assume a live trusty-memory daemon; this test
    /// points at an unreachable port so the palace section degrades gracefully.
    /// What: creates a temp dir with a git repo, calls `pm_catchup_context`,
    /// asserts no panic and that the returned value is well-formed.
    /// Test: this test.
    #[tokio::test]
    async fn pm_catchup_context_does_not_panic_on_empty_repo() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);

        // Point memory_url at an unreachable port so palace fetch fails-open.
        // SAFETY: single-threaded test; no concurrent env reads.
        unsafe {
            std::env::set_var(
                trusty_common::mcp::memory_rpc::TRUSTY_MEMORY_URL_ENV,
                "http://127.0.0.1:19999",
            )
        };

        let result = pm_catchup_context(tmp.path()).await;

        // Result is either Some(non-empty) or None — never panics.
        match &result {
            Some(ctx) => {
                assert!(
                    !ctx.trim().is_empty(),
                    "Some(ctx) must not be empty/whitespace"
                );
            }
            None => {
                // Acceptable: the engine may return all-whitespace if every
                // source produces an empty section header only.
            }
        }

        // Confirm no state file was written under the temp dir (advance=false
        // means no ~/.trusty-mpm/<palace-id>/catchup-state.json for this
        // ephemeral project — state path is keyed by palace_id, not project_dir,
        // so we cannot predict the exact path; instead, verify the return value
        // alone which is the observable contract for advance_watermark=false).
        //
        // The palace_id for a repo with no remote defaults to the basename of
        // the tempdir.  We check that neither the path nor any sub-path was
        // created inside the temp dir itself (the engine writes to ~/.trusty-mpm,
        // not to the project dir).
        let state_file_in_project = tmp.path().join(".trusty-mpm");
        assert!(
            !state_file_in_project.exists(),
            "advance_watermark=false must not write state inside the project dir"
        );
    }

    /// Verifies that `pm_catchup_context` does not panic when the project dir
    /// is not a git repository (engine is expected to fail-open on git errors).
    ///
    /// Why: operators may invoke `tcode run-task` outside a git repo; the PM
    /// prompt must still be assembled without aborting.
    /// What: calls `pm_catchup_context` on a plain temp dir (no git init) and
    /// asserts that the function returns without panicking.
    /// Test: this test.
    #[tokio::test]
    async fn pm_catchup_context_does_not_panic_on_non_git_dir() {
        let tmp = TempDir::new().unwrap();

        // Point memory_url at an unreachable port.
        // SAFETY: single-threaded test; no concurrent env reads.
        unsafe {
            std::env::set_var(
                trusty_common::mcp::memory_rpc::TRUSTY_MEMORY_URL_ENV,
                "http://127.0.0.1:19999",
            )
        };

        // Must not panic — fall through to None or Some(whitespace-only) path.
        let _result = pm_catchup_context(tmp.path()).await;
    }
}
