//! The `owner_config` doctor row reads the per-owner launch config (#8988 PR 1a).
//!
//! Why an INTEGRATION test: these assertions must run against the code BEFORE
//! the fix, to prove they were red there. `run_doctor`, `DoctorCheck` and
//! `CheckStatus` are public library API that predates #8988, so the same file
//! compiles on either side; a unit test would have to name the new module and
//! could only ever have been green.
//! What: plants `<repos_root>/<owner>/.trusty-mpm-owner.toml` (and, for the
//! fallback arms, a last-good record under the scratch `$HOME`), runs the whole
//! doctor battery for a project under that owner, and asserts on the one row.
//! Each failure arm — a bad file with and without a last-good record, a broken
//! `extends` chain, and a deleted file beside a retained last-good — has its own
//! case, because each is a branch where the state advances while the file is bad.
//! Test: this file IS the test.

use std::path::{Path, PathBuf};

use trusty_mpm::core::doctor::{CheckStatus, DoctorCheck};
use trusty_mpm::daemon::doctor::run_doctor;

/// The row name, spelled as a literal: the production constant did not exist
/// before the fix.
const ROW: &str = "owner_config";

/// The owner file's basename, as a literal for the same reason.
const OWNER_FILE: &str = ".trusty-mpm-owner.toml";

/// Restores `HOME` and `TRUSTY_MPM_REPOS_ROOT` when a case ends, panic or not.
///
/// `env_serial` runs one test at a time, so no other test observes the rewrite.
struct EnvGuard {
    home: Option<std::ffi::OsString>,
    repos_root: Option<std::ffi::OsString>,
}

impl EnvGuard {
    fn install(home: &Path, repos_root: &Path) -> Self {
        let guard = Self {
            home: std::env::var_os("HOME"),
            repos_root: std::env::var_os("TRUSTY_MPM_REPOS_ROOT"),
        };
        // SAFETY: `env_serial` forces one test thread; Drop restores both.
        unsafe {
            std::env::set_var("HOME", home);
            std::env::set_var("TRUSTY_MPM_REPOS_ROOT", repos_root);
        }
        guard
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        // SAFETY: as in `install`.
        unsafe {
            match self.home.take() {
                Some(h) => std::env::set_var("HOME", h),
                None => std::env::remove_var("HOME"),
            }
            match self.repos_root.take() {
                Some(r) => std::env::set_var("TRUSTY_MPM_REPOS_ROOT", r),
                None => std::env::remove_var("TRUSTY_MPM_REPOS_ROOT"),
            }
        }
    }
}

/// A scratch home holding `projects/acme/widget` as a git checkout.
struct Fixture {
    _tmp: tempfile::TempDir,
    home: PathBuf,
    repos_root: PathBuf,
    project: PathBuf,
}

impl Fixture {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let home = tmp.path().canonicalize().expect("canonical tempdir");
        let repos_root = home.join("projects");
        let project = repos_root.join("acme").join("widget");
        std::fs::create_dir_all(&project).expect("project dir");
        let status = std::process::Command::new("git")
            .args(["init", "-q"])
            .arg(&project)
            .status()
            .expect("run git init");
        assert!(status.success(), "git init failed");
        Self {
            _tmp: tmp,
            home,
            repos_root,
            project,
        }
    }

    /// Write `<repos_root>/<owner>/.trusty-mpm-owner.toml`.
    fn owner_file(&self, owner: &str, body: &str) {
        let dir = self.repos_root.join(owner);
        std::fs::create_dir_all(&dir).expect("owner dir");
        std::fs::write(dir.join(OWNER_FILE), body).expect("owner file");
    }

    /// Plant a last-good record for `owner`, recorded at `recorded_at`.
    fn last_good(&self, owner: &str, recorded_at: &str) {
        let dir = self
            .home
            .join(".trusty-tools")
            .join("trusty-mpm")
            .join("owner-policy");
        std::fs::create_dir_all(&dir).expect("last-good dir");
        let record = serde_json::json!({
            "version": 1,
            "owner": owner,
            "chain": [owner],
            "policy": {"allow": [], "deny": ["heygen"], "claude_ai_connectors": false},
            "source_sha256": "0".repeat(64),
            "recorded_at": recorded_at,
        });
        std::fs::write(
            dir.join(format!("{owner}.last-good.json")),
            serde_json::to_vec_pretty(&record).expect("serialize"),
        )
        .expect("last-good file");
    }

    /// Run the doctor battery for the project and return the owner row.
    async fn row(&self) -> DoctorCheck {
        trusty_mpm::daemon::doctor_launchd_process_type::override_launch_agents_dir(
            self.home.join("no-launch-agents"),
        );
        let _env = EnvGuard::install(&self.home, &self.repos_root);
        let report = run_doctor(Some(&self.project), Some(&self.repos_root), &[], None).await;
        report
            .checks
            .into_iter()
            .find(|c| c.name == ROW)
            .unwrap_or_else(|| panic!("tm doctor has no `{ROW}` row"))
    }
}

