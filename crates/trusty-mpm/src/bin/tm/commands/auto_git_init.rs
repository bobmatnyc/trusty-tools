//! Auto-`git init` when `tm` is invoked in a directory that is not a git
//! repository (#6274).
//!
//! Why: `tm` used to stop at the front door of a plain directory. Bare `tm`
//! printed [`super::misc::NON_GIT_FALLBACK_HINT`] ("not in a git project") and
//! did nothing; `tm launch` failed with "no git origin remote found". The
//! operator's next move was always the same — run `git init`, re-run `tm`. This
//! module takes that step for them and then lets the ordinary flow continue
//! exactly as it does for a git repository with no origin remote — which since
//! #6276 means a working session in that checkout, not a second refusal (see
//! [`super::origin_plan`]).
//! What: [`ensure_git_repo`] probes for an existing repository, applies the
//! sanity guards, runs `git init`, and prints a one-line notice. The decision
//! is the pure [`plan_auto_init`]; the "is there a repository here" stderr
//! discrimination is the pure [`stderr_means_no_repository`]. Nothing here
//! installs the `git` executable — a missing `git` is a prerequisite error.
//! #7749: the upward probe is not enough. A directory with a repository BENEATH
//! it is a workspace parent, and a `.git` written here becomes an ancestor
//! repository for every project under it, so the guards also run the downward
//! [`trusty_mpm::core::child_repo_scan::scan_for_child_repo`] — the same scan
//! the `CLAUDE.md` seed guard uses — and refuse on a child repository or on a
//! scan that could not finish.
//! Test: `auto_git_init_tests.rs`.

use std::path::{Path, PathBuf};

use trusty_mpm::core::child_repo_scan::{ChildRepoScan, ScanIncomplete, scan_for_child_repo};

/// The git executable this module drives.
const GIT_PROGRAM: &str = "git";

/// The stderr phrase that means git found no repository at or above a
/// directory.
///
/// Why: the SHORT phrase `not a git repository` is ambiguous in this codebase —
/// git answers `fatal: not a git repository: (null)` for a STALE WORKTREE
/// POINTER, which is a broken repository rather than no repository at all.
/// Matching the long phrase is the same discrimination
/// `trusty_agents_common::agents::vcs_claim` and `trusty_review::report::scan`
/// already make. Anything else that fails is reported, never auto-initialized.
const NO_REPO_STDERR: &str = "not a git repository (or any of the parent directories)";

/// Whether git already has a repository for the directory.
///
/// Why: "is this a repo" must catch MORE than a work tree — a bare repository
/// answers `rev-parse --show-toplevel` with a failure while being very much a
/// repository, and `git init` there re-initializes it. `rev-parse --git-dir`
/// answers the broader question once: a work tree at any depth, a bare repo, or
/// the inside of a `.git` directory all report `Present`.
/// Test: `auto_init_leaves_a_repo_root_alone`,
/// `auto_init_does_not_init_inside_another_work_tree`,
/// `auto_init_leaves_a_bare_repository_alone`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum RepoContext {
    /// Git reports a repository for this directory.
    Present,
    /// Git reports no repository at or above this directory.
    Absent,
}

/// Why a sanity guard refused to create a repository.
///
/// Why: `git init` writes a `.git` directory that git then treats as a
/// project root for every descendant. In `$HOME` or at `/` that is a
/// long-lived mistake nobody asked for, so those two directories refuse
/// rather than initialize. #7749: a workspace parent is the same mistake one
/// level down — the `.git` adopts every child project instead of every sibling
/// directory — and a scan that could not finish cannot rule one out.
/// Test: `plan_refuses_the_home_directory`, `plan_refuses_the_filesystem_root`,
/// `plan_refuses_a_workspace_parent`, `plan_refuses_an_unfinished_scan`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AutoInitRefusal {
    /// The directory is the operator's home directory.
    HomeDirectory,
    /// The directory is the filesystem root.
    FilesystemRoot,
    /// #7749: a git repository lives beneath the directory, which makes it a
    /// workspace parent. Carries the child repository the scan found.
    WorkspaceParent(PathBuf),
    /// #7749: the downward scan stopped before checking everything, so a child
    /// repository cannot be ruled out. Fails closed — nothing is initialized.
    ScanIncomplete(ScanIncomplete),
}

