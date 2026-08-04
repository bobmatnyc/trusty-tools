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

// ---------------------------------------------------------------------------
// #4448 review CRITICAL — the exit code alone is not a classifier.
//
// Until this was fixed, ANY non-zero `rev-parse` exit mapped to `NoRepo`, i.e.
// to `Unclaimed`, i.e. to SWEEPABLE. Git exits 128 for every fatal condition,
// so a live work tree git merely declined to read reached the permissive state
// and its committed files were movable. Each test below drives a real git
// failure that is NOT "there is no repository here".
// ---------------------------------------------------------------------------

/// A real repository that git refuses to read. `repositoryformatversion` is the
/// deterministic stand-in for the realistic triggers — `detected dubious
/// ownership` on a checkout owned by another uid, and a `git` shim on `PATH`
/// that fails — all of which exit 128 through this same branch.
#[test]
fn an_unreadable_repo_is_unknown_not_no_repo() {
    let (tmp, tier) = repo_with_mixed_tier();
    assert_eq!(
        VcsIndex::probe(&tier).claim("tracked.md"),
        VcsClaim::Claimed,
        "the fixture must start healthy, or this test proves nothing"
    );

    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(tmp.path())
        .args(["config", "core.repositoryformatversion", "99"])
        .output()
        .expect("spawn git config");
    assert!(out.status.success(), "could not break the repo");

    assert_eq!(
        VcsIndex::probe(&tier).claim("tracked.md"),
        VcsClaim::Unknown,
        "a repository git cannot read must be UNKNOWN — `NoRepo` would make its \
         committed files sweepable"
    );
}

/// A `.git` file whose worktree gitdir is gone. This is the state the ~70
/// worktrees orphaned by the 2026-07-21 `.base` incident were left in.
///
/// It is also the reason the fix cannot match the short phrase `not a git
/// repository`: git emits `fatal: not a git repository: (null)` here, which
/// contains that phrase while meaning the OPPOSITE of absence. Only the
/// parenthesised "searched the parents" form is genuine.
#[test]
fn a_stale_worktree_pointer_is_unknown_not_no_repo() {
    let tmp = TempDir::new().expect("tempdir");
    let tier = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&tier).expect("create tier");
    std::fs::write(
        tmp.path().join(".git"),
        "gitdir: /nonexistent/parent/.git/worktrees/gone\n",
    )
    .expect("write stale pointer");

    assert_eq!(
        VcsIndex::probe(&tier).claim("qa.md"),
        VcsClaim::Unknown,
        "a stale worktree pointer must be UNKNOWN, not NoRepo"
    );
}

/// A bare repository exits 0 printing `false`. It has no work tree, so it can
/// neither confirm nor clear a claim.
#[test]
fn a_bare_repo_is_unknown() {
    let tmp = TempDir::new().expect("tempdir");
    let bare = tmp.path().join("bare.git");
    let out = std::process::Command::new("git")
        .args(["init", "-q", "--bare"])
        .arg(&bare)
        .output()
        .expect("spawn git init --bare");
    assert!(out.status.success(), "could not create a bare repo");

    assert_eq!(VcsIndex::probe(&bare).claim("qa.md"), VcsClaim::Unknown);
}

/// The healthy path still answers, so the fix did not simply freeze the gate.
#[test]
fn a_healthy_repo_still_answers_both_ways() {
    let (_tmp, tier) = repo_with_mixed_tier();
    let index = VcsIndex::probe(&tier);
    assert_eq!(index.claim("tracked.md"), VcsClaim::Claimed);
    assert_eq!(index.claim("loose.md"), VcsClaim::Unclaimed);
}

// ---------------------------------------------------------------------------
// #4448 review round 2 — git's "no repository" message is not PROOF of absence.
//
// Git emits the exact parenthesised NO_REPO_STDERR whenever discovery never got
// far enough to conclude otherwise. In each case below the repository is real
// and its files are committed; before `classify_failure` corroborated the
// message with a filesystem witness, every one of them classified `NoRepo` →
// `Unclaimed` → sweepable.
// ---------------------------------------------------------------------------

