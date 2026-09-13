//! Tests for the git-init-offer half of the seed-site guard (#7673 round 3).
//!
//! Split out with `#[path]` so `claude_md_seed_git.rs` stays under the
//! 500-SLOC production cap.

use super::*;
use crate::core::child_repo_scan::ScanIncomplete;
use std::path::PathBuf;
use tempfile::TempDir;

/// `git -C <dir> init -q`, reporting whether git was available at all.
fn git_init(dir: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init", "-q"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// A prompt seam that never reads stdin: counts how many times it is called
/// and always answers `answer`. `Cell` rather than a `&mut usize` field keeps
/// the closure itself `Fn`-shaped so a plain `move ||` suffices.
fn counting_prompt(answer: bool) -> (impl FnMut() -> bool, std::rc::Rc<std::cell::Cell<usize>>) {
    let calls = std::rc::Rc::new(std::cell::Cell::new(0usize));
    let counter = calls.clone();
    (
        move || {
            counter.set(counter.get() + 1);
            answer
        },
        calls,
    )
}

/// FAILS BEFORE THIS ROUND: nothing scanned below the target at all, so a
/// workspace parent containing child repositories was silently seedable and
/// the seeded file became an ancestor `CLAUDE.md` for every child project.
#[test]
fn a_workspace_parent_is_refused_naming_the_child() {
    let tmp = TempDir::new().unwrap();
    let parent = std::fs::canonicalize(tmp.path()).unwrap().join("workspace");
    let child_a = parent.join("service-a");
    let child_b = parent.join("service-b");
    // #7774 review: the scan only stats `<child>/.git`, so no git binary is
    // needed and the test cannot pass by skipping.
    std::fs::create_dir_all(child_a.join(".git")).unwrap();
    std::fs::create_dir_all(child_b.join(".git")).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let err = offer_git_init(&parent, Some(&home), None).expect_err("a workspace parent refuses");

    match err {
        SeedRefusal::WorkspaceParent(child) => {
            assert!(
                child == child_a || child == child_b,
                "the refusal must name a real child repository: {}",
                child.display()
            );
        }
        other => panic!("expected WorkspaceParent, got {other:?}"),
    }
}

/// A child repository one level deeper (`<parent>/packages/<name>`) is still
/// within the bounded downward scan.
#[test]
fn a_grandchild_repository_is_found() {
    let tmp = TempDir::new().unwrap();
    let parent = std::fs::canonicalize(tmp.path()).unwrap().join("workspace");
    let grandchild = parent.join("packages").join("service-a");
    std::fs::create_dir_all(grandchild.join(".git")).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let err = offer_git_init(&parent, Some(&home), None).expect_err("a grandchild repo refuses");

    assert_eq!(err, SeedRefusal::WorkspaceParent(grandchild));
}

/// A workspace-parent fixture: `<tmp>/workspace` plus a `<tmp>/home`.
fn workspace_and_home(tmp: &TempDir) -> (PathBuf, PathBuf) {
    let parent = std::fs::canonicalize(tmp.path()).unwrap().join("workspace");
    std::fs::create_dir_all(&parent).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    (parent, home)
}

/// FAILS BEFORE THIS ROUND (#7673 round 2 review, CRITICAL): `node_modules`,
/// created first, held more subdirectories than the scan budget; the scan ran
/// out before `packages/app` and its `None` was read as "workspace is clear",
/// so tm seeded the workspace parent. Whatever order `read_dir` gives, the
/// only acceptable answers are the two refusals.
#[test]
fn a_wide_node_modules_sibling_never_lets_a_workspace_parent_seed() {
    let tmp = TempDir::new().unwrap();
    let (parent, home) = workspace_and_home(&tmp);
    for i in 0..300 {
        std::fs::create_dir_all(parent.join("node_modules").join(format!("pkg{i:03}"))).unwrap();
    }
    let app = parent.join("packages").join("app");
    std::fs::create_dir_all(app.join(".git")).unwrap();

    match offer_git_init(&parent, Some(&home), None) {
        Err(SeedRefusal::WorkspaceParent(child)) => assert_eq!(child, app),
        Err(SeedRefusal::ScanIncomplete(_)) => {}
        other => panic!("a workspace parent must never seed, got {other:?}"),
    }
}

/// FAILS BEFORE THIS ROUND: a directory wider than the budget, holding no
/// repository at all, exhausted the scan and was seeded as if clear.
#[test]
fn an_exhausted_scan_refuses_to_seed() {
    let tmp = TempDir::new().unwrap();
    let (parent, home) = workspace_and_home(&tmp);
    for i in 0..(crate::core::child_repo_scan::WORKSPACE_SCAN_BUDGET + 44) {
        std::fs::create_dir_all(parent.join(format!("d{i:03}"))).unwrap();
    }

    assert_eq!(
        offer_git_init(&parent, Some(&home), None),
        Err(SeedRefusal::ScanIncomplete(ScanIncomplete::BudgetExhausted))
    );
}

/// FAILS BEFORE THIS ROUND: an unreadable child was skipped, so the directory
/// seeded without the scan having looked inside it.
#[cfg(unix)]
#[test]
fn an_unreadable_child_directory_refuses_to_seed() {
    use std::os::unix::fs::PermissionsExt;
    struct RestoreMode(PathBuf);
    impl Drop for RestoreMode {
        fn drop(&mut self) {
            let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
        }
    }
    let tmp = TempDir::new().unwrap();
    let (parent, home) = workspace_and_home(&tmp);
    let locked = parent.join("locked");
    std::fs::create_dir_all(&locked).unwrap();
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();
    let _restore = RestoreMode(locked.clone());
    if std::fs::read_dir(&locked).is_ok() {
        eprintln!("#7673 tests: running with permission overrides (root?), skipping");
        return;
    }

    match offer_git_init(&parent, Some(&home), None) {
        Err(SeedRefusal::ScanIncomplete(ScanIncomplete::Unreadable { path, .. })) => {
            assert_eq!(path, locked);
        }
        other => panic!("an unreadable child must refuse the seed, got {other:?}"),
    }
    assert!(!parent.join(".git").exists());
}

/// FAILS BEFORE THIS ROUND (regression b): a marker-less, non-git directory
/// below home must still seed — `git init` declined by the non-interactive
/// default (`should_init: None`) — with no `.git` created.
#[test]
fn a_non_interactive_caller_declines_git_init_and_still_may_seed() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("projects");
    std::fs::create_dir_all(&dir).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let ran = offer_git_init(&dir, Some(&home), None).expect("a plain directory is not refused");

    assert!(!ran, "a non-interactive caller must default to NO");
    assert!(!dir.join(".git").exists(), "no git init may run silently");
}

