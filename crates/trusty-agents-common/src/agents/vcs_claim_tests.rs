//! Tests for the #4448 VCS-claim gate.
//!
//! These drive REAL `git` against real temporary repositories rather than a
//! fabricated tracked-set, because the gate's whole value is that it reflects
//! what git actually reports. The only fabricated state is
//! [`IndexState::Unavailable`], which cannot be produced without removing git
//! from the machine — the tests reach it by constructing the private enum,
//! which is why no public constructor for it exists.

use super::*;
use tempfile::TempDir;

/// `git init` a directory and return it. Panics loudly: a test that cannot set
/// up a repository has proven nothing and must not pass.
fn init_repo(dir: &std::path::Path) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init", "-q"])
        .output()
        .expect("git must be available to run the VCS-claim tests");
    assert!(
        out.status.success(),
        "git init failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `git add` a path. Only the INDEX is needed — `ls-files` reads it, so no
/// commit (and no `user.email` config) is required.
fn git_add(repo: &std::path::Path, rel: &str) {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(repo)
        .args(["add", "--force", rel])
        .output()
        .expect("spawn git add");
    assert!(
        out.status.success(),
        "git add {rel} failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

/// Stage a tier holding one tracked and one untracked agent, returning the
/// tier directory. `--force` on the add is deliberate: a project may well
/// gitignore `.claude/agents/`, and the point of the test is that an EXPLICIT
/// track still claims the file.
fn repo_with_mixed_tier() -> (TempDir, std::path::PathBuf) {
    let tmp = TempDir::new().expect("tempdir");
    init_repo(tmp.path());
    let tier = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&tier).expect("create tier");
    std::fs::write(tier.join("tracked.md"), "---\nname: tracked\n---\n").expect("write tracked");
    std::fs::write(tier.join("loose.md"), "---\nname: loose\n---\n").expect("write loose");
    git_add(tmp.path(), ".claude/agents/tracked.md");
    (tmp, tier)
}

#[test]
fn probe_finds_tracked_files() {
    let (_tmp, tier) = repo_with_mixed_tier();
    let index = VcsIndex::probe(&tier);
    assert_eq!(index.claim("tracked.md"), VcsClaim::Claimed);
}

#[test]
fn claim_of_an_untracked_file() {
    let (_tmp, tier) = repo_with_mixed_tier();
    let index = VcsIndex::probe(&tier);
    assert_eq!(index.claim("loose.md"), VcsClaim::Unclaimed);
}

/// A name that does not exist at all is unclaimed, not `Unknown` — the repo
/// answered, and its answer was "not mine".
#[test]
fn claim_of_an_absent_file() {
    let (_tmp, tier) = repo_with_mixed_tier();
    let index = VcsIndex::probe(&tier);
    assert_eq!(index.claim("never-existed.md"), VcsClaim::Unclaimed);
}

/// No repository means nothing can be claiming the file — the sweep must still
/// be able to run in a project that does not use git at all.
#[test]
fn claim_outside_a_repo_is_unclaimed() {
    let tmp = TempDir::new().expect("tempdir");
    let tier = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&tier).expect("create tier");
    std::fs::write(tier.join("qa.md"), "---\nname: qa\n---\n").expect("write");
    let index = VcsIndex::probe(&tier);
    assert_eq!(index.claim("qa.md"), VcsClaim::Unclaimed);
}

/// A tracked file NESTED below the tier can never be confused with a flat one.
#[test]
fn probe_drops_nested_entries() {
    let tmp = TempDir::new().expect("tempdir");
    init_repo(tmp.path());
    let tier = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(tier.join("sub")).expect("create sub");
    std::fs::write(tier.join("sub").join("qa.md"), "---\nname: qa\n---\n").expect("write");
    git_add(tmp.path(), ".claude/agents/sub/qa.md");

    let index = VcsIndex::probe(&tier);
    assert_eq!(
        index.claim("qa.md"),
        VcsClaim::Unclaimed,
        "`sub/qa.md` must not claim the flat name `qa.md`"
    );
}

/// The fail-closed state. When git cannot be consulted the answer is `Unknown`,
/// never `Unclaimed` — the caller then refuses to move rather than guessing.
#[test]
fn claim_is_unknown_when_git_is_unavailable() {
    let index = VcsIndex {
        state: IndexState::Unavailable,
    };
    assert_eq!(index.claim("qa.md"), VcsClaim::Unknown);
    assert_eq!(index.claim("anything.md"), VcsClaim::Unknown);
}

/// Probing a directory that does not exist must not panic; it degrades to a
/// no-conclusion or no-repo answer, both of which the caller handles.
#[test]
fn probe_of_a_missing_directory_does_not_panic() {
    let tmp = TempDir::new().expect("tempdir");
    let index = VcsIndex::probe(&tmp.path().join("nope").join("agents"));
    assert_ne!(
        index.claim("qa.md"),
        VcsClaim::Claimed,
        "a directory that does not exist can hold no tracked file"
    );
}