/// The decision [`ensure_git_repo`] acts on.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AutoInitPlan {
    /// A repository is already here — do nothing.
    AlreadyGit,
    /// No repository and no guard fired — run `git init`.
    Init,
    /// No repository, but a guard refused to create one.
    Refuse(AutoInitRefusal),
}

/// What [`ensure_git_repo`] actually did.
#[derive(Debug, PartialEq, Eq)]
pub(crate) enum AutoInitOutcome {
    /// The directory already had a repository; nothing was written.
    AlreadyGit,
    /// `git init` ran here.
    Initialized,
    /// A sanity guard refused; the reason was printed to stderr.
    Refused(AutoInitRefusal),
    /// The path is not an existing directory, so there is nothing to
    /// initialize. The caller's own resolution error is the better message,
    /// so this arm stays silent and writes nothing.
    NotADirectory,
}

/// Does a failed `git rev-parse` stderr mean "there is no repository here"?
///
/// Why: see [`NO_REPO_STDERR`] — a substring test on the short phrase also
/// matches a BROKEN repository, and auto-initializing over one of those is the
/// worst thing this module could do. Fails closed: an unrecognised failure is
/// not "no repository".
/// What: `true` only when the long phrase appears in `stderr`.
/// Test: `no_repo_stderr_matches_the_long_phrase`,
/// `no_repo_stderr_rejects_the_stale_worktree_pointer`.
pub(crate) fn stderr_means_no_repository(stderr: &str) -> bool {
    stderr.contains(NO_REPO_STDERR)
}

/// Decide what to do about `dir`, given git's verdict, the home directory, and
/// a downward scan for child repositories.
///
/// Why: the whole decision, free of process spawning and filesystem access, so
/// every arm is asserted directly. `context` is checked FIRST so a home
/// directory (or `/`) that IS already a repository keeps behaving exactly as it
/// does today — the guards only ever prevent a NEW repository.
/// What: `AlreadyGit` when git reports a repository; `Refuse` when `dir` is the
/// filesystem root or the home directory; `Init` otherwise. #7749: `Init` is
/// reached only through `scan`, which is `Clear` exactly when a completed walk
/// found no repository beneath `dir` — `Found` and `Incomplete` both refuse, so
/// a workspace parent never gets a `.git` and an unfinished walk never reads as
/// absence. `scan` is a closure, so the walk costs nothing on the arms decided
/// before it. `dir` and `home` are expected canonicalized by the caller so the
/// comparison is not defeated by a symlinked `$HOME`.
/// Test: `plan_initializes_a_plain_directory`, `plan_refuses_the_home_directory`,
/// `plan_refuses_the_filesystem_root`,
/// `plan_leaves_an_existing_repo_alone_even_in_the_home_directory`,
/// `plan_refuses_a_workspace_parent`, `plan_refuses_an_unfinished_scan`,
/// `plan_does_not_scan_when_an_earlier_guard_already_decided`.
pub(crate) fn plan_auto_init(
    dir: &Path,
    home: Option<&Path>,
    context: RepoContext,
    scan: impl FnOnce() -> ChildRepoScan,
) -> AutoInitPlan {
    if context == RepoContext::Present {
        return AutoInitPlan::AlreadyGit;
    }
    if dir.parent().is_none() {
        return AutoInitPlan::Refuse(AutoInitRefusal::FilesystemRoot);
    }
    if home == Some(dir) {
        return AutoInitPlan::Refuse(AutoInitRefusal::HomeDirectory);
    }
    // #7749: a `.git` here would become an ancestor repository for every child
    // project, so only a scan that finished and found nothing may initialize.
    match scan() {
        ChildRepoScan::Clear => AutoInitPlan::Init,
        ChildRepoScan::Found(child) => {
            AutoInitPlan::Refuse(AutoInitRefusal::WorkspaceParent(child))
        }
        ChildRepoScan::Incomplete(stop) => {
            AutoInitPlan::Refuse(AutoInitRefusal::ScanIncomplete(stop))
        }
    }
}

/// The one-line notice printed after a successful `git init`.
///
/// Test: `initialized_message_names_the_directory`.
pub(crate) fn initialized_message(dir: &Path) -> String {
    format!("tm: initialized git in {}", dir.display())
}

