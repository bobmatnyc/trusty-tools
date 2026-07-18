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
/// else a fail-fast placeholder — issue #2030), then delegates to
/// [`pm_catchup_context_with_memory_url`].
/// Test: `catchup::tests::pm_catchup_context_does_not_panic_on_empty_repo`.
pub async fn pm_catchup_context(project_dir: &Path) -> Option<String> {
    let memory_url = trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable();
    pm_catchup_context_with_memory_url(project_dir, memory_url).await
}

/// Same as [`pm_catchup_context`], but with the trusty-memory base URL passed
/// in explicitly rather than resolved from the process-wide `TRUSTY_MEMORY_URL`
/// env var (issue #3003).
///
/// Why: [`pm_catchup_context`]'s tests need a guaranteed-unreachable dial
/// target so the fail-open path is exercised deterministically. The previous
/// approach mutated the process-global `TRUSTY_MEMORY_URL` env var via
/// `unsafe { std::env::set_var(..) }` with no cleanup and no cross-test lock —
/// `cargo test`'s default parallelism runs every test in this crate's lib
/// binary as threads of ONE process, so that leaked env value was observable
/// by any concurrently-running test whose code path also calls
/// [`trusty_common::mcp::memory_rpc::resolve_memory_base_url_or_unreachable`]
/// (e.g. `run_task::tests::*`, via `execute_run_task` -> `pm_catchup_context`),
/// producing false-red flakes unrelated to the leaking test itself. Threading
/// `memory_url` through as a parameter — the same pattern
/// [`crate::session::memory_sink::TurnMemorySink::with_capacity`] already uses
/// for the identical reason — removes the shared mutable global entirely, so
/// tests need no lock and no cleanup.
/// What: builds `CatchupOptions` with `memory_url` and sane limits, calls
/// `run_catchup` with `advance_watermark = false`, and returns `Some(digest)`
/// when the result is non-whitespace, else `None`.
/// Test: `catchup::tests::pm_catchup_context_does_not_panic_on_empty_repo`,
/// `catchup::tests::pm_catchup_context_does_not_panic_on_non_git_dir`.
async fn pm_catchup_context_with_memory_url(
    project_dir: &Path,
    memory_url: String,
) -> Option<String> {
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

    /// A guaranteed-never-listening loopback address (port 1 is a reserved,
    /// unassigned TCP port) so a connection attempt fails fast rather than
    /// hanging or timing out — the same convention
    /// `trusty_common::mcp::memory_rpc`'s own `UNREACHABLE_PLACEHOLDER` uses.
    ///
    /// Why (#3003): passed directly to
    /// [`pm_catchup_context_with_memory_url`] instead of mutating the
    /// process-global `TRUSTY_MEMORY_URL` env var, so this test needs no lock
    /// and cannot leak state into concurrently-running tests (e.g.
    /// `run_task::tests::*`, which also resolves a trusty-memory URL via
    /// `pm_catchup_context` inside `execute_run_task`).
    const UNREACHABLE_MEMORY_URL: &str = "http://127.0.0.1:1";

    /// Verifies that `pm_catchup_context` does not panic and returns either
    /// `Some` or `None` consistently when the memory daemon is unreachable.
    ///
    /// Why: tests can never assume a live trusty-memory daemon; this test
    /// points at an unreachable port so the palace section degrades gracefully.
    /// What: creates a temp dir with a git repo, calls
    /// `pm_catchup_context_with_memory_url` with [`UNREACHABLE_MEMORY_URL`],
    /// asserts no panic and that the returned value is well-formed.
    /// Test: this test.
    #[tokio::test]
    async fn pm_catchup_context_does_not_panic_on_empty_repo() {
        let tmp = TempDir::new().unwrap();
        init_git_repo(&tmp);

        let result =
            pm_catchup_context_with_memory_url(tmp.path(), UNREACHABLE_MEMORY_URL.to_string())
                .await;

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
    /// What: calls `pm_catchup_context_with_memory_url` with
    /// [`UNREACHABLE_MEMORY_URL`] on a plain temp dir (no git init) and
    /// asserts that the function returns without panicking.
    /// Test: this test.
    #[tokio::test]
    async fn pm_catchup_context_does_not_panic_on_non_git_dir() {
        let tmp = TempDir::new().unwrap();

        // Must not panic — fall through to None or Some(whitespace-only) path.
        let _result =
            pm_catchup_context_with_memory_url(tmp.path(), UNREACHABLE_MEMORY_URL.to_string())
                .await;
    }
}
