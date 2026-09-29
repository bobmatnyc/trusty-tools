//! Tests for the `tm fleet init` directory preflight (#8436 code-critic BLOCK).
//!
//! Every refusal test drives [`init`] itself, one case per test, so each one
//! fails on its own against the pre-fix `init` (3d4bff471), which accepted
//! every directory. Each runs under a scratch `<outer>/home`, so a write the
//! old code made still lands inside the scratch tree.

use std::os::unix::fs::{MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::SystemTime;

use super::super::tests::NO_TMUX;
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

/// What `init` writes: the target directory, its `.git` and project file, and
/// the user allowlist. Each is `(path, Some((len, mtime)))`, or `None` when
/// the path does not exist.
fn write_set(dir: &Path, home: &Path) -> Vec<(PathBuf, Option<(u64, SystemTime)>)> {
    [
        dir.to_path_buf(),
        dir.join(".git"),
        dir.join(".trusty-mpm.toml"),
        user_config_path(home),
    ]
    .into_iter()
    .map(|path| {
        let meta = std::fs::symlink_metadata(&path).ok();
        let stamp = meta.map(|m| (m.len(), m.modified().expect("mtime")));
        (path, stamp)
    })
    .collect()
}

/// `init` fails with a [`DirRefusal`] and left its write set as it found it:
/// the target was not created, and its `.git`, the project file and the
/// allowlist were neither created nor changed.
fn refused_under(dir: &Path, home: &Path) -> DirRefusal {
    let before = write_set(dir, home);
    let err =
        init(dir, home, false, NO_TMUX).expect_err(&format!("{} must be refused", dir.display()));
    let refusal = err
        .downcast_ref::<DirRefusal>()
        .unwrap_or_else(|| panic!("{}: not a preflight refusal: {err:#}", dir.display()))
        .clone();
    assert_eq!(
        write_set(dir, home),
        before,
        "{}: init changed what it writes",
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
    // A mount root runs the device comparison. `/dev` is devfs on macOS and
    // devtmpfs on Linux; a host with no separate `/dev` has no mount root to
    // test here, so only this assertion is skipped there.
    let dev = Path::new("/dev");
    let device = |p: &Path| std::fs::metadata(p).map(|m| m.dev()).unwrap();
    if device(dev) != device(Path::new("/")) {
        assert_eq!(
            s.refused(dev),
            DirRefusal::FilesystemRoot(dev.to_path_buf())
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

/// `<home>/.trusty-tools/trusty-mpm/config.yaml` holding `yaml`.
fn write_tm_config(home: &Path, yaml: &str) -> PathBuf {
    let config = home.join(".trusty-tools/trusty-mpm/config.yaml");
    std::fs::create_dir_all(config.parent().unwrap()).unwrap();
    std::fs::write(&config, yaml).unwrap();
    config
}

/// Fail-Open Check (critic H1): a config that will not parse hides the
/// configured projects root, so it refuses instead of using the default.
#[test]
fn a_malformed_tm_config_refuses() {
    let s = Scratch::new();
    let config = write_tm_config(&s.home(), "workspace_root_template: [unclosed\n");
    let refusal = s.refused(&s.home().join("arch"));
    assert_unresolvable(refusal, &config.display().to_string());
}

/// Critic H1: every candidate projects root is refused, not only the one
/// that wins by precedence. Here, the configured root.
#[test]
fn the_configured_projects_root_is_refused() {
    let s = Scratch::new();
    let root = s.outer().join("elsewhere");
    write_tm_config(
        &s.home(),
        &format!("workspace_root_template: {}\n", root.display()),
    );
    assert_workspace_parent(s.refused(&root), &root, "projects root");
}

/// Critic M1: `~/.trusty-mpm` holds the `[supervisor] projects` allowlist.
/// It holds no repository here, so the repository scan cannot refuse it.
#[test]
fn the_tm_config_directory_is_refused() {
    let s = Scratch::new();
    let config_dir = s.home().join(".trusty-mpm");
    std::fs::create_dir_all(config_dir.join("sub")).unwrap();
    for dir in [
        config_dir.clone(),
        config_dir.join("sub"),
        config_dir.join("arch"),
    ] {
        assert_eq!(
            s.refused(&dir),
            DirRefusal::ConfigDir {
                path: dir.clone(),
                config_dir: config_dir.clone()
            }
        );
    }
}

/// Critic M1: through a symlinked `~/.trusty-mpm`, the directory holding its
/// real target is refused too.
#[test]
fn a_directory_holding_a_symlinked_tm_config_directory_is_refused() {
    let s = Scratch::new();
    let dotfiles = s.outer().join("dotfiles");
    let real = dotfiles.join("trusty-mpm");
    std::fs::create_dir_all(&real).unwrap();
    std::os::unix::fs::symlink(&real, s.home().join(".trusty-mpm")).unwrap();
    assert_eq!(
        s.refused(&dotfiles),
        DirRefusal::ConfigDir {
            path: dotfiles.clone(),
            config_dir: real
        }
    );
}

/// Fail-Open Check (critic L1): an ancestor `.git` that is a symlink to
/// itself cannot be checked (ELOOP), so it refuses.
#[test]
fn a_looping_ancestor_git_entry_refuses() {
    let s = Scratch::new();
    let code = s.home().join("code");
    std::fs::create_dir(&code).unwrap();
    std::os::unix::fs::symlink(code.join(".git"), code.join(".git")).unwrap();
    match s.refused(&code.join("arch")) {
        DirRefusal::Unresolvable { path, .. } => assert_eq!(path, code.join(".git")),
        other => panic!("expected Unresolvable for the looping .git, got {other:?}"),
    }
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
    init(&default, &home, false, NO_TMUX).unwrap();
    assert_eq!(check(&default, &home), Ok(default.clone()));
    let again = init(&default, &home, false, NO_TMUX).unwrap();
    assert!(!again.changed(), "{}", again.render());
}
