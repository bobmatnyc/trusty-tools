//! Unit tests for `fs_browse` — path resolution, git-ness detection (including
//! the linked-worktree `.git`-as-file shape), typed-error distinguishability,
//! and the 7a response shape.

use super::*;

/// A directory's entries are returned with their names and absolute paths.
#[test]
fn lists_entries_with_names_and_paths() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("alpha")).expect("mkdir");
    std::fs::write(tmp.path().join("beta.txt"), "x").expect("write");

    let listing = list_dir(&tmp.path().display().to_string(), false).expect("list must succeed");

    let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["alpha", "beta.txt"], "dirs sort before files");
    assert!(listing.entries[0].is_dir);
    assert!(!listing.entries[1].is_dir);
    assert!(
        listing.entries[0].path.ends_with("alpha"),
        "entry path must be absolute: {}",
        listing.entries[0].path
    );
}

/// Entries sort directories-first, then case-insensitively — a stable picker
/// order that does not inherit the OS's readdir order.
#[test]
fn entries_sort_dirs_first_then_case_insensitively() {
    let tmp = tempfile::tempdir().expect("tempdir");
    for d in ["Zebra", "apple"] {
        std::fs::create_dir(tmp.path().join(d)).expect("mkdir");
    }
    std::fs::write(tmp.path().join("aaa.txt"), "x").expect("write");

    let listing = list_dir(&tmp.path().display().to_string(), false).expect("list must succeed");
    let names: Vec<&str> = listing.entries.iter().map(|e| e.name.as_str()).collect();
    assert_eq!(names, vec!["apple", "Zebra", "aaa.txt"]);
}

/// Hidden entries are omitted by default and included on request.
#[test]
fn hidden_entries_are_gated_by_include_hidden() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join(".hidden")).expect("mkdir");
    std::fs::create_dir(tmp.path().join("visible")).expect("mkdir");

    let default = list_dir(&tmp.path().display().to_string(), false).expect("list");
    assert_eq!(default.entries.len(), 1);
    assert_eq!(default.entries[0].name, "visible");

    let with_hidden = list_dir(&tmp.path().display().to_string(), true).expect("list");
    assert_eq!(with_hidden.entries.len(), 2);
}

// ── git-ness ────────────────────────────────────────────────────────────────

/// A conventional repo — `.git` as a DIRECTORY — earns the badge.
#[test]
fn git_dir_is_detected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("acme-api");
    std::fs::create_dir(&repo).expect("mkdir");
    std::fs::create_dir(repo.join(".git")).expect("mkdir .git");

    let listing = list_dir(&tmp.path().display().to_string(), false).expect("list");
    assert!(listing.entries[0].is_git_repo);
}

/// REGRESSION (the `.git`-as-file subtlety, cf. PR #2839): in a LINKED WORKTREE
/// `.git` is a FILE holding a `gitdir:` pointer, not a directory. An
/// `is_dir()`-based check reports such a worktree as non-git — this repo's own
/// checkouts are linked worktrees, so that bug would badge them all `—`.
#[test]
fn linked_worktree_gitfile_is_detected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let worktree = tmp.path().join("feature-wt");
    std::fs::create_dir(&worktree).expect("mkdir");
    // Exactly what `git worktree add` writes.
    std::fs::write(
        worktree.join(".git"),
        "gitdir: /Users/dev/main/.git/worktrees/feature-wt\n",
    )
    .expect("write .git file");

    assert!(
        is_git_repo(&worktree),
        "a linked worktree's `.git` FILE must count as a repo"
    );

    let listing = list_dir(&tmp.path().display().to_string(), false).expect("list");
    assert!(
        listing.entries[0].is_git_repo,
        "the worktree row must carry the git badge"
    );
}

/// A real `git worktree add`, if git is available — proves the detector against
/// git's actual on-disk output rather than only our hand-written fixture.
#[test]
fn real_linked_worktree_is_detected() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let main = tmp.path().join("main");
    std::fs::create_dir(&main).expect("mkdir");

    // `false` on any failure (git absent, sandboxed) so the test skips rather
    // than failing for reasons unrelated to the detector.
    let git = |args: &[&str], cwd: &std::path::Path| {
        std::process::Command::new("git")
            .arg("-C")
            .arg(cwd)
            .args(args)
            .output()
            .map(|o| o.status.success())
            .unwrap_or(false)
    };

    // Skip if git is absent or the repo cannot be initialised in this sandbox.
    for args in [
        &["init", "-q"][..],
        &["config", "user.email", "t@example.com"][..],
        &["config", "user.name", "t"][..],
        &["commit", "-q", "--allow-empty", "-m", "init"][..],
    ] {
        if !git(args, &main) {
            return;
        }
    }
    let wt = tmp.path().join("wt");
    if !git(
        &["worktree", "add", "-q", &wt.display().to_string(), "-d"],
        &main,
    ) {
        return;
    }

    assert!(
        wt.join(".git").is_file(),
        "precondition: git writes `.git` as a FILE in a linked worktree"
    );
    assert!(
        is_git_repo(&wt),
        "a real linked worktree must be detected as a repo"
    );
}

