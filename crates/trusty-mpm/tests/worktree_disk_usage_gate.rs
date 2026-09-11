//! End-to-end coverage for the `disk.max_usage_pct` worktree gate (#7497).
//!
//! Why: the gate's unit tests feed synthetic percentages, and the pm-guard hook
//! covers the Bash surface. Neither drives the REAL provisioning path — the one
//! that runs `git worktree add` — through a real measurement of a real mount.
//! This file does, and it is also why the gate's own `cfg`-free design matters:
//! an EXPLICIT `disk.max_usage_pct` applies in a test process exactly as in
//! production, so nothing here is exercising a test-only branch.
//!
//! What: measures the mount the fixture sits on, then pins the threshold at the
//! floor of that measurement (must refuse) and one point above it (must
//! create). Deriving both thresholds from the live measurement is what keeps
//! the assertions independent of how full the runner's disk is.
//! Test: this file IS the test.

use serial_test::serial;
use tempfile::TempDir;

use trusty_mpm::core::disk_usage_guard;
use trusty_mpm::daemon::managed_routes::inproject::create_session_worktree;
use trusty_mpm::daemon::managed_routes::{SpawnParams, spawn_managed};
use trusty_mpm::daemon::state::DaemonState;
use trusty_mpm::session_manager::ManagedSessionId;

/// The repos root the base clone is resolved from.
const REPOS_ROOT_ENV: &str = "TRUSTY_MPM_REPOS_ROOT";
/// The workspace root the local-path fallback would clone into.
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

/// RAII guard pinning `$HOME` and restoring it on drop, including on panic.
struct HomeGuard(Option<std::ffi::OsString>);

impl HomeGuard {
    fn set(dir: &std::path::Path) -> Self {
        let prev = std::env::var_os("HOME");
        // SAFETY: every caller is `#[serial]`, so only one thread in this
        // binary mutates the process environment at a time.
        unsafe { std::env::set_var("HOME", dir) };
        Self(prev)
    }
}

impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var("HOME", v),
                None => std::env::remove_var("HOME"),
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

/// A real, committed git repository usable as a base clone.
fn base_repo() -> TempDir {
    let dir = TempDir::new().expect("base tempdir");
    git(dir.path(), &["init", "-q"]);
    git(dir.path(), &["config", "user.email", "t@example.com"]);
    git(dir.path(), &["config", "user.name", "Test"]);
    git(dir.path(), &["commit", "-q", "--allow-empty", "-m", "root"]);
    dir
}

/// Write `disk.max_usage_pct` into `<home>/.trusty-tools/trusty-mpm/config.yaml`.
fn write_threshold(home: &std::path::Path, max_usage_pct: u8) {
    let dir = home.join(".trusty-tools").join("trusty-mpm");
    std::fs::create_dir_all(&dir).expect("create config dir");
    std::fs::write(
        dir.join("config.yaml"),
        format!("disk:\n  max_usage_pct: {max_usage_pct}\n"),
    )
    .expect("write config");
}

/// The measured usage of the mount holding `path`, floored to a threshold this
/// test can express.
///
/// Returns `None` on a volume measuring under 1%, where no valid threshold
/// (1..=100) sits at or below the measurement and the refuse-arm cannot be set
/// up at all.
fn refuse_threshold(path: &std::path::Path) -> Option<(u8, f32)> {
    let m = disk_usage_guard::measure(path).expect("the fixture sits on some mount");
    let floored = m.usage_pct.floor();
    (floored >= 1.0).then_some((floored as u8, m.usage_pct))
}

/// A worktree whose mount is AT OR ABOVE the threshold is refused, and nothing
/// is created (#7497).
///
/// Why: this is the feature. The refusal must also be actionable — mount,
/// measured percent, threshold, config key — because an operator reading only
/// "refused" cannot act.
/// What: pins the threshold at the floor of the live measurement (so the mount
/// is at or above it by construction), calls the production
/// `create_session_worktree`, and asserts the error text plus the absence of
/// any directory or branch.
/// Test: this function IS the test.
#[test]
#[serial]
fn create_session_worktree_refuses_at_or_above_the_threshold() {
    let home = TempDir::new().expect("home tempdir");
    let base = base_repo();
    let Some((threshold, measured)) = refuse_threshold(base.path()) else {
        eprintln!("skipped: the fixture volume measures under 1% used");
        return;
    };
    write_threshold(home.path(), threshold);
    let _home = HomeGuard::set(home.path());

    let err = create_session_worktree(base.path(), "tm-gate-01", &ManagedSessionId::new())
        .expect_err("a mount at or above the threshold must refuse the worktree");

    assert!(
        err.contains("disk.max_usage_pct"),
        "the refusal must name the config key: {err}"
    );
    assert!(
        err.contains(&format!("threshold of {threshold}%")),
        "the refusal must name the threshold in force: {err}"
    );
    assert!(
        err.contains("% disk usage"),
        "the refusal must name the measured percent (measured {measured}): {err}"
    );
    assert!(
        !base.path().join(".worktrees").join("tm-gate-01").exists(),
        "a refused worktree must leave nothing on disk"
    );
}

