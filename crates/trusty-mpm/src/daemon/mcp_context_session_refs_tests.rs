//! Wiring tests: the resolved gh identity reaches the session-ref git calls,
//! and a refused one stops them (#9160).
//!
//! Why: the resolver's own tests prove what env it answers; these prove the
//! catch-up and the pause act on that answer. Every remote is a local bare
//! repository reached through `url.<path>.insteadOf`, so an env that carries
//! the rewrite stands in for a credential pin with no network.
//! Test: itself.

use std::path::{Path, PathBuf};

use super::*;

/// The unresolvable https origin every fixture checkout names.
const FIXTURE_ORIGIN: &str = "https://fixture.invalid/o/r.git";

/// Run `git -C <dir> <args>`, panicking on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = trusty_common::git::command_in(dir)
        .args(args)
        .output()
        .expect("git");
    assert!(
        out.status.success(),
        "fixture `git {args:?}` failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A bare remote carrying one session ref, and a checkout whose `origin` is
/// [`FIXTURE_ORIGIN`].
struct Remote {
    _tmp: tempfile::TempDir,
    bare: PathBuf,
    repo: PathBuf,
}

impl Remote {
    fn new() -> Self {
        let tmp = tempfile::TempDir::new().expect("tempdir");
        let bare = tmp.path().join("remote.git");
        let seed = tmp.path().join("seed");
        let repo = tmp.path().join("repo");
        for dir in [&seed, &repo] {
            std::fs::create_dir_all(dir).expect("dir");
        }
        git(
            tmp.path(),
            &["init", "--bare", bare.to_str().expect("utf-8")],
        );
        for dir in [&seed, &repo] {
            git(dir, &["init", "--initial-branch=main"]);
            git(dir, &["config", "user.email", "refs-9160@example.invalid"]);
            git(dir, &["config", "user.name", "refs-9160"]);
        }
        std::fs::write(seed.join("README.md"), "seed\n").expect("seed");
        git(&seed, &["add", "README.md"]);
        git(&seed, &["commit", "-m", "seed"]);
        git(
            &seed,
            &["update-ref", "refs/tm/sessions/someone/s-9160", "HEAD"],
        );
        git(
            &seed,
            &[
                "push",
                bare.to_str().expect("utf-8"),
                "refs/tm/sessions/someone/s-9160:refs/tm/sessions/someone/s-9160",
            ],
        );
        git(&repo, &["remote", "add", "origin", FIXTURE_ORIGIN]);
        Self {
            _tmp: tmp,
            bare,
            repo,
        }
    }

    /// The rewrite as child env — what a proven pin's vars stand in for.
    fn pinned(&self) -> SessionRefGitEnv {
        SessionRefGitEnv::Pinned {
            vars: vec![
                ("GIT_CONFIG_COUNT".to_string(), "1".to_string()),
                (
                    "GIT_CONFIG_KEY_0".to_string(),
                    format!("url.{}.insteadOf", self.bare.display()),
                ),
                ("GIT_CONFIG_VALUE_0".to_string(), FIXTURE_ORIGIN.to_string()),
            ],
            identity: "gh account 'octo-fixture'".to_string(),
        }
    }

    /// The same rewrite in the checkout's own config: the AMBIENT credential
    /// can read the remote.
    fn make_ambient_readable(&self) {
        let key = format!("url.{}.insteadOf", self.bare.display());
        git(&self.repo, &["config", &key, FIXTURE_ORIGIN]);
    }

    /// Session refs present in the checkout.
    fn local_session_refs(&self) -> String {
        git(&self.repo, &["for-each-ref", "refs/tm/sessions/"])
    }
}

/// 🔴 #9160: a proven pin's env reaches the catch-up's fetch — without it the
/// https origin does not even resolve.
/// Test: itself.
#[tokio::test]
async fn a_pinned_env_reaches_the_catchup_fetch() {
    let fx = Remote::new();
    let env = fx.pinned();
    let receipt = hydrate_session_refs_with(&fx.repo, Some("s-9160"), move |_| env).await;
    assert!(receipt.hydrated, "{receipt:?}");
    assert_eq!(receipt.refs_seen, 1, "{receipt:?}");
}

/// 🔴 #9160 (Fail-Open Check): an unproven pin makes NO fetch, even when the
/// ambient credential could read the remote — the catch-up reports
/// `hydrated: false` with the refusal, and no ref is fetched.
/// Test: itself.
#[tokio::test]
async fn an_unproven_pin_never_falls_back_to_the_ambient_credential() {
    let fx = Remote::new();
    fx.make_ambient_readable();
    let receipt =
        hydrate_session_refs_with(&fx.repo, Some("s-9160"), |_| SessionRefGitEnv::Refused {
            reason: "fixture: gh account 'octo-fixture' is not proven".to_string(),
        })
        .await;
    assert!(!receipt.hydrated, "{receipt:?}");
    assert_eq!(receipt.refs_seen, 0, "{receipt:?}");
    let error = receipt.error.as_deref().unwrap_or_default();
    assert!(error.contains("is not proven"), "{receipt:?}");
    assert!(
        fx.local_session_refs().is_empty(),
        "a refused pin fetched refs: {}",
        fx.local_session_refs()
    );
}

/// 🔴 #9160: a pinned fetch that fails names the identity it ran as.
/// Test: itself.
#[tokio::test]
async fn a_pinned_fetch_failure_names_the_identity() {
    let fx = Remote::new();
    let receipt =
        hydrate_session_refs_with(&fx.repo, Some("s-9160"), |_| SessionRefGitEnv::Pinned {
            vars: Vec::new(),
            identity: "gh account 'octo-fixture', GH_CONFIG_DIR=/fixture/gh".to_string(),
        })
        .await;
    assert!(!receipt.hydrated, "{receipt:?}");
    let error = receipt.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("git ran as gh account 'octo-fixture', GH_CONFIG_DIR=/fixture/gh"),
        "{receipt:?}"
    );
}
