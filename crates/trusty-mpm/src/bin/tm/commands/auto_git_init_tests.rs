//! Tests for the auto-`git init` decision and driver (#6274).
//!
//! Why: the decision has four arms an operator can hit on their first ever
//! `tm` run — initialize, leave alone, refuse, prerequisite error — and three
//! of them write (or deliberately do not write) to a real directory.
//! What: pure-decision tests for [`super::plan_auto_init`],
//! [`super::stderr_means_no_repository`] and the message builders, plus
//! driver tests that run real git against hermetic temp directories.
//! Test: this file.

use std::path::{Path, PathBuf};
use std::process::Command;

use super::{
    AutoInitOutcome, AutoInitPlan, AutoInitRefusal, RepoContext, ensure_git_repo_with,
    initialized_message, missing_git_error, plan_auto_init, refusal_message,
    stderr_means_no_repository,
};
use crate::test_support::hermetic_temp_dir;

/// A program name no PATH entry can resolve, for the prerequisite arm.
const ABSENT_GIT: &str = "tm-test-no-such-git-executable-6274";

/// Run a real git command that must succeed, for fixture setup.
fn git_ok(dir: &Path, args: &[&str]) {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .expect("git must be installed to run these tests");
    assert!(
        out.status.success(),
        "git {args:?} failed in {}: {}",
        dir.display(),
        String::from_utf8_lossy(&out.stderr)
    );
}

/// `git rev-parse --show-toplevel` from `dir`, or `None` when there is none.
fn toplevel(dir: &Path) -> Option<PathBuf> {
    let out = Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["rev-parse", "--show-toplevel"])
        .output()
        .ok()
        .filter(|o| o.status.success())?;
    Some(PathBuf::from(
        String::from_utf8_lossy(&out.stdout).trim().to_owned(),
    ))
}

// ── The pure decision ────────────────────────────────────────────────────────

/// The feature itself: a plain directory that is nobody's home earns an init.
#[test]
fn plan_initializes_a_plain_directory() {
    let plan = plan_auto_init(
        Path::new("/Users/someone/scratch/notes"),
        Some(Path::new("/Users/someone")),
        RepoContext::Absent,
    );
    assert_eq!(plan, AutoInitPlan::Init);
}

/// `$HOME` never gets a repository it did not already have — a `.git` there
/// silently adopts every descendant directory as part of one repo.
#[test]
fn plan_refuses_the_home_directory() {
    let home = Path::new("/Users/someone");
    assert_eq!(
        plan_auto_init(home, Some(home), RepoContext::Absent),
        AutoInitPlan::Refuse(AutoInitRefusal::HomeDirectory)
    );
}

/// Same for `/`, which has no parent.
#[test]
fn plan_refuses_the_filesystem_root() {
    assert_eq!(
        plan_auto_init(
            Path::new("/"),
            Some(Path::new("/Users/someone")),
            RepoContext::Absent
        ),
        AutoInitPlan::Refuse(AutoInitRefusal::FilesystemRoot)
    );
}

/// The guards must not fire on a directory that ALREADY is a repository: an
/// operator whose home directory is a dotfiles repo keeps today's behavior.
#[test]
fn plan_leaves_an_existing_repo_alone_even_in_the_home_directory() {
    let home = Path::new("/Users/someone");
    assert_eq!(
        plan_auto_init(home, Some(home), RepoContext::Present),
        AutoInitPlan::AlreadyGit
    );
}

/// With no resolvable home directory the plain-directory case still initializes.
#[test]
fn plan_initializes_when_the_home_directory_is_unknown() {
    assert_eq!(
        plan_auto_init(Path::new("/srv/project"), None, RepoContext::Absent),
        AutoInitPlan::Init
    );
}

// ── The "is there a repository here" stderr discrimination ───────────────────

/// The long phrase is the only thing that means "no repository".
#[test]
fn no_repo_stderr_matches_the_long_phrase() {
    assert!(stderr_means_no_repository(
        "fatal: not a git repository (or any of the parent directories): .git"
    ));
}

/// A stale worktree pointer contains the SHORT phrase while meaning the
/// opposite — matching it would `git init` over a broken repository.
#[test]
fn no_repo_stderr_rejects_the_stale_worktree_pointer() {
    assert!(!stderr_means_no_repository(
        "fatal: not a git repository: (null)"
    ));
}

/// Any other failure (dubious ownership, permissions) is not "no repository".
#[test]
fn no_repo_stderr_rejects_an_unrelated_failure() {
    assert!(!stderr_means_no_repository(
        "fatal: detected dubious ownership in repository at '/srv/project'"
    ));
}

// ── Operator-facing messages ─────────────────────────────────────────────────

/// The notice says where the repository was created.
#[test]
fn initialized_message_names_the_directory() {
    let msg = initialized_message(Path::new("/srv/project"));
    assert_eq!(msg, "tm: initialized git in /srv/project");
}

/// A refusal states its reason, not just that it refused.
#[test]
fn refusal_message_names_the_home_directory() {
    let msg = refusal_message(AutoInitRefusal::HomeDirectory, Path::new("/Users/someone"));
    assert!(msg.contains("/Users/someone"), "{msg}");
    assert!(msg.contains("home directory"), "{msg}");
}

/// Same for the filesystem-root arm.
#[test]
fn refusal_message_names_the_filesystem_root() {
    let msg = refusal_message(AutoInitRefusal::FilesystemRoot, Path::new("/"));
    assert!(msg.contains("filesystem root"), "{msg}");
}