/// Regression (c): the accept path, driven through the injected prompt seam —
/// never stdin — runs `git init` and still seeds.
#[test]
fn an_accepted_offer_runs_git_init() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("projects");
    std::fs::create_dir_all(&dir).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (mut prompt, calls) = counting_prompt(true);

    let ran = offer_git_init(
        &dir,
        Some(&home),
        Some(&mut prompt as &mut dyn FnMut() -> bool),
    )
    .expect("a plain directory is not refused");

    assert!(ran, "an accepted offer must report that git init ran");
    assert!(dir.join(".git").is_dir(), "git init must have actually run");
    assert_eq!(calls.get(), 1);
}

/// A directory where `git init` exits non-zero: a `.git` FILE with no valid
/// `gitdir:` line. Neither `harness_root_for` nor [`has_git_ancestor`] sees a
/// repository and the downward scan is clear, so the offer is reached.
fn dir_where_git_init_fails(tmp: &TempDir) -> (PathBuf, PathBuf) {
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("projects");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".git"), "not a gitfile\n").unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    (dir, home)
}

/// FAILS BEFORE THE #7774 REVIEW FIX: a failed `git init` returned `Ok(false)`,
/// the same answer as a decline, so the caller seeded the non-git directory.
#[test]
fn a_failed_git_init_refuses_to_seed() {
    let tmp = TempDir::new().unwrap();
    let (dir, home) = dir_where_git_init_fails(&tmp);
    let (mut prompt, calls) = counting_prompt(true);

    let refusal = offer_git_init(
        &dir,
        Some(&home),
        Some(&mut prompt as &mut dyn FnMut() -> bool),
    )
    .expect_err("an accepted offer whose git init fails must refuse the seed");

    assert_eq!(calls.get(), 1);
    let msg = refusal.message(&dir);
    assert!(msg.contains("`git init` failed"), "{msg}");
    assert!(msg.contains(&dir.display().to_string()), "{msg}");
}