/// A file named `.git` that is NOT a git pointer must not earn a badge.
#[test]
fn bogus_dot_git_file_is_not_a_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("not-a-repo");
    std::fs::create_dir(&dir).expect("mkdir");
    std::fs::write(dir.join(".git"), "just some notes\n").expect("write");

    assert!(!is_git_repo(&dir));
}

/// A large `.git` FILE that is not a real pointer must not earn a badge, and
/// the bounded 64-byte read must not choke on (or need to read) the rest of it.
#[test]
fn large_dot_git_file_is_not_a_repo() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("not-a-repo");
    std::fs::create_dir(&dir).expect("mkdir");
    // Well over the 64-byte read bound, and not a `gitdir:` pointer.
    std::fs::write(dir.join(".git"), "x".repeat(10_000)).expect("write");

    assert!(!is_git_repo(&dir));
}

/// A `gitdir:` pointer whose content sits right at (and just past) the 64-byte
/// read bound is still detected — the bound must not truncate a real pointer.
#[test]
fn gitdir_pointer_at_exactly_the_read_bound() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let dir = tmp.path().join("worktree-at-bound");
    std::fs::create_dir(&dir).expect("mkdir");
    // "gitdir: " (8 bytes) + a path long enough to push the whole line past
    // 64 bytes — the prefix itself must still land within the first 64 bytes.
    let long_path = format!("/very/long/path/to/main/.git/worktrees/{}", "x".repeat(40));
    std::fs::write(dir.join(".git"), format!("gitdir: {long_path}\n")).expect("write");

    assert!(
        is_git_repo(&dir),
        "a gitdir: pointer must be detected even when the full line exceeds the read bound"
    );
}

/// A plain directory with no `.git` is a first-class NON-GIT entry (7a's
/// `scratch  —` row) — reported, never an error (#2728).
#[test]
fn non_git_dir_is_reported_as_non_git() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(tmp.path().join("scratch")).expect("mkdir");

    let listing = list_dir(&tmp.path().display().to_string(), false).expect("list must succeed");
    assert_eq!(listing.entries[0].name, "scratch");
    assert!(!listing.entries[0].is_git_repo);
}

/// A FILE never claims to be a repo, even next to a `.git` sibling.
#[test]
fn file_entries_are_never_git_repos() {
    let tmp = tempfile::tempdir().expect("tempdir");
    std::fs::write(tmp.path().join("readme.md"), "x").expect("write");

    let listing = list_dir(&tmp.path().display().to_string(), false).expect("list");
    assert!(!listing.entries[0].is_dir);
    assert!(!listing.entries[0].is_git_repo);
}

// ── shape / navigation ──────────────────────────────────────────────────────

/// The listing carries the breadcrumb caption, the up-target, and the badge
/// state 7a renders (`~/code / acme-api /` + `acme-api  git` / `scratch  —`).
#[test]
fn listing_shape_supports_breadcrumb_and_badges() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let repo = tmp.path().join("acme-api");
    std::fs::create_dir(&repo).expect("mkdir");
    std::fs::create_dir(repo.join(".git")).expect("mkdir");
    std::fs::create_dir(tmp.path().join("scratch")).expect("mkdir");

    let listing = list_dir(&tmp.path().display().to_string(), false).expect("list");

    // Breadcrumb: a caption the client splits on `/`.
    assert!(!listing.display_path.is_empty());
    assert!(listing.display_path.contains('/'));
    // Up-navigation: parent is present below the filesystem root.
    assert!(listing.parent.is_some());
    // Badges.
    let by_name = |n: &str| listing.entries.iter().find(|e| e.name == n).expect("row");
    assert!(by_name("acme-api").is_git_repo);
    assert!(!by_name("scratch").is_git_repo);
}

/// `parent` is `None` exactly at the filesystem root — the one case where the
/// client must disable its up-affordance.
#[test]
fn parent_is_none_at_filesystem_root() {
    let listing = list_dir("/", false).expect("root must list");
    assert_eq!(listing.path, "/");
    assert!(listing.parent.is_none());
}