/// The stderr notice printed when a sanity guard refuses.
///
/// Why: a refusal that does not say WHY reads as a malfunction. Each arm names
/// the directory and the specific reason it is not a project. #7749: the two
/// scan arms carry what the scan learned — the child repository, or what
/// stopped the walk — so the operator can act on it rather than re-deriving it.
/// #7749 review: they also carry their OWN remedy. "Run tm from a project
/// directory" is right for `$HOME` and `/`, and wrong for a scan arm — tm was
/// run from a directory the operator considers a project, and
/// [`trusty_mpm::core::child_repo_scan::WORKSPACE_SCAN_BUDGET`] is 256
/// directories, so a sizeable plain directory hits
/// [`AutoInitRefusal::ScanIncomplete`] routinely. Both scan arms give the
/// remedy `SeedRefusal::ScanIncomplete` gives.
/// Test: `refusal_message_names_the_home_directory`,
/// `refusal_message_names_the_filesystem_root`,
/// `refusal_message_names_the_child_repository`,
/// `refusal_message_says_what_stopped_the_scan`.
pub(crate) fn refusal_message(refusal: &AutoInitRefusal, dir: &Path) -> String {
    // #7749: a directory tm cannot rule out is still one the operator can
    // settle themselves, so the scan arms point at `git init` here first and
    // at the actual project directory second.
    let scan_remedy = format!(
        "If {dir} is a single project, run `git init` there yourself and retry; otherwise run \
         tm from the actual project directory.",
        dir = dir.display()
    );
    const SITE_REMEDY: &str = "Run tm from a project directory.";
    let (reason, remedy) = match refusal {
        AutoInitRefusal::HomeDirectory => (
            "that is your home directory, not a project".to_string(),
            SITE_REMEDY.to_string(),
        ),
        AutoInitRefusal::FilesystemRoot => (
            "that is the filesystem root, not a project".to_string(),
            SITE_REMEDY.to_string(),
        ),
        AutoInitRefusal::WorkspaceParent(child) => (
            format!(
                "{} is a git repository beneath it, so a .git here would become an ancestor \
                 repository for that project and every other one under this directory",
                child.display()
            ),
            scan_remedy,
        ),
        AutoInitRefusal::ScanIncomplete(stop) => (
            format!("tm could not rule out git repositories beneath it ({stop})"),
            scan_remedy,
        ),
    };
    format!(
        "tm: not initializing git in {} — {reason}. {remedy}",
        dir.display()
    )
}

/// The prerequisite error for a missing `git` executable.
///
/// Why: the owner directive is "install git in the directory" — a repository,
/// not the git binary. When `git` itself is absent there is nothing this
/// module can do, and the error has to name git so the operator's next move is
/// obvious. Nothing has been written at the point this fires.
/// Test: `missing_git_error_names_git_and_the_directory`,
/// `ensure_reports_a_missing_git_executable_without_writing`.
pub(crate) fn missing_git_error(dir: &Path) -> anyhow::Error {
    anyhow::anyhow!(
        "`git` was not found on PATH — tm needs it to work in '{}'. \
         Install git and re-run; nothing in that directory was changed.",
        dir.display()
    )
}

/// Run `git -C <dir> <args>` and capture its output.
///
/// Why: both git calls in this module need the same spawn-failure mapping, and
/// a `NotFound` spawn error is the prerequisite case rather than a git failure.
/// `program` is a parameter purely so the missing-executable path is testable
/// without mutating `PATH` for the whole test binary.
/// Test: `ensure_reports_a_missing_git_executable_without_writing`.
fn run_git(program: &str, dir: &Path, args: &[&str]) -> anyhow::Result<std::process::Output> {
    std::process::Command::new(program)
        .arg("-C")
        .arg(dir)
        .args(args)
        .output()
        .map_err(|e| {
            if e.kind() == std::io::ErrorKind::NotFound {
                missing_git_error(dir)
            } else {
                anyhow::anyhow!(
                    "could not run `git {}` in '{}': {e}",
                    args.join(" "),
                    dir.display()
                )
            }
        })
}