/// One point below the measurement, the same call creates the worktree (#7497).
///
/// Why: the acceptance criterion is that nothing changes under the threshold. A
/// gate that refused here would be indistinguishable from one that refuses
/// always.
/// What: threshold = floor(measured) + 1, so the mount is strictly below it.
/// Test: this function IS the test.
#[test]
#[serial]
fn create_session_worktree_creates_below_the_threshold() {
    let home = TempDir::new().expect("home tempdir");
    let base = base_repo();
    let Some((floored, _)) = refuse_threshold(base.path()) else {
        eprintln!("skipped: the fixture volume measures under 1% used");
        return;
    };
    if floored >= 100 {
        eprintln!("skipped: the fixture volume is full; no threshold sits above it");
        return;
    }
    write_threshold(home.path(), floored + 1);
    let _home = HomeGuard::set(home.path());

    let path = create_session_worktree(base.path(), "tm-gate-02", &ManagedSessionId::new())
        .expect("a mount below the threshold must still get its worktree");
    assert!(
        path.exists(),
        "the worktree must exist at {}",
        path.display()
    );
}

/// A `worktree: true` spawn over the threshold fails and leaves NO session
/// record (#7497, extending the ADR-0037 fail-closed pattern).
///
/// Why: refusing inside `create_session_worktree` is only half the contract.
/// The pre-ADR-0037 behaviour was to warn and spawn somewhere else, so the
/// assertion that separates a refusal from a silent fall-through is that the
/// session manager holds nothing afterwards.
/// What: a real checkout with a GitHub origin, a real base clone under the
/// repos root (so the spawn reaches worktree creation rather than failing
/// earlier), a threshold at the floor of the live measurement, and then the
/// real `spawn_managed`.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn an_over_threshold_spawn_leaves_no_session_record() {
    let home = TempDir::new().expect("home tempdir");
    let roots = TempDir::new().expect("roots tempdir");
    let Some((threshold, _)) = refuse_threshold(roots.path()) else {
        eprintln!("skipped: the fixture volume measures under 1% used");
        return;
    };
    write_threshold(home.path(), threshold);
    let _env = EnvGuard::set(&[
        ("HOME", home.path()),
        (REPOS_ROOT_ENV, roots.path()),
        (WORKSPACE_ROOT_ENV, roots.path()),
    ]);

    // The base clone the in-project path would use, already present and real,
    // so `ensure_base_clone` short-circuits without any network.
    let base = roots.path().join("an-owner").join("a-repo");
    std::fs::create_dir_all(&base).expect("create base clone dir");
    git(&base, &["init", "-q"]);
    git(&base, &["config", "user.email", "t@example.com"]);
    git(&base, &["config", "user.name", "Test"]);
    git(&base, &["commit", "-q", "--allow-empty", "-m", "root"]);

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

    let daemon_root = TempDir::new().expect("daemon root tempdir");
    let state = std::sync::Arc::new(
        DaemonState::with_root_isolated_managed(daemon_root.path().to_path_buf()).await,
    );

    let err = spawn_managed(
        &state,
        ManagedSessionId::new(),
        SpawnParams {
            repo_url: checkout.path().to_string_lossy().to_string(),
            git_ref: "main".to_string(),
            task: "disk gate fixture".to_string(),
            name_hint: None,
            runtime: None,
            ephemeral: Some(true),
            mcp_initiated: false,
            inject_task: Some(false),
            deliverable_id: None,
            force_new: true,
            worktree: true,
        },
    )
    .await
    .expect_err("a spawn over the disk threshold must fail");

    assert!(
        err.contains("disk.max_usage_pct"),
        "the spawn must fail BECAUSE of the disk gate, not for some other \
         reason: {err}"
    );
    let mgr = state.session_manager().await;
    assert!(
        mgr.list().await.is_empty(),
        "a refused worktree request must leave no session record"
    );
}

/// An absent `disk:` section changes nothing (#7497).
///
/// Why: the gate ships enabled by default in production, but a config that
/// never mentions it must behave exactly as before — the acceptance criterion
/// for every existing caller.
/// What: no config file at all, and the worktree is created.
/// Test: this function IS the test.
#[test]
#[serial]
fn an_absent_disk_section_creates_the_worktree_as_before() {
    let home = TempDir::new().expect("home tempdir");
    let base = base_repo();
    let _home = HomeGuard::set(home.path());

    let path = create_session_worktree(base.path(), "tm-gate-03", &ManagedSessionId::new())
        .expect("no `disk:` section must change nothing");
    assert!(path.exists());
}
