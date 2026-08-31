//! Tests for the stale-install guard (#4462).
//!
//! Why: the guard's whole value is that it refuses on positive evidence and
//! only then — a false refusal blocks a legitimate dev-loop install, and a
//! missed one puts the shared global binary back on a stale build.
//! What: real temp git repositories for the probe (a comparison against a
//! fabricated ref proves nothing about what git actually answers), plus pure
//! decision tests for the refusal.
//! Test: this file IS the test module.

use std::path::Path;
use std::process::Command;

use super::*;

/// Run a git command in `dir`, returning whether it succeeded.
fn git(dir: &Path, args: &[&str]) -> bool {
    Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

/// A repository with two commits and an `origin/main` remote-tracking ref
/// pointing at the SECOND one. Returns the temp dir and the first commit's sha.
///
/// Why: `refs/remotes/origin/main` is what `merge-base --is-ancestor` reads,
/// and it is writable directly — no network, no second repository.
fn repo_with_origin_main_ahead() -> Option<(tempfile::TempDir, String)> {
    let dir = tempfile::tempdir().ok()?;
    let p = dir.path();
    if !git(p, &["init", "--quiet", "--initial-branch", "main"]) {
        return None;
    }
    git(p, &["config", "user.email", "t@example.com"]);
    git(p, &["config", "user.name", "t"]);
    std::fs::write(p.join("a.txt"), "a").ok()?;
    git(p, &["add", "-A"]);
    if !git(p, &["commit", "--quiet", "-m", "first"]) {
        return None;
    }
    let first = Command::new("git")
        .arg("-C")
        .arg(p)
        .args(["rev-parse", "HEAD"])
        .output()
        .ok()?;
    let first = String::from_utf8_lossy(&first.stdout).trim().to_string();
    std::fs::write(p.join("b.txt"), "b").ok()?;
    git(p, &["add", "-A"]);
    if !git(p, &["commit", "--quiet", "-m", "second"]) {
        return None;
    }
    // origin/main = the second commit.
    if !git(p, &["update-ref", "refs/remotes/origin/main", "HEAD"]) {
        return None;
    }
    Some((dir, first))
}

/// A source whose HEAD IS `origin/main` installs without complaint.
#[test]
fn head_at_origin_main_is_current() {
    let Some((dir, _first)) = repo_with_origin_main_ahead() else {
        eprintln!("git unavailable; skipping");
        return;
    };
    assert_eq!(
        probe_source_freshness(dir.path()),
        SourceFreshness::Current,
        "HEAD == origin/main must read as current"
    );
}

/// The #4462 case: a worktree parked on an older commit than `origin/main`.
#[test]
fn head_behind_origin_main_is_behind() {
    let Some((dir, first)) = repo_with_origin_main_ahead() else {
        eprintln!("git unavailable; skipping");
        return;
    };
    assert!(
        git(dir.path(), &["checkout", "--quiet", &first]),
        "checkout of the first commit must succeed"
    );
    assert_eq!(
        probe_source_freshness(dir.path()),
        SourceFreshness::Behind,
        "a HEAD that origin/main is not an ancestor of must read as behind"
    );
}

/// Not a repository: the guard learns nothing and must not block.
#[test]
fn a_directory_that_is_not_a_repo_is_unknown() {
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(matches!(
        probe_source_freshness(dir.path()),
        SourceFreshness::Unknown(_)
    ));
}

#[test]
fn a_behind_source_is_refused() {
    let reason = stale_install_refusal(Path::new("/w/stale"), &SourceFreshness::Behind, false)
        .expect("a behind source must be refused");
    assert!(reason.contains("/w/stale"), "{reason}");
    assert!(reason.contains(ALLOW_STALE_INSTALL_ENV), "{reason}");
}

#[test]
fn the_override_permits_a_behind_source() {
    assert!(
        stale_install_refusal(Path::new("/w/stale"), &SourceFreshness::Behind, true).is_none(),
        "the documented override must permit a deliberate stale install"
    );
}

/// Deny on positive evidence only — an undeterminable source still installs.
#[test]
fn an_unknown_source_is_permitted() {
    assert!(
        stale_install_refusal(
            Path::new("/w/mystery"),
            &SourceFreshness::Unknown("not a git repository".into()),
            false
        )
        .is_none()
    );
    assert!(
        stale_install_refusal(Path::new("/w/fresh"), &SourceFreshness::Current, false).is_none()
    );
}
