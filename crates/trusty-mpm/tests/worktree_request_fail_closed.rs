//! Fail-open regression coverage for an explicit worktree request (ADR-0037).
//!
//! Why: `spawn_managed_routed` handled BOTH in-project failures — base-clone
//! establishment and worktree reservation — by logging a `warn!` and continuing
//! into `spawn_managed_local`, which spawns a session with no worktree at all.
//! A launch that asked for `--worktree` therefore reported success while running
//! somewhere the operator never chose. This drives the real `spawn_managed`
//! entry point, so a regression to the fall-through turns this file red.
//!
//! What: a real local checkout with a GitHub `origin`, launched with
//! `worktree: true`, against a repos root where the base clone CANNOT be
//! established — `<repos-root>/<owner>/<repo>/.worktrees` exists with no
//! top-level `.git`, which `migrate_old_layout_aside` refuses outright (#4270).
//! No network and no `git clone` is reached on the post-fix path.
//! Test: this file IS the test.
//!
//! Pre-fix, this same case reaches `spawn_managed_local`, which fails for its
//! OWN reason (provisioning), so the assertion is on WHICH failure came back —
//! `is_err()` alone would pass against the pre-fix commit and prove nothing.

use serial_test::serial;
use tempfile::TempDir;

use trusty_mpm::daemon::managed_routes::{SpawnParams, spawn_managed};
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::session_manager::ManagedSessionId;

/// Env vars this test pins so nothing resolves against the operator's real
/// machine: the repos root the base clone is computed from, the workspace root
/// the local-path fallback would clone into, and `$HOME`.
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

/// Run `git` in `dir`, panicking with stderr on failure.
fn git(dir: &std::path::Path, args: &[&str]) {
    let out = std::process::Command::new("git")
        .args(args)
        .current_dir(dir)
        .output()
        .expect("spawn git");
    assert!(
        out.status.success(),
        "git {args:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

fn params(repo_url: &str, worktree: bool) -> SpawnParams {
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
        worktree,
    }
}

/// A `--worktree` launch whose base clone cannot be established must FAIL,
/// not silently spawn without a worktree (ADR-0037).
///
/// Why: the pre-fix path `warn!`ed and continued, so the operator got a live
/// session in a placement they did not ask for and no error to act on.
/// What: builds a checkout with a GitHub origin, blocks the base clone with a
/// `.worktrees` directory the migration refuses to touch, spawns with
/// `worktree: true`, and asserts the returned error names the unhonourable
/// worktree request — the assertion that separates the fix from the
/// fall-through, since both end in `Err`.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn explicit_worktree_request_fails_rather_than_spawning_without_one() {
    let home = TempDir::new().expect("home tempdir");
    let roots = TempDir::new().expect("roots tempdir");
    let _env = EnvGuard::set(&[
        ("HOME", home.path()),
        (REPOS_ROOT_ENV, roots.path()),
        (WORKSPACE_ROOT_ENV, roots.path()),
    ]);

    // A real checkout with a parseable GitHub origin — enough for the routed
    // spawn to take the in-project branch.
    let checkout = TempDir::new().expect("checkout tempdir");
    git(checkout.path(), &["init", "-q"]);
    git(
        checkout.path(),
        &[
            "remote",
            "add",
            "origin",
            "https://github.com/an-owner/a-repo",
        ],
    );

    // #4270: a base directory holding `.worktrees` with no top-level `.git` is
    // one `migrate_old_layout_aside` refuses to rename, so `ensure_base_clone`
    // returns `Err` with no network involved.
    std::fs::create_dir_all(roots.path().join("an-owner/a-repo/.worktrees"))
        .expect("block the base clone");

    let daemon_root = TempDir::new().expect("daemon root tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(daemon_root.path().to_path_buf()).await,
    );

    let err = spawn_managed(
        &state,
        ManagedSessionId::new(),
        params(&checkout.path().to_string_lossy(), true),
    )
    .await
    .expect_err("an unhonourable worktree request must fail the spawn");

    assert!(
        err.contains("explicitly requested a worktree"),
        "the spawn must fail BECAUSE the worktree request could not be honoured, \
         not fall through to a placement nobody asked for; got: {err}"
    );

    let mgr = state.session_manager().await;
    assert!(
        mgr.list().await.is_empty(),
        "a refused worktree request must leave no session record behind"
    );
}
