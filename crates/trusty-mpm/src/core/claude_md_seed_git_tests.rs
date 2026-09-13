//! Tests for the git-init-offer half of the seed-site guard (#7673 round 3).
//!
//! Split out with `#[path]` so `claude_md_seed_git.rs` stays under the
//! 500-SLOC production cap.

use super::*;
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
    std::fs::create_dir_all(&child_a).unwrap();
    std::fs::create_dir_all(&child_b).unwrap();
    if !git_init(&child_a) || !git_init(&child_b) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }
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
    std::fs::create_dir_all(&grandchild).unwrap();
    if !git_init(&grandchild) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    let err = offer_git_init(&parent, Some(&home), None).expect_err("a grandchild repo refuses");

    assert_eq!(err, SeedRefusal::WorkspaceParent(grandchild));
}

/// FAILS BEFORE THIS ROUND (#7673 round 3 review, MEDIUM): the original
/// two-level, depth-bound scan never looked past `<parent>/packages/<name>`,
/// so a pnpm/yarn/npm SCOPED package — `<parent>/packages/@scope/pkg-a`, a
/// common convention — sat one level past the bound and was never found. The
/// budget-bounded scan has no notion of depth at all, so it reaches this
/// repository the same way it reaches a two-level one.
#[test]
fn a_scoped_package_repository_three_levels_down_is_found() {
    let tmp = TempDir::new().unwrap();
    let parent = std::fs::canonicalize(tmp.path()).unwrap().join("workspace");
    let deep = parent.join("packages").join("@scope").join("pkg-a");
    std::fs::create_dir_all(&deep).unwrap();
    if !git_init(&deep) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }

    assert_eq!(find_child_git_repo(&parent), Some(deep));
}

/// A repository far enough down a single-child chain to exceed
/// [`WORKSPACE_SCAN_BUDGET`] sits outside the scan — cheap by design, not
/// exhaustive. This is what keeps the scan bounded against a cycle or a
/// pathologically deep tree; it is the deliberate trade-off the module doc
/// names, not a bug.
#[test]
fn a_repository_beyond_the_scan_budget_is_not_found() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("workspace");
    let mut deep = dir.clone();
    for _ in 0..(WORKSPACE_SCAN_BUDGET + 50) {
        deep = deep.join("d");
    }
    std::fs::create_dir_all(&deep).unwrap();
    if !git_init(&deep) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }

    assert_eq!(find_child_git_repo(&dir), None);
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
    if !git_init(&repo) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }
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
