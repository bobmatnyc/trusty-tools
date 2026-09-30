//! Tests for the #8934 local-only managed spawn.
//!
//! Each spawn test drives the real `spawn_managed` entry point against a real
//! temp git repository and a fake tmux driver, under a scratch `$HOME`, so the
//! routing, the record and the gh pin are the production ones.

use std::path::Path;

use super::*;
use crate::session_manager::ManagedSessionId;

/// Restores `$HOME` on drop; callers are `#[serial_test::serial]`.
struct HomeGuard(Option<String>);
impl Drop for HomeGuard {
    fn drop(&mut self) {
        // SAFETY: serialized via `#[serial_test::serial]`.
        match self.0 {
            Some(ref p) => unsafe { std::env::set_var("HOME", p) },
            None => unsafe { std::env::remove_var("HOME") },
        }
    }
}

/// A scratch `$HOME` whose config lifts the #7497 disk gate, so a worktree
/// test does not depend on how full the host volume is.
fn scratch_home() -> (tempfile::TempDir, HomeGuard) {
    let home = tempfile::TempDir::new().expect("tmp home");
    let dir = home.path().join(".trusty-tools").join("trusty-mpm");
    std::fs::create_dir_all(&dir).expect("config dir");
    std::fs::write(dir.join("config.yaml"), "disk:\n  max_usage_pct: 100\n").expect("config");
    let prior = std::env::var("HOME").ok();
    // SAFETY: serialized via `#[serial_test::serial]`.
    unsafe { std::env::set_var("HOME", home.path()) };
    (home, HomeGuard(prior))
}

/// Run git in `dir`, asserting success, and return trimmed stdout.
fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["-c", "user.name=t", "-c", "user.email=t@example.com"])
        .args(args)
        .output()
        .expect("git runs");
    assert!(out.status.success(), "git {args:?}: {out:?}");
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// A repository with one commit on `main` and, optionally, an `origin`.
fn repo(dir: &Path, origin: Option<&str>) {
    git(dir, &["init", "-q", "-b", "main"]);
    git(dir, &["commit", "-q", "--allow-empty", "-m", "init"]);
    if let Some(url) = origin {
        git(dir, &["remote", "add", "origin", url]);
    }
}

fn params(dir: &Path, worktree: bool) -> SpawnParams {
    SpawnParams {
        repo_url: dir.to_string_lossy().into_owned(),
        git_ref: "main".into(),
        task: String::new(),
        name_hint: None,
        runtime: None,
        ephemeral: Some(true),
        mcp_initiated: false,
        inject_task: None,
        deliverable_id: None,
        force_new: true,
        worktree,
    }
}

async fn isolated_state(root: &Path) -> Arc<DaemonState> {
    Arc::new(DaemonState::with_root_isolated_managed(root.to_path_buf()).await)
}

/// Why: a local-only record must not carry a made-up GitHub identity that
/// workstream labels or project registration would act on.
/// Test: itself.
#[test]
fn session_source_local_only_carries_no_github_identity() {
    let source = SessionSource::local_only(Path::new("/x/supervisor"));
    assert_eq!(source.source_id, None);
    assert_eq!(source.repo_url, None);
    assert_eq!(source.name, "supervisor");
    let gh = SessionSource::github("acme", "widget");
    assert_eq!(gh.source_id.as_deref(), Some("acme/widget"));
    assert_eq!(
        gh.repo_url.as_deref(),
        Some("https://github.com/acme/widget")
    );
}

/// 🔴 #8934 closure 1 + 3: `spawn_managed` in a repository with no `origin`
/// SUCCEEDS on the checkout itself (before #8934 it returned "managed sessions
/// require a GitHub remote"), and the session's gh is pinned off.
/// Test: itself.
#[tokio::test]
#[serial_test::serial]
async fn a_local_only_repo_spawns_on_its_main_checkout_with_gh_disabled() {
    let (_home, _guard) = scratch_home();
    let data = tempfile::TempDir::new().expect("data root");
    let state = isolated_state(data.path()).await;
    let checkout = tempfile::TempDir::new().expect("checkout");
    repo(checkout.path(), None);

    let record = super::super::lifecycle::spawn_managed(
        &state,
        ManagedSessionId::new(),
        params(checkout.path(), false),
    )
    .await
    .expect("a local-only repo spawns");
    assert_eq!(record.cwd, checkout.path());
    assert_eq!(record.repo_url, None, "no synthetic GitHub URL");
    assert_eq!(record.source_id, None);
    assert!(!checkout.path().join(".worktrees").exists());

    let vars = super::super::lifecycle::resolve_gh_env(&state, checkout.path()).await;
    let config_dir = vars.iter().find(|(k, _)| k == "GH_CONFIG_DIR");
    assert_eq!(
        config_dir.map(|(_, v)| v.as_str()),
        Some(crate::core::remote_mode::LOCAL_ONLY_GH_CONFIG_DIR),
        "{vars:?}"
    );
}

/// #8934 closure 2: an explicit worktree request cuts `session/<name>` from
/// the local default branch (`main` here, also the root's `HEAD`), keeps
/// `.worktrees/` out of `git status`, and arms no upstream.
/// Test: itself.
#[tokio::test]
#[serial_test::serial]
async fn a_local_only_repo_spawns_a_worktree_from_its_local_head() {
    let (_home, _guard) = scratch_home();
    let data = tempfile::TempDir::new().expect("data root");
    let state = isolated_state(data.path()).await;
    let checkout = tempfile::TempDir::new().expect("checkout");
    repo(checkout.path(), None);
    let head = git(checkout.path(), &["rev-parse", "HEAD"]);

    let record = super::super::lifecycle::spawn_managed(
        &state,
        ManagedSessionId::new(),
        params(checkout.path(), true),
    )
    .await
    .expect("a local-only worktree spawns");
    let worktree = record.cwd.clone();
    assert!(
        worktree.starts_with(checkout.path().join(".worktrees")),
        "{worktree:?}"
    );
    assert_eq!(git(&worktree, &["rev-parse", "HEAD"]), head);
    let branch = git(&worktree, &["symbolic-ref", "--short", "HEAD"]);
    assert!(branch.starts_with("session/"), "{branch}");
    let status = git(checkout.path(), &["status", "--porcelain"]);
    assert!(!status.contains(".worktrees"), "{status}");
    let upstream = std::process::Command::new("git")
        .arg("-C")
        .arg(&worktree)
        .args(["rev-parse", "--abbrev-ref", "@{upstream}"])
        .output()
        .expect("git runs");
    assert!(
        !upstream.status.success(),
        "no upstream in a local-only repo"
    );
}

/// #8934: a repository WITH an origin keeps today's routing and gh behaviour —
/// it is never routed local-only, and an unpinned origin still injects nothing.
/// Test: itself.
#[tokio::test]
#[serial_test::serial]
async fn a_repo_with_an_origin_is_not_routed_local_only() {
    let (_home, _guard) = scratch_home();
    let data = tempfile::TempDir::new().expect("data root");
    let state = isolated_state(data.path()).await;
    let checkout = tempfile::TempDir::new().expect("checkout");
    repo(checkout.path(), Some("https://github.com/acme/widget.git"));

    assert_eq!(
        crate::core::remote_mode::is_local_only_root(checkout.path()),
        Ok(false)
    );
    let vars = super::super::lifecycle::resolve_gh_env(&state, checkout.path()).await;
    assert!(vars.is_empty(), "an unpinned origin is unchanged: {vars:?}");
}
