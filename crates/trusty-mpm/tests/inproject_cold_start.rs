//! Integration test: a cold start hands off to the daemon-managed spawn path (#4990).
//!
//! Why: the unit tests prove `ensure_managed_checkout_at` clones and refuses
//! correctly. They do NOT prove the thing that makes the feature work — that
//! what it produces is exactly what the daemon-managed session-launch path
//! demands. `try_inproject_spawn` is that gate: it accepts a directory only
//! when `.git` exists AND `remote.origin.url` parses into a GitHub identity,
//! and its `Ok(Some((base, owner, repo)))` return is what the lifecycle layer
//! turns into a `SessionRecord` with a tmux pane. Feeding a freshly
//! cold-started checkout through the real gate is the strongest claim available
//! without a live daemon, tmux server, and `claude` binary.
//!
//! HERMETIC BY CONSTRUCTION. Both the cold start and `try_inproject_spawn`
//! resolve `<repos_root>/<owner>/<repo>`, which defaults to the operator's real
//! `~/trusty-mpm-projects`. Pinning `TRUSTY_MPM_REPOS_ROOT` to a temp directory
//! is what keeps this test from writing into a real checkout — or, on a machine
//! that has not cloned the repo, from cloning it over the network.
//! What: builds a real local source repo, cold-starts a managed checkout from
//! it with no pre-existing cwd, and asserts `try_inproject_spawn` accepts the
//! result and reports the same base path.
//! Test: this file IS the test; run with `cargo test -p trusty-mpm --test
//! inproject_cold_start`.

use std::path::{Path, PathBuf};
use std::process::Command;

use trusty_mpm::daemon::managed_routes::inproject::{REPOS_ROOT_ENV, try_inproject_spawn};
use trusty_mpm::daemon::managed_routes::inproject_cold_start::{
    ensure_managed_checkout, ensure_managed_checkout_at,
};

/// RAII guard pinning `TRUSTY_MPM_REPOS_ROOT` and restoring it on drop.
///
/// Why: `base_clone_path` resolves against the operator's real home by default.
/// Without this, the cold start under test would clone into (and
/// `try_inproject_spawn` would write `.git/info/exclude` inside) the real
/// `~/trusty-mpm-projects/bobmatnyc/trusty-tools`. `std::env::set_var` is
/// `unsafe` in Rust 2024 because it is thread-unsafe, so every test using this
/// guard is `#[serial_test::serial]` — the same pattern `tests/local_spawn.rs`
/// uses for `$HOME`.
struct ReposRootGuard(Option<String>);

impl ReposRootGuard {
    fn set(dir: &Path) -> Self {
        let prev = std::env::var(REPOS_ROOT_ENV).ok();
        // SAFETY: every caller is `#[serial_test::serial]`, so only one thread
        // in this test binary mutates the environment at a time.
        unsafe { std::env::set_var(REPOS_ROOT_ENV, dir) };
        Self(prev)
    }
}

impl Drop for ReposRootGuard {
    fn drop(&mut self) {
        // SAFETY: see `set`.
        match &self.0 {
            Some(p) => unsafe { std::env::set_var(REPOS_ROOT_ENV, p) },
            None => unsafe { std::env::remove_var(REPOS_ROOT_ENV) },
        }
    }
}

/// Run a git command in `dir`, panicking with full output on failure.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("failed to spawn git {args:?} in {dir:?}: {e}"));
    assert!(
        out.status.success(),
        "git {args:?} in {dir:?} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Build a one-commit source repository to clone from.
fn init_source(path: &Path) -> PathBuf {
    std::fs::create_dir_all(path).expect("mkdir source");
    git(path, &["init", "-q", "-b", "main"]);
    git(path, &["config", "user.email", "test@example.com"]);
    git(path, &["config", "user.name", "Test"]);
    std::fs::write(path.join("README.md"), "hello\n").expect("write");
    git(path, &["add", "."]);
    git(path, &["commit", "-q", "-m", "initial"]);
    path.to_path_buf()
}

/// THE COLD-START PROOF: no checkout, no cwd — just an identity and a URL — and
/// the result satisfies the daemon-managed spawn gate.
///
/// Why: `try_inproject_spawn` returning `Ok(Some(..))` is precisely the
/// condition under which `spawn_managed_routed` proceeds to resolve a session
/// name, create the per-session worktree, and register a `SessionRecord`. A
/// cold-started checkout that fails this gate would silently fall through to
/// the local-path spawn — the exact "not a managed session" outcome the feature
/// exists to prevent.
#[test]
#[serial_test::serial]
fn cold_start_produces_a_checkout_the_managed_spawn_path_accepts() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let source = init_source(&tmp.path().join("source"));
    let repos_root = tmp.path().join("repos");
    let _guard = ReposRootGuard::set(&repos_root);

    // Cold start: nothing exists at the destination.
    let expected = repos_root.join("bobmatnyc").join("trusty-tools");
    assert!(!expected.exists(), "precondition: no checkout on disk");

    let checkout = ensure_managed_checkout(
        "bobmatnyc",
        "trusty-tools",
        source.to_str().expect("utf8 path"),
    )
    .expect("cold start clones");

    assert!(!checkout.reused, "nothing was there to reuse");
    assert_eq!(
        checkout.base_path, expected,
        "the cold start must land on the canonical <repos_root>/<owner>/<repo>"
    );

    // A real `git clone` of a GitHub remote leaves that GitHub URL as `origin`;
    // this fixture cloned from a local path, so point `origin` where the real
    // one would be. It is what the identity gate reads.
    git(
        &checkout.base_path,
        &[
            "remote",
            "set-url",
            "origin",
            "git@github.com:bobmatnyc/trusty-tools.git",
        ],
    );

    // THE GATE — the real function the daemon's lifecycle layer calls.
    let (resolved_base, owner, repo) = try_inproject_spawn(&checkout.base_path)
        .expect("in-project spawn detection must not error")
        .expect("a cold-started checkout must be accepted, not fall through to local-path spawn");

    assert_eq!(owner, "bobmatnyc");
    assert_eq!(repo, "trusty-tools");
    assert_eq!(
        resolved_base, checkout.base_path,
        "the spawn path must resolve to the very directory the cold start produced"
    );
}

/// A wrong-remote reuse is refused with the existing checkout untouched.
///
/// Why: the unit test proves the error type; this proves the ordering claim
/// that matters operationally — nothing is fetched into or merged into a
/// directory belonging to a different repository before the refusal lands.
#[test]
#[serial_test::serial]
fn wrong_remote_reuse_is_refused_without_touching_the_checkout() {
    let tmp = tempfile::TempDir::new().expect("temp dir");
    let source = init_source(&tmp.path().join("source"));
    let base = tmp.path().join("base");

    ensure_managed_checkout_at(&base, source.to_str().expect("utf8 path")).expect("first clone");
    let head_before = git(&base, &["rev-parse", "HEAD"]);

    let err = ensure_managed_checkout_at(&base, "https://github.com/someone/else.git")
        .expect_err("a different remote must be refused");
    assert!(
        err.to_string().contains("someone/else"),
        "message names the requested repo: {err}"
    );

    assert_eq!(
        git(&base, &["rev-parse", "HEAD"]),
        head_before,
        "the refused checkout must be left exactly as it was"
    );
}