/// FAILS BEFORE THE #7774 REVIEW FIX: `git -C <dir> init` cannot run in a
/// first-touch directory the pipeline has not created yet, so an accepted
/// offer there silently produced no repository.
#[test]
fn an_accepted_offer_on_a_first_touch_directory_runs_git_init() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("new")
        .join("project");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (mut prompt, calls) = counting_prompt(true);

    let ran = offer_git_init(
        &dir,
        Some(&home),
        Some(&mut prompt as &mut dyn FnMut() -> bool),
    )
    .expect("a first-touch directory is not refused");

    assert!(ran, "an accepted offer must report that git init ran");
    assert!(
        dir.join(".git").is_dir(),
        "git init must create the directory"
    );
    assert_eq!(calls.get(), 1);
}

/// Child-mode switch for [`offer_in_child_process`]: the directory to offer
/// `git init` for. Set on the child `Command` only, never on this process.
const CHILD_DIR: &str = "TM_7774_OFFER_CHILD_DIR";
/// The home directory the child passes to [`offer_git_init`].
const CHILD_HOME: &str = "TM_7774_OFFER_CHILD_HOME";
/// Prefix of the line the child prints its outcome on.
const CHILD_OUTCOME: &str = "7774-child-outcome:";

/// Child half of a re-executed test: when [`offer_in_child_process`] started
/// this process, run an accepted offer, print its outcome, and return `true`.
fn ran_as_child() -> bool {
    let Some(dir) = std::env::var_os(CHILD_DIR) else {
        return false;
    };
    let home = std::env::var_os(CHILD_HOME).map(PathBuf::from);
    let mut accept = || true;
    let outcome = offer_git_init(
        Path::new(&dir),
        home.as_deref(),
        Some(&mut accept as &mut dyn FnMut() -> bool),
    );
    println!("{CHILD_OUTCOME} {outcome:?}");
    true
}