/// A client navigates up by passing the previous response's `parent` back in.
#[test]
fn parent_round_trips_as_a_list_target() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let child = tmp.path().join("child");
    std::fs::create_dir(&child).expect("mkdir");

    let listing = list_dir(&child.display().to_string(), false).expect("list child");
    let parent = listing.parent.expect("child has a parent");
    let up = list_dir(&parent, false).expect("parent must list");

    assert!(up.entries.iter().any(|e| e.name == "child"));
}

/// `..` segments are resolved server-side, so relative navigation works too.
#[test]
fn dotdot_segments_are_resolved() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let child = tmp.path().join("child");
    std::fs::create_dir(&child).expect("mkdir");

    let listing = list_dir(&format!("{}/..", child.display()), false).expect("list");
    let expected = std::fs::canonicalize(tmp.path()).expect("canonicalize");
    assert_eq!(listing.path, expected.display().to_string());
}

// ── tilde ───────────────────────────────────────────────────────────────────

/// `~/x` expands against the home directory.
#[test]
fn tilde_expands_to_home() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    assert_eq!(expand_tilde("~/code").expect("expand"), home.join("code"));
}

/// A bare `~` expands to the home directory itself.
#[test]
fn tilde_bare_expands_to_home() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    assert_eq!(expand_tilde("~").expect("expand"), home);
}

/// Non-tilde paths pass through untouched — including a path merely CONTAINING
/// a tilde, which must not be mangled.
#[test]
fn absolute_path_is_left_alone() {
    assert_eq!(
        expand_tilde("/abs/path").expect("expand"),
        PathBuf::from("/abs/path")
    );
    assert_eq!(
        expand_tilde("/tmp/we~ird").expect("expand"),
        PathBuf::from("/tmp/we~ird")
    );
}

/// `display_path` collapses the home prefix back to `~` for the breadcrumb.
#[test]
fn display_path_collapses_home_to_tilde() {
    let Some(home) = dirs::home_dir() else {
        return;
    };
    assert_eq!(collapse_home(&home), "~");
    assert_eq!(collapse_home(&home.join("code")), "~/code");
    assert_eq!(collapse_home(Path::new("/etc")), "/etc");
}

/// Listing `~` succeeds and captions itself `~`.
#[test]
fn tilde_path_lists_home() {
    if dirs::home_dir().is_none() {
        return;
    }
    let listing = list_dir("~", false).expect("home must list");
    assert_eq!(listing.display_path, "~");
}

// ── errors ──────────────────────────────────────────────────────────────────

/// A nonexistent path is `NotFound`, not a generic IO error.
#[test]
fn nonexistent_path_is_not_found() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let missing = tmp.path().join("nope");

    let err = list_dir(&missing.display().to_string(), false).expect_err("must fail");
    assert!(
        matches!(err, ListDirError::NotFound(_)),
        "expected NotFound, got {err:?}"
    );
}

/// An existing FILE is `NotADirectory` — distinct from `NotFound`, because the
/// picker's remedy differs (the path is real, just not browsable).
#[test]
fn file_path_is_not_a_directory() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let file = tmp.path().join("a.txt");
    std::fs::write(&file, "x").expect("write");

    let err = list_dir(&file.display().to_string(), false).expect_err("must fail");
    assert!(
        matches!(err, ListDirError::NotADirectory(_)),
        "expected NotADirectory, got {err:?}"
    );
}

/// An unreadable directory surfaces as `PermissionDenied` — the OS's refusal is
/// reported as an ordinary typed error, with no bespoke permission model on top
/// (see module docs).
#[cfg(unix)]
#[test]
fn unreadable_dir_is_permission_denied() {
    use std::os::unix::fs::PermissionsExt;

    // Running as root defeats mode bits — the OS never refuses, so there is
    // nothing to assert.
    if unsafe { libc::geteuid() } == 0 {
        return;
    }

    let tmp = tempfile::tempdir().expect("tempdir");
    let locked = tmp.path().join("locked");
    std::fs::create_dir(&locked).expect("mkdir");
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).expect("chmod 000");

    let result = list_dir(&locked.display().to_string(), false);

    // Restore before asserting so the tempdir can always clean itself up.
    std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o755)).expect("chmod 755");

    let err = result.expect_err("an unreadable dir must fail");
    assert!(
        matches!(err, ListDirError::PermissionDenied(_)),
        "expected PermissionDenied, got {err:?}"
    );
}
