//! Tests for the `tm fleet init` directory preflight (#8436 code-critic BLOCK).
//!
//! Every refusal test drives [`init`] itself, one case per test, so each one
//! fails on its own against the pre-fix `init` (3d4bff471), which accepted
//! every directory. Each runs under a scratch `<outer>/home`, so a write the
//! old code made still lands inside the scratch tree.

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};

use super::super::{init, resolve_dir, user_config_path};
use super::{DirRefusal, check};

/// `<outer>/home`, so a test can name a real ancestor of the home directory.
struct Scratch {
    outer: tempfile::TempDir,
}

impl Scratch {
    fn new() -> Self {
        let outer = tempfile::tempdir().expect("scratch dir");
        std::fs::create_dir(outer.path().join("home")).expect("scratch home");
        Self { outer }
    }

    fn outer(&self) -> PathBuf {
        std::fs::canonicalize(self.outer.path()).unwrap()
    }

    fn home(&self) -> PathBuf {
        self.outer().join("home")
    }

    /// A git repository at `path`, with `origin` when `remote`.
    fn repo(&self, path: &Path, remote: bool) -> PathBuf {
        std::fs::create_dir_all(path).unwrap();
        git(path, &["init", "--quiet"]);
        if remote {
            git(
                path,
                &["remote", "add", "origin", "https://example.invalid/r.git"],
            );
        }
        path.to_path_buf()
    }

    /// `init` refuses `dir` under this scratch home before writing anything.
    fn refused(&self, dir: &Path) -> DirRefusal {
        refused_under(dir, &self.home())
    }
}

fn git(dir: &Path, args: &[&str]) {
    let out = super::git().arg("-C").arg(dir).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {out:?}");
}

/// `init` fails with a [`DirRefusal`] and wrote neither the grant nor the
/// project file.
fn refused_under(dir: &Path, home: &Path) -> DirRefusal {
    let err = init(dir, home, false).expect_err(&format!("{} must be refused", dir.display()));
    let refusal = err
        .downcast_ref::<DirRefusal>()
        .unwrap_or_else(|| panic!("{}: not a preflight refusal: {err:#}", dir.display()))
        .clone();
    assert!(
        !user_config_path(home).exists(),
        "{}: the allowlist was written",
        dir.display()
    );
    assert!(
        !dir.join(".trusty-mpm.toml").exists(),
        "{}: the project file was written",
        dir.display()
    );
    refusal
}

fn assert_workspace_parent(refusal: DirRefusal, dir: &Path, why: &str) {
    match refusal {
        DirRefusal::WorkspaceParent { path, reason } => {
            assert_eq!(path, dir);
            assert!(reason.contains(why), "{dir:?}: {reason}");
        }
        other => panic!("{dir:?}: expected a workspace parent, got {other:?}"),
    }
}

fn assert_unresolvable(refusal: DirRefusal, why: &str) {
    assert!(
        matches!(&refusal, DirRefusal::Unresolvable { reason, .. } if reason.contains(why)),
        "expected Unresolvable({why}), got {refusal:?}"
    );
}

#[test]
fn the_filesystem_root_is_refused() {
    let s = Scratch::new();
    for root in ["/", "/usr/.."] {
        assert_eq!(
            s.refused(Path::new(root)),
            DirRefusal::FilesystemRoot(PathBuf::from("/"))
        );
    }
}

#[test]
fn the_home_directory_is_refused_however_it_is_spelled() {
    let s = Scratch::new();
    let home = s.home();
    std::fs::create_dir(home.join("sub")).unwrap();
    let link = s.outer().join("home-link");
    std::os::unix::fs::symlink(&home, &link).unwrap();
    for spelled in [home.clone(), home.join("sub/.."), link] {
        assert_eq!(
            s.refused(&spelled),
            DirRefusal::Home(home.clone()),
            "{spelled:?}"
        );
    }
}

#[test]
fn a_path_inside_another_work_tree_is_refused() {
    let s = Scratch::new();
    let repo = s.repo(&s.home().join("code/app"), false);
    std::fs::create_dir(repo.join("docs")).unwrap();
    for inner in [repo.join("docs"), repo.join("new/arch")] {
        assert_eq!(
            s.refused(&inner),
            DirRefusal::InsideWorkTree {
                path: inner.clone(),
                work_tree: repo.clone()
            }
        );
    }
}

#[test]
fn a_repository_with_a_remote_is_refused() {
    let s = Scratch::new();
    let cloned = s.repo(&s.home().join("clone"), true);
    assert_eq!(
        s.refused(&cloned),
        DirRefusal::HasRemote {
            path: cloned,
            remotes: "origin".to_owned()
        }
    );
}