/// Ask git whether `dir` already has a repository.
///
/// What: `git -C <dir> rev-parse --git-dir`. Success is [`RepoContext::Present`];
/// a failure whose stderr carries [`NO_REPO_STDERR`] is
/// [`RepoContext::Absent`]; any other failure is an `Err` carrying git's own
/// stderr, so an unreadable repository is reported rather than initialized over.
/// Test: `auto_init_leaves_a_bare_repository_alone`,
/// `auto_init_initializes_a_plain_directory`.
fn repo_context(program: &str, dir: &Path) -> anyhow::Result<RepoContext> {
    let out = run_git(program, dir, &["rev-parse", "--git-dir"])?;
    if out.status.success() {
        return Ok(RepoContext::Present);
    }
    let stderr = String::from_utf8_lossy(&out.stderr);
    if stderr_means_no_repository(&stderr) {
        return Ok(RepoContext::Absent);
    }
    Err(anyhow::anyhow!(
        "cannot tell whether '{}' is a git repository: {}",
        dir.display(),
        stderr.trim()
    ))
}

/// Make `dir` a git repository when it is a plain directory (#6274).
///
/// Why: the entry point both `tm` (guided default) and `tm launch` call before
/// project detection, so a plain directory becomes an ordinary no-origin git
/// project instead of a refusal. It never touches a directory that already has
/// a repository, so every path that works today is unchanged.
/// What: probes with [`repo_context`], decides with [`plan_auto_init`], runs
/// `git init` and prints [`initialized_message`] for the `Init` arm, prints
/// [`refusal_message`] for the `Refuse` arm. #7749: the `scan` seam
/// [`plan_auto_init`] consults is [`scan_for_child_repo`] over `dir` itself,
/// evaluated only when no cheaper guard already decided. Errors only when `git`
/// is missing (see [`missing_git_error`]), when git's verdict is unreadable, or
/// when `git init` itself fails.
/// Test: `auto_init_initializes_a_plain_directory`,
/// `auto_init_refuses_a_workspace_parent_with_a_deep_child_repository`,
/// `auto_init_refuses_when_the_downward_scan_cannot_finish`, and the rest of
/// `auto_git_init_tests.rs`.
pub(crate) fn ensure_git_repo(dir: &Path) -> anyhow::Result<AutoInitOutcome> {
    ensure_git_repo_with(dir, dirs::home_dir().as_deref(), GIT_PROGRAM)
}

/// [`ensure_git_repo`] with the home directory and the git executable injected.
///
/// Why: the two pieces of ambient state this decision depends on. Injecting
/// them lets the guard and prerequisite arms be tested without mutating `$HOME`
/// or `PATH` for the whole test binary.
/// Test: `auto_init_refuses_the_home_directory`,
/// `ensure_reports_a_missing_git_executable_without_writing`.
pub(crate) fn ensure_git_repo_with(
    dir: &Path,
    home: Option<&Path>,
    program: &str,
) -> anyhow::Result<AutoInitOutcome> {
    // A path that is not an existing directory is the caller's error to
    // report — `tm launch --dir /nope` already says so, and creating anything
    // there would be worse than that message.
    if !dir.is_dir() {
        return Ok(AutoInitOutcome::NotADirectory);
    }
    let context = repo_context(program, dir)?;
    let canonical = dir.canonicalize().unwrap_or_else(|_| dir.to_path_buf());
    let home_canonical = home.map(|h| h.canonicalize().unwrap_or_else(|_| h.to_path_buf()));
    // #7749: the downward scan is the last guard consulted, so it runs only for
    // a directory git already reported as having no repository at all.
    match plan_auto_init(&canonical, home_canonical.as_deref(), context, || {
        scan_for_child_repo(dir)
    }) {
        AutoInitPlan::AlreadyGit => Ok(AutoInitOutcome::AlreadyGit),
        AutoInitPlan::Refuse(refusal) => {
            eprintln!("{}", refusal_message(&refusal, dir));
            Ok(AutoInitOutcome::Refused(refusal))
        }
        AutoInitPlan::Init => {
            let out = run_git(program, dir, &["init"])?;
            if !out.status.success() {
                anyhow::bail!(
                    "`git init` failed in '{}': {}",
                    dir.display(),
                    String::from_utf8_lossy(&out.stderr).trim()
                );
            }
            eprintln!("{}", initialized_message(dir));
            Ok(AutoInitOutcome::Initialized)
        }
    }
}

#[cfg(test)]
#[path = "auto_git_init_tests.rs"]
mod tests;
