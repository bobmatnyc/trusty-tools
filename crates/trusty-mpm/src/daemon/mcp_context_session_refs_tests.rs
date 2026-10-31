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

/// A pause snapshot written into the fixture checkout's store.
fn pause(fx: &Remote) -> trusty_common::catchup::pause::PauseSnapshotOutcome {
    let snapshot_path = fx
        .repo
        .join(".trusty-mpm/sessions/s-9160/session-20261013-100000.md");
    std::fs::create_dir_all(snapshot_path.parent().expect("parent")).expect("store");
    std::fs::write(&snapshot_path, "# Session Pause\n\n## Summary\n9160.\n").expect("snapshot");
    trusty_common::catchup::pause::PauseSnapshotOutcome {
        snapshot_path,
        timestamp: chrono::Utc::now(),
    }
}

/// Session refs on the bare remote.
fn remote_session_refs(fx: &Remote) -> Vec<String> {
    git(
        &fx.bare,
        &["for-each-ref", "--format=%(refname)", "refs/tm/sessions/"],
    )
    .lines()
    .map(str::to_string)
    .collect()
}

/// 🔴 #9160: a proven pin's env reaches the pause's ls-remote and push; the
/// ref key is still the ambient user id (Q3), never the pin's login.
/// Test: itself.
#[tokio::test]
async fn a_pinned_env_reaches_the_pause_publish() {
    let fx = Remote::new();
    let outcome = pause(&fx);
    let env = fx.pinned();
    let receipt =
        publish_session_ref_with(&fx.repo, "s-9160", &outcome, true, move |_| env, private).await;
    assert!(receipt.published, "{receipt:?}");
    let expected = crate::core::session_ref_publish::session_ref_for(&fx.repo, "s-9160");
    assert_eq!(receipt.ref_name, expected, "{receipt:?}");
    let ref_name = expected.expect("a ref name resolves");
    assert!(
        remote_session_refs(&fx).contains(&ref_name),
        "{:?}",
        remote_session_refs(&fx)
    );
}

/// 🔴 #9160 (Fail-Open Check): an unproven pin publishes nothing, even when
/// the ambient credential could push — the receipt carries the refusal and
/// the remote gains no ref.
/// Test: itself.
#[tokio::test]
async fn an_unproven_pin_never_publishes_with_the_ambient_credential() {
    let fx = Remote::new();
    fx.make_ambient_readable();
    let outcome = pause(&fx);
    let receipt = publish_session_ref_with(
        &fx.repo,
        "s-9160",
        &outcome,
        true,
        |_| SessionRefGitEnv::Refused {
            reason: "fixture: gh account 'octo-fixture' is not proven".to_string(),
        },
        private,
    )
    .await;
    assert!(!receipt.published, "{receipt:?}");
    let error = receipt.error.as_deref().unwrap_or_default();
    assert!(error.contains("is not proven"), "{receipt:?}");
    assert_eq!(
        remote_session_refs(&fx),
        ["refs/tm/sessions/someone/s-9160"],
        "a refused pin pushed a ref"
    );
}

/// A visibility resolver that confirms the origin is private.
fn private(_: &Path, _: &[(String, String)]) -> RepoVisibility {
    RepoVisibility::Private
}

/// 🔴 See #10614 (fail closed): the PRODUCTION pause path, on an
/// origin GitHub cannot vouch for, pushes no ref — even though the ambient
/// credential could. The receipt says why; the local snapshot is untouched.
/// Test: itself.
#[tokio::test]
async fn an_unknown_visibility_skips_the_publish() {
    let fx = Remote::new();
    fx.make_ambient_readable();
    let outcome = pause(&fx);
    let receipt = publish_session_ref(&fx.repo, "s-9160", &outcome, true).await;
    assert!(!receipt.published, "{receipt:?}");
    let error = receipt.error.as_deref().unwrap_or_default();
    assert!(
        error.contains("could not be confirmed as private"),
        "{receipt:?}"
    );
    assert_eq!(
        remote_session_refs(&fx),
        ["refs/tm/sessions/someone/s-9160"],
        "an unknown-visibility origin received a ref"
    );
    assert!(outcome.snapshot_path.exists(), "the local snapshot stays");
}

