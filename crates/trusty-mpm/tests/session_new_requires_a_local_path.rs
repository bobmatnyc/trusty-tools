//! ADR-0055 (#6000): `session_new` refuses a `repo_url` that is not an existing
//! local directory, at every entry point.
//!
//! Why: #6000 removed trusty-mpm's clone-and-worktree provisioner, so a remote
//! URL has nowhere to route. It must fail loudly and name the remedy rather
//! than degrade into a different placement — and it must fail the same way
//! whichever surface asked. These tests drive the two daemon-side entry points
//! (the spawn RPC the HTTP route calls, and the `session_new` MCP tool); the
//! `tm session new` CLI's own refusal is covered by
//! `client::executor::tests::execute_managed_new_refuses_a_remote_repo_url`.
//!
//! What: each test spawns against a remote URL and asserts the returned error
//! names ADR-0055 and leaves no session record behind. Pre-#6000 the same call
//! reached the provisioner and failed (if at all) with "workspace provisioning
//! failed", so `is_err()` alone would prove nothing — the assertion is on WHICH
//! failure came back.
//! Test: this file IS the test.

use serial_test::serial;
use tempfile::TempDir;

use trusty_mpm::daemon::managed_routes::{SpawnParams, spawn_managed};
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::session_manager::ManagedSessionId;

/// Env vars these tests pin so nothing resolves against the operator's machine.
const REPOS_ROOT_ENV: &str = "TRUSTY_MPM_REPOS_ROOT";
const WORKSPACE_ROOT_ENV: &str = "TRUSTY_MPM_WORKSPACE_ROOT";

/// RAII guard restoring a set of env vars on drop, including on panic.
struct EnvGuard(Vec<(&'static str, Option<std::ffi::OsString>)>);

impl EnvGuard {
    fn set(pairs: &[(&'static str, &std::path::Path)]) -> Self {
        let mut prev = Vec::new();
        for (key, value) in pairs {
            prev.push((*key, std::env::var_os(key)));
            // SAFETY: every caller is `#[serial]`, so only one thread mutates
            // the process environment at a time.
            unsafe { std::env::set_var(key, value) };
        }
        Self(prev)
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        for (key, value) in self.0.drain(..) {
            // SAFETY: see `set`.
            unsafe {
                match value {
                    Some(v) => std::env::set_var(key, v),
                    None => std::env::remove_var(key),
                }
            }
        }
    }
}

fn params(repo_url: &str) -> SpawnParams {
    SpawnParams {
        repo_url: repo_url.to_string(),
        git_ref: "main".to_string(),
        task: "regression fixture".to_string(),
        name_hint: None,
        runtime: None,
        ephemeral: Some(true),
        mcp_initiated: false,
        inject_task: Some(false),
        deliverable_id: None,
        force_new: true,
        worktree: false,
    }
}

/// Stand up an isolated daemon state plus the env pins, returning both guards.
fn isolate() -> (TempDir, TempDir, TempDir, EnvGuard) {
    let home = TempDir::new().expect("home tempdir");
    let roots = TempDir::new().expect("roots tempdir");
    let daemon_root = TempDir::new().expect("daemon root tempdir");
    let env = EnvGuard::set(&[
        ("HOME", home.path()),
        (REPOS_ROOT_ENV, roots.path()),
        (WORKSPACE_ROOT_ENV, roots.path()),
    ]);
    (home, roots, daemon_root, env)
}

/// The daemon spawn RPC refuses a remote `repo_url` before any side effect.
///
/// Why: this is the function both the HTTP `POST /api/v1/sessions/managed`
/// route and the SM-STDIO adapter call, so refusing here covers both.
/// What: spawns with an https URL and asserts the error names ADR-0055 and the
/// remedy, and that no record was created.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn spawn_rpc_refuses_a_remote_repo_url() {
    let (_home, _roots, daemon_root, _env) = isolate();
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(daemon_root.path().to_path_buf()).await,
    );

    let err = spawn_managed(
        &state,
        ManagedSessionId::new(),
        params("https://github.com/an-owner/a-repo"),
    )
    .await
    .expect_err("a remote repo_url must fail the spawn");

    assert!(
        err.contains("ADR-0055"),
        "the spawn must fail BECAUSE trusty-mpm no longer clones, not because a \
         clone it attempted went wrong; got: {err}"
    );
    assert!(
        err.contains("tm session new"),
        "the refusal must name the two-step remedy; got: {err}"
    );

    let mgr = state.session_manager().await;
    assert!(
        mgr.list().await.is_empty(),
        "a refused spawn must leave no session record behind"
    );
}

/// A relative path is refused with the same message as a remote URL.
///
/// Why: `is_local_workdir` rejects a relative path because it is ambiguous
/// against the daemon's cwd. Before #6000 that ambiguity fell through to the
/// clone branch; now it must land on the same actionable refusal.
/// What: spawns with `some/relative/dir` and asserts the ADR-0055 message.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn spawn_rpc_refuses_a_relative_path() {
    let (_home, _roots, daemon_root, _env) = isolate();
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(daemon_root.path().to_path_buf()).await,
    );

    let err = spawn_managed(&state, ManagedSessionId::new(), params("some/relative/dir"))
        .await
        .expect_err("a relative path must fail the spawn");

    assert!(
        err.contains("ADR-0055"),
        "a relative path must land on the local-path refusal; got: {err}"
    );
}

