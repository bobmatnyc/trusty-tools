//! End-to-end coverage for the `disk.max_usage_pct` worktree gate (#7497).
//!
//! Why: the guard's unit tests feed synthetic percentages to the pure decision
//! functions, and the pm-guard hook covers the Bash surface. Neither drives the
//! REAL provisioning path — the one that runs `git worktree add` — so nothing
//! proved that the gate refuses BEFORE git creates anything, or that a refusal
//! leaves no session record behind.
//!
//! What: every assertion about the threshold pins the measurement through
//! [`create_session_worktree_measured`], so the verdict cannot depend on how
//! full the runner's volume happens to be (#7497 review, MEDIUM 2). One live
//! smoke test asserts the real measurement is plausible, and one spawn-level
//! test drives `spawn_managed` against the live disk because that path has no
//! measurement seam — it derives its threshold from what it measured.
//! Test: this file IS the test.

use serial_test::serial;
use tempfile::TempDir;

use trusty_mpm::core::disk_usage_guard::{self, MeasuredMount};
use trusty_mpm::daemon::managed_routes::inproject::{
    create_session_worktree, create_session_worktree_measured,
};
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

/// A pinned measurement — the seam that keeps these assertions independent of
/// the runner's disk.
fn pinned(usage_pct: f32) -> MeasuredMount {
    MeasuredMount {
        mount_point: "/System/Volumes/Data".to_string(),
        usage_pct,
    }
}

/// A worktree whose mount is AT OR ABOVE the threshold is refused, and nothing
/// is created (#7497).
///
/// Why: this is the feature, asserted at the real entry point rather than at
/// the pure decision function. The refusal must also be actionable — mount,
/// measured percent, threshold, config key — because an operator reading only
/// "refused" cannot act.
/// What: an explicit 90% threshold with a pinned 92.4% measurement, then the
/// production creation call, then the assertion that git created nothing.
/// Test: this function IS the test.
#[test]
#[serial]
fn create_session_worktree_refuses_a_pinned_over_threshold_measurement() {
    let home = TempDir::new().expect("home tempdir");
    write_threshold(home.path(), 90);
    let base = base_repo();
    let _home = HomeGuard::set(home.path());

    let err = create_session_worktree_measured(
        base.path(),
        "tm-gate-01",
        &ManagedSessionId::new(),
        Some(&pinned(92.4)),
    )
    .expect_err("a mount at or above the threshold must refuse the worktree");

    assert!(
        err.contains("disk.max_usage_pct"),
        "the refusal must name the config key: {err}"
    );
    assert!(
        err.contains("threshold of 90%"),
        "the refusal must name the threshold in force: {err}"
    );
    assert!(
        err.contains("92.4% disk usage"),
        "the refusal must name the measured percent: {err}"
    );
    assert!(
        err.contains("/System/Volumes/Data"),
        "the refusal must name the mount: {err}"
    );
    assert!(
        !base.path().join(".worktrees").join("tm-gate-01").exists(),
        "a refused worktree must leave nothing on disk"
    );
}

/// A mount that could not be measured refuses too — fail CLOSED (#7497,
/// ADR-0037).
///
/// Why: "I could not check" is not "it is fine". This arm is reachable because
/// `host_metrics::mount_for_path` returns `None` for a filesystem the OS does
/// not enumerate rather than substituting `/`.
/// What: a `None` measurement at the production entry point, and nothing
/// created.
/// Test: this function IS the test.
#[test]
#[serial]
fn create_session_worktree_refuses_an_unmeasurable_mount() {
    let home = TempDir::new().expect("home tempdir");
    write_threshold(home.path(), 90);
    let base = base_repo();
    let _home = HomeGuard::set(home.path());

    let err =
        create_session_worktree_measured(base.path(), "tm-gate-02", &ManagedSessionId::new(), None)
            .expect_err("an unmeasurable mount must refuse the worktree");

    assert!(
        err.contains("could not be measured"),
        "the refusal must say the measurement failed: {err}"
    );
    assert!(
        !base.path().join(".worktrees").join("tm-gate-02").exists(),
        "a refused worktree must leave nothing on disk"
    );
}