/// 🔴 See #10614: a public origin receives no ref, whatever
/// credential could push it.
/// Test: itself.
#[tokio::test]
async fn a_public_origin_skips_the_publish() {
    let fx = Remote::new();
    let outcome = pause(&fx);
    let env = fx.pinned();
    let receipt = publish_session_ref_with(
        &fx.repo,
        "s-9160",
        &outcome,
        true,
        move |_| env,
        |_, _| RepoVisibility::Public("o/r".to_string()),
    )
    .await;
    assert!(!receipt.published, "{receipt:?}");
    let error = receipt.error.as_deref().unwrap_or_default();
    assert!(error.contains("o/r is a public repository"), "{receipt:?}");
    assert_eq!(
        remote_session_refs(&fx),
        ["refs/tm/sessions/someone/s-9160"],
        "a public origin received a ref"
    );
}

/// 🔴 See #10614: a private origin still gets the ref, but the
/// pushed blob carries `~` for the home directory and the ref's user segment
/// is the hashed id, never the raw `git config user.name`.
/// Test: itself.
#[tokio::test]
async fn a_private_origin_publishes_a_redacted_ref() {
    let fx = Remote::new();
    let home = dirs::home_dir().expect("the test host has a home directory");
    let outcome = pause(&fx);
    let leaked = format!("worktree {}/scratch/df-g02\n", home.display());
    std::fs::write(&outcome.snapshot_path, &leaked).expect("snapshot");
    let env = fx.pinned();
    let receipt =
        publish_session_ref_with(&fx.repo, "s-9160", &outcome, true, move |_| env, private).await;
    assert!(receipt.published, "{receipt:?}");
    let ref_name = receipt.ref_name.clone().expect("a ref name");
    let user = ref_name
        .strip_prefix("refs/tm/sessions/")
        .and_then(|rest| rest.split('/').next())
        .unwrap_or_default();
    assert!(
        user.starts_with('u') && user.len() == 13 && !user.contains("refs-9160"),
        "{ref_name}"
    );
    let blob = git(
        &fx.bare,
        &[
            "show",
            &format!("{ref_name}:.trusty-mpm/sessions/s-9160/session-20261013-100000.md"),
        ],
    );
    assert_eq!(blob, "worktree ~/scratch/df-g02", "{blob}");
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

/// df-D4e (#10614): a ref written before the hash change, under the
/// login-named segment, is still found by a resume when no hashed ref exists.
/// Test: itself.
#[tokio::test]
async fn a_resume_reads_a_legacy_login_named_ref() {
    use crate::core::session_ref_publish::{
        SessionRefRequest, publish_session_ref_as, resolve_legacy_user_id, resolve_user_id,
    };
    let fx = Remote::new();
    let outcome = pause(&fx);
    let legacy = resolve_legacy_user_id(&fx.repo).expect("the fixture sets user.name");
    assert_ne!(Some(legacy.clone()), resolve_user_id(&fx.repo));
    let env = fx.pinned();
    publish_session_ref_as(
        &SessionRefRequest {
            repo: &fx.repo,
            session_id: "s-9160",
            snapshot_path: &outcome.snapshot_path,
            timestamp: outcome.timestamp,
            git_env: env.vars(),
        },
        Some(&legacy),
    )
    .expect("the legacy-named fixture ref is written");
    let legacy_ref = format!("refs/tm/sessions/{legacy}/s-9160");
    assert!(
        remote_session_refs(&fx).contains(&legacy_ref),
        "{:?}",
        remote_session_refs(&fx)
    );
    std::fs::remove_dir_all(fx.repo.join(".trusty-mpm/sessions")).expect("drop the cache");

    let receipt = hydrate_session_refs_with(&fx.repo, Some("s-9160"), move |_| env).await;
    assert!(receipt.hydrated, "{receipt:?}");
    assert!(receipt.own_ref_found, "{receipt:?}");
    assert_eq!(receipt.restored, 1, "{receipt:?}");
    assert!(outcome.snapshot_path.exists(), "the snapshot was restored");
    assert_eq!(
        remote_session_refs(&fx).len(),
        2,
        "the read pushed nothing: {:?}",
        remote_session_refs(&fx)
    );
}