/// Q5: `allow = []` is a valid policy — closure condition 1 is written this way.
#[tokio::test]
async fn owner_config_reports_a_valid_owner_file_with_an_empty_allow_list() {
    let fx = Fixture::new();
    fx.owner_file("acme", "[mcp]\nallow = []\nclaude_ai_connectors = false\n");

    let row = fx.row().await;

    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(row.message.contains("acme"), "{}", row.message);
}

/// Q7: a PR 2 table is an unknown key, so the file is bad. With no last-good
/// on record the owner layer is skipped — never a refused launch.
#[tokio::test]
async fn owner_config_fails_on_a_bad_owner_file_and_skips_the_layer_without_a_last_good() {
    let fx = Fixture::new();
    fx.owner_file("acme", "[tools]\ndeny = [\"WebFetch\"]\n");

    let row = fx.row().await;

    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("INVALID"), "{}", row.message);
    assert!(row.message.contains("no last good"), "{}", row.message);
}

/// Q2: denying a floor server is a bad file; the last-good policy takes over.
#[tokio::test]
async fn owner_config_fails_on_a_floor_violation_and_names_the_last_good_it_uses() {
    let fx = Fixture::new();
    fx.owner_file("acme", "[mcp]\ndeny = [\"claude-in-chrome\"]\n");
    fx.last_good("acme", "2026-10-01T06:48:00Z");

    let row = fx.row().await;

    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("INVALID"), "{}", row.message);
    assert!(
        row.message.contains("2026-10-01T06:48:00Z"),
        "the row must name the last-good record it falls back to: {}",
        row.message
    );
}

/// A cycle in `extends` makes the child a bad file.
#[tokio::test]
async fn owner_config_fails_on_an_extends_cycle() {
    let fx = Fixture::new();
    fx.owner_file("acme", "extends = \"beta\"\n");
    fx.owner_file("beta", "extends = \"acme\"\n");

    let row = fx.row().await;

    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
    assert!(row.message.contains("cycle"), "{}", row.message);
}

/// A deleted owner file turns the layer OFF, even with a last-good on record:
/// deleting the file is the operator's off switch.
#[tokio::test]
async fn owner_config_is_off_when_the_owner_file_is_deleted_even_with_a_last_good() {
    let fx = Fixture::new();
    fx.last_good("acme", "2026-10-01T06:48:00Z");

    let row = fx.row().await;

    assert_eq!(row.status, CheckStatus::Ok, "{}", row.message);
    assert!(row.message.contains("none"), "{}", row.message);
    assert!(
        !row.message.contains("falls back"),
        "a deleted file must never fall back to last-good: {}",
        row.message
    );
}

/// #8988: a dangling symlink at the owner-file path is an unreadable (bad) file,
/// not a deleted one — the row fails instead of reporting the layer off.
#[cfg(unix)]
#[tokio::test]
async fn owner_config_fails_on_a_dangling_symlink_owner_file() {
    let fx = Fixture::new();
    let dir = fx.repos_root.join("acme");
    std::fs::create_dir_all(&dir).expect("owner dir");
    std::os::unix::fs::symlink(dir.join("missing-target"), dir.join(OWNER_FILE))
        .expect("dangling symlink");

    let row = fx.row().await;

    assert_eq!(row.status, CheckStatus::Fail, "{}", row.message);
}