/// Below the threshold, the same call creates the worktree (#7497).
///
/// Why: the acceptance criterion is that nothing changes under the threshold. A
/// gate that refused here would be indistinguishable from one that refuses
/// always.
/// What: an explicit 90% threshold with a pinned 10% measurement.
/// Test: this function IS the test.
#[test]
#[serial]
fn create_session_worktree_creates_on_a_pinned_under_threshold_measurement() {
    let home = TempDir::new().expect("home tempdir");
    write_threshold(home.path(), 90);
    let base = base_repo();
    let _home = HomeGuard::set(home.path());

    let path = create_session_worktree_measured(
        base.path(),
        "tm-gate-03",
        &ManagedSessionId::new(),
        Some(&pinned(10.0)),
    )
    .expect("a mount below the threshold must still get its worktree");
    assert!(
        path.exists(),
        "the worktree must exist at {}",
        path.display()
    );
}

/// An absent `disk:` section changes nothing (#7497).
///
/// Why: the gate ships enabled by default, but a config that never mentions it
/// must behave exactly as before for every existing caller.
/// What: no config file at all — so the default 90% applies — with a pinned 10%
/// measurement, and the worktree is created.
/// Test: this function IS the test.
#[test]
#[serial]
fn an_absent_disk_section_creates_the_worktree_as_before() {
    let home = TempDir::new().expect("home tempdir");
    let base = base_repo();
    let _home = HomeGuard::set(home.path());

    let path = create_session_worktree_measured(
        base.path(),
        "tm-gate-04",
        &ManagedSessionId::new(),
        Some(&pinned(10.0)),
    )
    .expect("no `disk:` section must change nothing");
    assert!(path.exists());
}

/// The production entry point measures a real mount (#7497).
///
/// Why: the pinned tests above prove the DECISION; this proves the
/// MEASUREMENT — that `create_session_worktree` resolves an actual mount for a
/// directory that does not exist yet, which is the half a synthetic value can
/// never cover.
/// What: asserts the live measurement is a plausible mount, then runs the real
/// gated call against a threshold of 100% so the outcome cannot depend on the
/// runner's volume.
/// Test: this function IS the test.
#[test]
#[serial]
fn create_session_worktree_measures_a_real_mount() {
    let home = TempDir::new().expect("home tempdir");
    write_threshold(home.path(), 100);
    let base = base_repo();
    let _home = HomeGuard::set(home.path());

    let target = base.path().join(".worktrees").join("tm-gate-05");
    let measured =
        disk_usage_guard::measure(&target).expect("the fixture sits on an enumerated mount");
    assert!(!measured.mount_point.is_empty());
    assert!(
        (0.0..=100.0).contains(&measured.usage_pct),
        "usage must be a percentage, got {}",
        measured.usage_pct
    );

    let path = create_session_worktree(base.path(), "tm-gate-05", &ManagedSessionId::new())
        .expect("a 100% threshold can only refuse a completely full volume");
    assert!(path.exists());
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
/// earlier), and a threshold pinned at the floor of what the fixture volume
/// actually measures — the spawn path has no measurement seam, so this one
/// test derives its threshold from the live disk. It reports and skips on a
/// volume under 1% used, where no valid threshold (1..=100) sits at or below
/// the measurement.
/// Test: this function IS the test.
#[tokio::test]
#[serial]
async fn an_over_threshold_spawn_leaves_no_session_record() {
    let home = TempDir::new().expect("home tempdir");
    let roots = TempDir::new().expect("roots tempdir");
    let measured = disk_usage_guard::measure(roots.path()).expect("the fixture sits on a mount");
    let floored = measured.usage_pct.floor();
    if floored < 1.0 {
        eprintln!(
            "skipped: the fixture volume measures {}% used, below the lowest \
             expressible threshold",
            measured.usage_pct
        );
        return;
    }
    write_threshold(home.path(), floored as u8);
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