/// Restore a path's permissions on drop, so a panicking test cannot leave a
/// mode-000 directory behind for `TempDir` to fail cleaning up.
#[cfg(unix)]
struct ModeGuard(std::path::PathBuf);

#[cfg(unix)]
impl Drop for ModeGuard {
    fn drop(&mut self) {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// TRIGGER 1 — an unreadable `.git` directory. The repository exists and tracks
/// the file; git simply could not open it.
#[test]
#[cfg(unix)]
fn an_unreadable_git_dir_is_unknown_not_no_repo() {
    use std::os::unix::fs::PermissionsExt;
    let (tmp, tier) = repo_with_mixed_tier();
    assert_eq!(
        VcsIndex::probe(&tier).claim("tracked.md"),
        VcsClaim::Claimed,
        "the fixture must start healthy, or this test proves nothing"
    );

    let git_dir = tmp.path().join(".git");
    let _guard = ModeGuard(git_dir.clone());
    std::fs::set_permissions(&git_dir, std::fs::Permissions::from_mode(0o000)).expect("chmod .git");

    assert_eq!(
        VcsIndex::probe(&tier).claim("tracked.md"),
        VcsClaim::Unknown,
        "an unreadable .git must be UNKNOWN — git reports the SAME text it uses \
         for a genuinely absent repository, so the message alone cannot decide"
    );
}

/// TRIGGER 1b — the same message from an unreadable `.git/HEAD`, with the
/// `.git` directory itself perfectly readable.
#[test]
#[cfg(unix)]
fn an_unreadable_git_head_is_unknown_not_no_repo() {
    use std::os::unix::fs::PermissionsExt;
    let (tmp, tier) = repo_with_mixed_tier();
    let head = tmp.path().join(".git").join("HEAD");
    let _guard = ModeGuard(head.clone());
    std::fs::set_permissions(&head, std::fs::Permissions::from_mode(0o000)).expect("chmod HEAD");

    assert_eq!(
        VcsIndex::probe(&tier).claim("tracked.md"),
        VcsClaim::Unknown
    );
}

/// TRIGGER 2 — `GIT_CEILING_DIRECTORIES` stops the upward walk before the
/// `.git`, so git reports absence for a repository that is right there.
///
/// `#[serial]` + a restoring guard: the env var is process-global.
#[test]
#[serial_test::serial]
fn a_ceiling_directory_is_unknown_not_no_repo() {
    let (tmp, tier) = repo_with_mixed_tier();
    assert_eq!(
        VcsIndex::probe(&tier).claim("tracked.md"),
        VcsClaim::Claimed,
        "the fixture must start healthy, or this test proves nothing"
    );

    struct EnvGuard(Option<std::ffi::OsString>);
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: this test is `#[serial]`, so no other thread races the
            // set/restore of this process-global variable.
            unsafe {
                match self.0.take() {
                    Some(prev) => std::env::set_var("GIT_CEILING_DIRECTORIES", prev),
                    None => std::env::remove_var("GIT_CEILING_DIRECTORIES"),
                }
            }
        }
    }
    let _guard = EnvGuard(std::env::var_os("GIT_CEILING_DIRECTORIES"));
    // SAFETY: as above.
    unsafe {
        std::env::set_var("GIT_CEILING_DIRECTORIES", tmp.path());
    }

    assert_eq!(
        VcsIndex::probe(&tier).claim("tracked.md"),
        VcsClaim::Unknown,
        "a ceiling-blocked discovery must be UNKNOWN — the repository and its \
         committed files are still there"
    );
}

/// A stray empty `.git` DIRECTORY is not a repository, and git says so with the
/// genuine message — but this refuses anyway. A deliberate over-refusal:
/// fail-closed, and visible in the report as `VcsUnknown` rather than silent.
#[test]
fn a_stray_empty_git_dir_is_unknown() {
    let tmp = TempDir::new().expect("tempdir");
    let tier = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&tier).expect("create tier");
    std::fs::create_dir_all(tmp.path().join(".git")).expect("create stray .git");

    assert_eq!(VcsIndex::probe(&tier).claim("qa.md"), VcsClaim::Unknown);
}