/// Re-run test `name` as a child process that accepts the offer for `dir`,
/// with `cwd` and `envs` applied to the child only, and return its stdout.
///
/// Why: the inputs under test are process state — an inherited `GIT_DIR`, the
/// `git` found on `PATH`, the directory a relative path resolves against.
/// Setting them in this process would leak into every parallel test (see
/// `docs/reference/common-pitfalls.md`), so they go on a child instead.
fn offer_in_child_process(
    name: &str,
    cwd: &Path,
    dir: &Path,
    home: &Path,
    envs: &[(&str, &std::ffi::OsStr)],
) -> String {
    // libtest names tests relative to the crate root, without the crate name.
    let module = module_path!();
    let module = module.split_once("::").map_or(module, |(_, rest)| rest);
    let mut cmd = std::process::Command::new(std::env::current_exe().expect("test binary path"));
    cmd.args([
        format!("{module}::{name}").as_str(),
        "--exact",
        "--nocapture",
        "--test-threads",
        "1",
    ])
    .current_dir(cwd)
    .env(CHILD_DIR, dir)
    .env(CHILD_HOME, home);
    for (key, value) in envs {
        cmd.env(key, value);
    }
    let out = cmd.output().expect("re-run this test as a child process");
    let stdout = String::from_utf8_lossy(&out.stdout).into_owned();
    assert!(
        stdout.contains("1 passed") && stdout.contains(CHILD_OUTCOME),
        "the child must have run the offer; stdout: {stdout}\nstderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    stdout
}

/// FAILS BEFORE THE #7774 ROUND-2 FIX: git honoured an inherited `GIT_DIR`
/// (as inside a git hook), created the repository there, exited 0, and the
/// target was seeded with no `.git` of its own.
#[test]
fn an_inherited_git_dir_cannot_redirect_git_init() {
    if ran_as_child() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("projects");
    std::fs::create_dir_all(&dir).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let elsewhere = root.join("elsewhere.git");
    let elsewhere_tree = root.join("elsewhere-tree");
    std::fs::create_dir_all(&elsewhere_tree).unwrap();

    let stdout = offer_in_child_process(
        "an_inherited_git_dir_cannot_redirect_git_init",
        &root,
        &dir,
        &home,
        &[
            ("GIT_DIR", elsewhere.as_os_str()),
            ("GIT_WORK_TREE", elsewhere_tree.as_os_str()),
        ],
    );

    assert!(stdout.contains("Ok(true)"), "{stdout}");
    assert!(
        dir.join(".git").join("HEAD").is_file(),
        "the repository must be created in the target; {stdout}"
    );
    assert!(
        !elsewhere.exists(),
        "GIT_DIR must not receive the repository"
    );
}

/// FAILS BEFORE THE #7774 ROUND-2 FIX: a `git` on `PATH` that exits 0 without
/// doing anything was trusted, and the target was seeded with no `.git`.
#[cfg(unix)]
#[test]
fn a_git_that_exits_zero_without_initialising_refuses_to_seed() {
    if ran_as_child() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let dir = root.join("projects");
    std::fs::create_dir_all(&dir).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();
    let shim_dir = root.join("shim-bin");
    std::fs::create_dir_all(&shim_dir).unwrap();
    // A symlink, not a written script: exec'ing a file this process just wrote
    // can race another test's fork (ETXTBSY).
    let truth = ["/usr/bin/true", "/bin/true"]
        .into_iter()
        .map(Path::new)
        .find(|p| p.is_file())
        .expect("a `true` binary at /usr/bin/true or /bin/true");
    std::os::unix::fs::symlink(truth, shim_dir.join("git")).unwrap();

    let stdout = offer_in_child_process(
        "a_git_that_exits_zero_without_initialising_refuses_to_seed",
        &root,
        &dir,
        &home,
        &[("PATH", shim_dir.as_os_str())],
    );

    assert!(stdout.contains("Err(GitInitFailed("), "{stdout}");
    assert!(!dir.join(".git").exists());
}

/// FAILS BEFORE THE #7774 ROUND-2 FIX: a relative target named `--bare` was
/// parsed as a `git init` option, so git built a bare repository in the
/// current directory, exited 0, and `./--bare` was seeded with no `.git`.
#[test]
fn a_dash_prefixed_directory_is_initialised_as_a_path() {
    if ran_as_child() {
        return;
    }
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    let stdout = offer_in_child_process(
        "a_dash_prefixed_directory_is_initialised_as_a_path",
        &root,
        Path::new("--bare"),
        &home,
        &[],
    );

    assert!(stdout.contains("Ok(true)"), "{stdout}");
    assert!(
        root.join("--bare").join(".git").join("HEAD").is_file(),
        "`--bare` must be initialised as a directory; {stdout}"
    );
    assert!(
        !root.join("HEAD").exists(),
        "no bare repository may be built in the current directory"
    );
}

/// FAILS BEFORE THE #7774 ROUND-2 FIX: a stale ancestor `.git` FILE (a removed
/// worktree's `gitdir:`) counted as a repository and returned before the
/// downward scan, so a workspace parent holding a real repository was seeded —
/// the #7673 incident shape.
#[test]
fn a_stale_ancestor_git_file_cannot_bypass_the_workspace_parent_scan() {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    std::fs::write(root.join(".git"), "gitdir: /nonexistent/worktrees/gone\n").unwrap();
    let workspace = root.join("ws");
    let app = workspace.join("app");
    std::fs::create_dir_all(app.join(".git")).unwrap();
    let home = root.join("home");
    std::fs::create_dir_all(&home).unwrap();

    assert_eq!(
        offer_git_init(&workspace, Some(&home), None),
        Err(SeedRefusal::WorkspaceParent(app))
    );
}

/// A declined offer, driven through the same prompt seam, leaves no `.git`.
#[test]
fn a_declined_offer_creates_no_git_directory() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("projects");
    std::fs::create_dir_all(&dir).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (mut prompt, calls) = counting_prompt(false);

    let ran = offer_git_init(
        &dir,
        Some(&home),
        Some(&mut prompt as &mut dyn FnMut() -> bool),
    )
    .expect("a plain directory is not refused");

    assert!(!ran);
    assert!(!dir.join(".git").exists());
    assert_eq!(calls.get(), 1);
}