/// The prerequisite error names `git` — this feature never installs the binary.
#[test]
fn missing_git_error_names_git_and_the_directory() {
    let msg = missing_git_error(Path::new("/srv/project")).to_string();
    assert!(msg.contains("`git` was not found on PATH"), "{msg}");
    assert!(msg.contains("/srv/project"), "{msg}");
}

// ── The driver, against real git ─────────────────────────────────────────────

/// The headline case: a plain directory becomes a repository rooted at itself.
#[test]
fn auto_init_initializes_a_plain_directory() {
    let tmp = hermetic_temp_dir();
    let dir = tmp.path().join("fresh-project");
    std::fs::create_dir(&dir).unwrap();

    let outcome = ensure_git_repo_with(&dir, Some(tmp.path()), "git").unwrap();

    assert_eq!(outcome, AutoInitOutcome::Initialized);
    assert!(dir.join(".git").is_dir(), "a .git directory must exist now");
    assert_eq!(
        toplevel(&dir).map(|p| p.canonicalize().unwrap()),
        Some(dir.canonicalize().unwrap()),
        "the repository root must be the invocation directory, not a parent"
    );
}

/// Regression for the pre-fix refusal path: the directory `tm` used to answer
/// "not in a git project" for is, after this call, exactly what
/// `classify_cwd_project` calls a usable project — with no origin remote.
#[test]
fn non_git_directory_now_classifies_as_a_usable_project() {
    use crate::commands::guided::{CwdProject, classify_cwd_project};

    let tmp = hermetic_temp_dir();
    let dir = tmp.path().join("plain");
    std::fs::create_dir(&dir).unwrap();
    assert!(
        matches!(classify_cwd_project(&dir), CwdProject::NotGit),
        "precondition: this is the directory tm used to refuse"
    );

    ensure_git_repo_with(&dir, Some(tmp.path()), "git").unwrap();

    assert!(
        matches!(classify_cwd_project(&dir), CwdProject::Usable(_)),
        "after auto-init tm must see an ordinary project here"
    );
    assert_eq!(
        trusty_mpm::daemon::managed_routes::inproject::get_origin_url(&dir).unwrap(),
        None,
        "a freshly initialized repo has no origin — the no-remote flow takes over"
    );
}

/// An existing repository root is left exactly as it was.
#[test]
fn auto_init_leaves_a_repo_root_alone() {
    let tmp = hermetic_temp_dir();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_ok(&repo, &["init"]);

    let outcome = ensure_git_repo_with(&repo, Some(tmp.path()), "git").unwrap();

    assert_eq!(outcome, AutoInitOutcome::AlreadyGit);
}

/// A directory INSIDE another repository's work tree — untracked, so it is not
/// part of that repo's tree — still must not get a nested repository: git is
/// already present there.
#[test]
fn auto_init_does_not_init_inside_another_work_tree() {
    let tmp = hermetic_temp_dir();
    let repo = tmp.path().join("repo");
    std::fs::create_dir(&repo).unwrap();
    git_ok(&repo, &["init"]);
    let nested = repo.join("untracked-notes");
    std::fs::create_dir(&nested).unwrap();

    let outcome = ensure_git_repo_with(&nested, Some(tmp.path()), "git").unwrap();

    assert_eq!(outcome, AutoInitOutcome::AlreadyGit);
    assert!(
        !nested.join(".git").exists(),
        "a nested repository must not be created inside another work tree"
    );
}

/// A bare repository has no work tree, so a `--show-toplevel` check would call
/// it a plain directory and re-initialize it. `--git-dir` does not.
#[test]
fn auto_init_leaves_a_bare_repository_alone() {
    let tmp = hermetic_temp_dir();
    let bare = tmp.path().join("origin.git");
    std::fs::create_dir(&bare).unwrap();
    git_ok(&bare, &["init", "--bare"]);

    let outcome = ensure_git_repo_with(&bare, Some(tmp.path()), "git").unwrap();

    assert_eq!(outcome, AutoInitOutcome::AlreadyGit);
}

/// The home-directory guard, end to end: nothing is written.
#[test]
fn auto_init_refuses_the_home_directory() {
    let tmp = hermetic_temp_dir();
    let home = tmp.path().join("home");
    std::fs::create_dir(&home).unwrap();

    let outcome = ensure_git_repo_with(&home, Some(&home), "git").unwrap();

    assert_eq!(
        outcome,
        AutoInitOutcome::Refused(AutoInitRefusal::HomeDirectory)
    );
    assert!(
        !home.join(".git").exists(),
        "the home directory must not become a repository"
    );
}

/// No git executable: a prerequisite error naming git, and nothing written.
#[test]
fn ensure_reports_a_missing_git_executable_without_writing() {
    let tmp = hermetic_temp_dir();
    let dir = tmp.path().join("plain");
    std::fs::create_dir(&dir).unwrap();

    let err = ensure_git_repo_with(&dir, Some(tmp.path()), ABSENT_GIT)
        .expect_err("a missing git executable must be an error, not a silent skip");

    assert!(
        err.to_string().contains("`git` was not found on PATH"),
        "{err}"
    );
    assert!(
        !dir.join(".git").exists(),
        "nothing may be written when git is unavailable"
    );
}

/// A path that is not a directory is the caller's error to report; this call
/// stays silent and writes nothing.
#[test]
fn auto_init_skips_a_path_that_is_not_a_directory() {
    let tmp = hermetic_temp_dir();
    let missing = tmp.path().join("does-not-exist");

    let outcome = ensure_git_repo_with(&missing, Some(tmp.path()), "git").unwrap();

    assert_eq!(outcome, AutoInitOutcome::NotADirectory);
    assert!(!missing.exists(), "the path must not be created");
}