/// A path that cannot be canonicalised is `Unknown`, never `NoRepo`.
///
/// This closes a hole the ancestor walk would otherwise have: a RELATIVE `dir`
/// walks a truncated chain (`.claude/agents` → `.claude` → `""`), misses the
/// project's `.git`, and lands back on the permissive answer. Not reachable
/// from today's call sites, which pass absolute paths — but one caller away,
/// and it fails in the dangerous direction.
#[test]
fn an_unresolvable_relative_path_is_unknown() {
    let relative = std::path::Path::new("no/such/relative/.claude/agents");
    assert_eq!(
        classify_failure(relative, NO_REPO_STDERR),
        IndexState::Unavailable
    );
}

/// The genuine case still resolves to `NoRepo`, so the corroboration did not
/// simply freeze the gate into always-refusing.
#[test]
fn a_truly_empty_directory_is_still_no_repo() {
    let tmp = TempDir::new().expect("tempdir");
    let tier = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&tier).expect("create tier");
    assert_eq!(classify_failure(&tier, NO_REPO_STDERR), IndexState::NoRepo);
    assert_eq!(VcsIndex::probe(&tier).claim("qa.md"), VcsClaim::Unclaimed);
}

/// A non-matching stderr never reaches the filesystem witness at all.
#[test]
fn a_non_matching_stderr_is_unavailable_without_stat() {
    assert_eq!(
        classify_failure(
            std::path::Path::new("/"),
            "fatal: detected dubious ownership in repository at '/x'"
        ),
        IndexState::Unavailable
    );
}

/// M40 — the witness must be `symlink_metadata`, not `metadata`.
///
/// A DANGLING `.git` symlink: git reports the parenthesised "no repository"
/// message, `lstat` succeeds, and `stat` fails. Following the link would
/// conclude "nothing is there" and hand back the sweepable answer for a project
/// whose `.git` is merely broken. The link's existence is the signal — refuse.
#[test]
#[cfg(unix)]
fn a_dangling_git_symlink_is_unknown_not_no_repo() {
    let tmp = TempDir::new().expect("tempdir");
    let tier = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&tier).expect("create tier");
    std::os::unix::fs::symlink("/nonexistent/gone", tmp.path().join(".git"))
        .expect("dangling .git symlink");

    // The premise: lstat sees it, stat does not.
    assert!(tmp.path().join(".git").symlink_metadata().is_ok());
    assert!(tmp.path().join(".git").metadata().is_err());

    assert_eq!(
        VcsIndex::probe(&tier).claim("qa.md"),
        VcsClaim::Unknown,
        "a broken .git symlink must refuse, not read as an absent repository"
    );
}

/// M42 — the constant must keep the parenthesised form even though the ancestor
/// witness now covers the stale-worktree case.
///
/// A bogus `GIT_DIR` in a directory with NO `.git` anywhere emits
/// `fatal: not a git repository: '/nonexistent/x'` — it matches the SHORT
/// phrase, and the filesystem witness finds nothing to contradict it. So the
/// two defences do not overlap here: shortening `NO_REPO_STDERR` would hand
/// back `NoRepo` for a directory whose repository was merely misconfigured.
#[test]
fn a_bogus_git_dir_message_is_unavailable_even_with_no_ancestor_git() {
    let tmp = TempDir::new().expect("tempdir");
    let tier = tmp.path().join(".claude").join("agents");
    std::fs::create_dir_all(&tier).expect("create tier");
    assert!(
        !tier.ancestors().any(|p| p.join(".git").exists()),
        "the fixture must have NO ancestor .git, or the witness masks the phrase"
    );

    assert_eq!(
        classify_failure(&tier, "fatal: not a git repository: '/nonexistent/x'"),
        IndexState::Unavailable,
        "only the parenthesised form means genuine absence"
    );
}