/// Regression (d): a target already inside an existing repository (a nested
/// subdirectory) never gets the `git init` offer at all — running it there
/// would create a nested repository (an accidental submodule) — and the
/// prompt seam is never invoked.
#[test]
fn git_init_is_never_offered_inside_an_existing_repository() {
    let tmp = TempDir::new().unwrap();
    let repo = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
    let nested = repo.join("crates").join("thing");
    std::fs::create_dir_all(&nested).unwrap();
    // #7774 review: `harness_root_for` needs a real repository, so a failed
    // `git init` fails the test instead of skipping it.
    assert!(git_init(&repo), "git init must succeed for this fixture");
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();
    let (mut prompt, calls) = counting_prompt(true);

    let ran = offer_git_init(
        &nested,
        Some(&home),
        Some(&mut prompt as &mut dyn FnMut() -> bool),
    )
    .expect("an ordinary subdirectory of a git project is not refused");

    assert!(!ran, "already inside a repository — nothing to run");
    assert_eq!(calls.get(), 0, "the offer must never even ask");
    assert!(
        !nested.join(".git").exists(),
        "no nested repository is created"
    );
}

/// The filesystem-only fallback finds a `.git` ancestor `harness_root_for`
/// would normally report through the `git` binary — exercised directly since
/// simulating "no git binary" in-process is not practical.
#[test]
fn an_ancestor_git_directory_is_found_without_the_git_binary() {
    let tmp = TempDir::new().unwrap();
    let repo = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
    let nested = repo.join("nested");
    std::fs::create_dir_all(repo.join(".git")).unwrap();
    std::fs::create_dir_all(&nested).unwrap();

    assert!(has_git_ancestor(&nested));
}

/// Home/AboveHome refusals from [`refuse_seed_at`] still apply unchanged.
#[test]
fn the_home_refusal_still_applies_through_offer_git_init() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    assert_eq!(
        offer_git_init(&home, Some(&home), None),
        Err(SeedRefusal::Home)
    );
}

/// [`SeedRefusal::message`] for the new variant names the child repository.
#[test]
fn the_workspace_parent_refusal_names_the_child_repository() {
    let msg =
        SeedRefusal::WorkspaceParent(PathBuf::from("/ws/service-a")).message(Path::new("/ws"));
    assert!(msg.contains("/ws/service-a"), "{msg}");
    assert!(msg.contains("/ws"), "{msg}");
}

/// The scan-incomplete refusal names the directory, says the scan could not
/// rule out child repositories, and names both ways forward.
#[test]
fn the_scan_incomplete_refusal_tells_the_operator_how_to_proceed() {
    let msg = SeedRefusal::ScanIncomplete(ScanIncomplete::Unreadable {
        path: PathBuf::from("/ws/locked"),
        error: "Permission denied (os error 13)".to_string(),
    })
    .message(Path::new("/ws"));
    assert!(msg.contains("at /ws "), "{msg}");
    assert!(msg.contains("could not rule out git repositories"), "{msg}");
    assert!(msg.contains("/ws/locked could not be read"), "{msg}");
    assert!(msg.contains("run `git init` there yourself"), "{msg}");
    assert!(
        msg.contains("run tm from the actual project directory"),
        "{msg}"
    );

    let exhausted =
        SeedRefusal::ScanIncomplete(ScanIncomplete::BudgetExhausted).message(Path::new("/ws"));
    assert!(
        exhausted.contains("stopped after checking 256 directories"),
        "{exhausted}"
    );
}
