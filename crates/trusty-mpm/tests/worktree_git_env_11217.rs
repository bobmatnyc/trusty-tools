//! An inherited `GIT_COMMON_DIR` never makes a worktree register another repo (#11217).
//!
//! Why: the daemon's environment may carry `GIT_DIR`, `GIT_COMMON_DIR` or
//! `GIT_WORK_TREE`. Unscrubbed, git resolved a worktree of repo A to repo X,
//! so the session would register (or report) X — a D50 boundary breach.
//! What: builds repo A with a linked worktree and an unrelated repo X, sets
//! `GIT_COMMON_DIR` to X's `.git` for the registration call only, and asserts
//! the row names A's main checkout and X resolves to nothing.
//! Test: this file IS the test (mounted in the `env_serial` target, which runs
//! one test at a time, so the env rewrite is unobserved by other tests).

use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_mpm::project::{
    CheckoutLookupError, SessionRegistration, register_session_checkout, registered_checkout,
};

/// The variables a hostile or stale environment could use to redirect git.
const REDIRECTS: [&str; 3] = ["GIT_DIR", "GIT_COMMON_DIR", "GIT_WORK_TREE"];

/// Sets `GIT_COMMON_DIR` (clearing the other redirects); restores all on drop.
struct GitEnvGuard {
    saved: Vec<(&'static str, Option<OsString>)>,
}

impl GitEnvGuard {
    fn common_dir(dir: &Path) -> Self {
        let saved = REDIRECTS
            .iter()
            .map(|k| (*k, std::env::var_os(k)))
            .collect();
        // SAFETY: `env_serial` runs one test at a time (#8345), so no other
        // thread reads the environment while it is rewritten.
        unsafe {
            for k in REDIRECTS {
                std::env::remove_var(k);
            }
            std::env::set_var("GIT_COMMON_DIR", dir);
        }
        Self { saved }
    }
}

impl Drop for GitEnvGuard {
    fn drop(&mut self) {
        // SAFETY: as in `common_dir`.
        unsafe {
            for (k, v) in &self.saved {
                match v {
                    Some(v) => std::env::set_var(k, v),
                    None => std::env::remove_var(k),
                }
            }
        }
    }
}

/// `git -C dir args…` with the redirects removed, panicking on failure.
fn git(dir: &Path, args: &[&str]) {
    let mut cmd = Command::new("git");
    cmd.arg("-C").arg(dir).args(args);
    for k in REDIRECTS {
        cmd.env_remove(k);
    }
    let status = cmd.status().expect("run git");
    assert!(status.success(), "git {args:?} failed");
}

/// A committed checkout at `dir` whose origin is `remote`, canonicalized.
fn committed(dir: &Path, remote: &str) -> PathBuf {
    std::fs::create_dir_all(dir).expect("mkdir checkout");
    git(dir, &["init", "-q"]);
    git(dir, &["remote", "add", "origin", remote]);
    git(
        dir,
        &[
            "-c",
            "user.name=t",
            "-c",
            "user.email=t@example.invalid",
            "commit",
            "-q",
            "--allow-empty",
            "-m",
            "init",
        ],
    );
    dir.canonicalize().expect("canonical checkout")
}

/// #11217: with `GIT_COMMON_DIR` pointing at repo X, a session in a worktree
/// of repo A registers A's main checkout, never X. Fails before the scrub,
/// when every registration git call inherited the variable.
#[tokio::test]
async fn inherited_git_common_dir_never_registers_another_repo_11217() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let registry = tmp.path().join("registry");
    let repo_a = committed(&tmp.path().join("a"), "https://github.com/acme/a");
    let repo_x = committed(&tmp.path().join("x"), "https://github.com/acme/x");
    let wt = tmp.path().join("a-wt");
    git(
        &repo_a,
        &["worktree", "add", "-q", "-b", "wt", &wt.to_string_lossy()],
    );

    let outcome = {
        let _env = GitEnvGuard::common_dir(&repo_x.join(".git"));
        register_session_checkout(&registry, &wt, None).await
    };

    assert!(
        matches!(&outcome, Ok(SessionRegistration::Registered { name }) if name == "a"),
        "{outcome:?}"
    );
    assert_eq!(registered_checkout(&registry, "acme/a").expect("a"), repo_a);
    assert!(
        matches!(
            registered_checkout(&registry, "acme/x"),
            Err(CheckoutLookupError::NotRegistered(_))
        ),
        "repo X was registered"
    );
}