#[test]
fn the_projects_root_is_refused_populated_or_not() {
    let s = Scratch::new();
    let root = s.home().join("trusty-mpm-projects");
    assert_workspace_parent(s.refused(&root), &root, "projects root");
    s.repo(&root.join("bobmatnyc/trusty-tools"), true);
    assert_workspace_parent(s.refused(&root), &root, "projects root");
}

#[test]
fn an_owner_directory_under_the_projects_root_is_refused() {
    let s = Scratch::new();
    let owner = s.home().join("trusty-mpm-projects/bobmatnyc");
    s.repo(&owner.join("trusty-tools"), true);
    assert_workspace_parent(s.refused(&owner), &owner, "contains the repository");
}

#[test]
fn a_directory_holding_a_deep_repository_is_refused() {
    let s = Scratch::new();
    let code = s.home().join("code");
    s.repo(&code.join("deep/nested/app"), false);
    assert_workspace_parent(s.refused(&code), &code, "contains the repository");
}

#[test]
fn an_ancestor_of_the_home_directory_is_refused() {
    let s = Scratch::new();
    let outer = s.outer();
    assert_workspace_parent(s.refused(&outer), &outer, "home directory");
}

/// Fail-Open Check: canonicalize cannot finish through a dangling symlink.
#[test]
fn a_dangling_symlink_refuses() {
    let s = Scratch::new();
    let dangling = s.home().join("dangling");
    std::os::unix::fs::symlink(s.home().join("missing"), &dangling).unwrap();
    assert_unresolvable(s.refused(&dangling), "dangling symlink");
}

/// Fail-Open Check: `..` below a directory that does not exist.
#[test]
fn dot_dot_below_a_missing_directory_refuses() {
    let s = Scratch::new();
    assert_unresolvable(s.refused(&s.home().join("nope/../arch")), "`..`");
}

/// Fail-Open Check: the home directory cannot be canonicalized.
#[test]
fn an_unresolvable_home_refuses() {
    let s = Scratch::new();
    let dir = s.outer().join("arch");
    let refusal = refused_under(&dir, &s.outer().join("no-such-home"));
    assert_unresolvable(refusal, "No such file");
    assert!(!dir.exists(), "init created the directory before refusing");
}

/// Restores a directory's permissions on drop, so a failed test still cleans up.
struct Unlock(PathBuf);

impl Drop for Unlock {
    fn drop(&mut self) {
        let _ = std::fs::set_permissions(&self.0, std::fs::Permissions::from_mode(0o755));
    }
}

/// Fail-Open Check: the repository scan cannot read the target.
#[test]
fn an_unreadable_target_refuses() {
    let s = Scratch::new();
    let unreadable = s.home().join("unreadable");
    std::fs::create_dir(&unreadable).unwrap();
    // Write and search, no read: `init` could write here, the scan cannot list it.
    std::fs::set_permissions(&unreadable, std::fs::Permissions::from_mode(0o300)).unwrap();
    let _unlock = Unlock(unreadable.clone());
    assert_unresolvable(s.refused(&unreadable), "did not finish");
}

/// Fail-Open Check: `git remote` errors on a `.git` git cannot read.
#[test]
fn a_failing_remote_listing_refuses() {
    let s = Scratch::new();
    let broken = s.home().join("broken");
    std::fs::create_dir(&broken).unwrap();
    std::fs::write(broken.join(".git"), "not a gitdir\n").unwrap();
    let refusal = s.refused(&broken);
    assert!(
        matches!(refusal, DirRefusal::GitProbeFailed { .. }),
        "{refusal:?}"
    );
}

#[test]
fn the_default_and_a_fresh_directory_pass() {
    let s = Scratch::new();
    let home = s.home();
    // A populated projects root does not block its `architect` child (ruling Q4).
    s.repo(
        &home.join("trusty-mpm-projects/bobmatnyc/trusty-tools"),
        true,
    );
    let default = resolve_dir(None, &home).unwrap();
    assert_eq!(check(&default, &home), Ok(default.clone()));

    let fresh = s.outer().join("fresh");
    std::fs::create_dir(&fresh).unwrap();
    assert_eq!(check(&fresh, &home), Ok(fresh.clone()));
    assert_eq!(check(&fresh.join("a/b"), &home), Ok(fresh.join("a/b")));

    // Through a symlink, the canonical path is what gets checked and written.
    let link = s.outer().join("fresh-link");
    std::os::unix::fs::symlink(&fresh, &link).unwrap();
    assert_eq!(check(&link, &home), Ok(fresh.clone()));

    // A second run over the repository the first run created still passes.
    init(&default, &home, false).unwrap();
    assert_eq!(check(&default, &home), Ok(default.clone()));
    let again = init(&default, &home, false).unwrap();
    assert!(!again.changed(), "{}", again.render());
}