/// A subdirectory of a repository is refused, not provisioned (ADR-0055).
///
/// Why: this is the second call site #6000 removed. A directory INSIDE a
/// repository has no `.git` of its own, so the in-project path declines it —
/// and `git config` answers from the parent, so it looks like a repo with a
/// remote. The old code cloned that remote into a managed workspace and added a
/// worktree there. With no provisioner left it must say so.
/// What: builds a real checkout with a GitHub origin, spawns against a
/// subdirectory of it with `worktree: true` (the request that reaches this
/// branch), and asserts the error names ADR-0055 and the repository root.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn a_subdirectory_of_a_repo_is_refused_not_provisioned() {
    let (_home, _roots, daemon_root, _env) = isolate();

    let checkout = TempDir::new().expect("checkout tempdir");
    for args in [
        vec!["init", "-q"],
        vec![
            "remote",
            "add",
            "origin",
            "https://github.com/an-owner/a-repo",
        ],
    ] {
        let out = std::process::Command::new("git")
            .args(&args)
            .current_dir(checkout.path())
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?} failed: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let subdir = checkout.path().join("crates").join("a-crate");
    std::fs::create_dir_all(&subdir).expect("subdir");

    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(daemon_root.path().to_path_buf()).await,
    );

    let mut p = params(&subdir.to_string_lossy());
    p.worktree = true;
    let err = spawn_managed(&state, ManagedSessionId::new(), p)
        .await
        .expect_err("a subdirectory has no checkout of its own to run in");

    assert!(
        err.contains("ADR-0055"),
        "the refusal must say trusty-mpm no longer provisions a workspace, not \
         report a provisioning failure; got: {err}"
    );
    assert!(
        err.contains("root"),
        "the refusal must point at the repository root; got: {err}"
    );
}

/// The `session_new` MCP tool refuses a remote `repo_url`.
///
/// Why: the MCP surface is the one an LLM caller drives, so its refusal has to
/// carry the remedy rather than an opaque provisioning failure. The check runs
/// AHEAD of the MCP spawn gate deliberately: no daemon configuration can make a
/// remote URL work, so that is the more specific defect to report.
/// What: calls `mcp_session::session_new` with an https URL and asserts the
/// error names ADR-0055 rather than the spawn gate.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn mcp_session_new_refuses_a_remote_repo_url() {
    let (_home, _roots, daemon_root, _env) = isolate();
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(daemon_root.path().to_path_buf()).await,
    );

    let err = trusty_mpm::daemon::mcp_session::session_new(
        &state,
        "https://github.com/an-owner/a-repo",
        "main",
        "regression fixture",
        None,
        None,
        Some(true),
    )
    .await
    .expect_err("a remote repo_url must fail the MCP tool");

    assert!(
        err.contains("ADR-0055"),
        "the MCP refusal must name the removed capability, not the spawn gate; \
         got: {err}"
    );

    let mgr = state.session_manager().await;
    assert!(
        mgr.list().await.is_empty(),
        "a refused MCP spawn must leave no session record behind"
    );
}
