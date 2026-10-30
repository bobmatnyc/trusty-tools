//! Refuse an Architect directory that fleet init must never own (#8436).
//!
//! Why: `tm fleet init` writes into its directory and always adds that
//! directory to the user-level `[supervisor] projects` allowlist (ruling Q6).
//! A mistaken or hostile `--dir` such as `$HOME` or a checkout with a remote
//! would scaffold into, and grant the supervisor profile over, a tree the
//! Architect must never own (code-critic BLOCK on #8436 P2).
//! What: [`check`] canonicalizes the target and refuses it, before any write,
//! with a distinct [`DirRefusal`] per case. Every fact it cannot establish is
//! a refusal, never a warning: the check fails closed.
//! Test: `preflight_tests.rs`.

use std::fmt;
use std::io;
use std::os::unix::fs::MetadataExt;
use std::path::{Component, Path, PathBuf};
use std::process::Command;

use trusty_common::crate_config::{crate_config_path_at, load_at};
use trusty_common::workspace_layout::expand_tilde;
use trusty_mpm::core::child_repo_scan::{
    ChildRepoScan, DeviceOf, device_of, scan_for_child_repo_exhaustive_with,
};
use trusty_mpm::core::trusty_tools_config::{CRATE_NAME, TrustyToolsConfig, WORKSPACE_ROOT_ENV};

use super::user_config_path;

/// The built-in projects root under the home directory (`~/trusty-mpm-projects`).
const PROJECTS_ROOT_DIR: &str = "trusty-mpm-projects";

/// The one child repository fleet init accepts, `<dir>/local` (#8878 R1).
pub(crate) const PRIVATE_REPO_DIR: &str = "local";

/// Directories the workspace-parent scan may visit under one fleet directory.
///
/// Why: #10355 — the Architect's own directory (scripts, inbox, evidence,
/// `.claude/`) outgrew the shared 256-directory budget, and the exhausted scan
/// refused every relaunch. The scan guards a supervisor grant, so exhaustion
/// stays a refusal; the budget is sized so an ordinary Architect directory
/// finishes, while a tree too large to check still refuses in bounded time.
/// What: 64x [`trusty_mpm::core::child_repo_scan::WORKSPACE_SCAN_BUDGET`];
/// each visit is one `read_dir`, one `stat` for `st_dev` and one `lstat` per
/// child directory. Since #10457 skip-listed trees such as `node_modules`
/// count against it too.
/// Test: `an_architect_dir_wider_than_the_workspace_budget_passes`,
/// `a_tree_wider_than_the_fleet_budget_refuses_in_bounded_time`.
pub(crate) const FLEET_SCAN_BUDGET: usize = 16_384;

/// Git variables that point a git command at a repository other than `-C <dir>`.
const GIT_REDIRECT_VARS: &[&str] = &["GIT_DIR", "GIT_WORK_TREE", "GIT_COMMON_DIR"];

/// Why fleet init refuses a directory. Each variant names what to do instead.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DirRefusal {
    /// A fact about the path could not be established (fail closed).
    Unresolvable { path: PathBuf, reason: String },
    /// `/` or the root of a mounted filesystem.
    FilesystemRoot(PathBuf),
    /// The home directory itself.
    Home(PathBuf),
    /// A directory holding other projects: a projects root, an ancestor of the
    /// home directory or a projects root, or a directory with a repository in it.
    WorkspaceParent { path: PathBuf, reason: String },
    /// The tm user config directory (`~/.trusty-mpm`, which holds the
    /// `[supervisor] projects` allowlist), a path inside it, or one holding it.
    ConfigDir { path: PathBuf, config_dir: PathBuf },
    /// A path below another git work tree.
    InsideWorkTree { path: PathBuf, work_tree: PathBuf },
    /// An existing repository with a remote configured.
    HasRemote { path: PathBuf, remotes: String },
    /// `git remote` could not list the repository's remotes.
    GitProbeFailed { path: PathBuf, detail: String },
}

impl fmt::Display for DirRefusal {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        const FIX: &str = "omit --dir to use ~/trusty-mpm-projects/architect, or pass a new, \
                           empty directory";
        match self {
            Self::Unresolvable { path, reason } => write!(
                f,
                "refusing {}: cannot check it ({reason}); fix the path or its permissions, or {FIX}",
                path.display()
            ),
            Self::FilesystemRoot(p) => write!(
                f,
                "refusing {}: it is a filesystem root; {FIX}",
                p.display()
            ),
            Self::Home(p) => write!(
                f,
                "refusing {}: it is your home directory; {FIX}",
                p.display()
            ),
            Self::WorkspaceParent { path, reason } => write!(
                f,
                "refusing {}: it is a workspace parent ({reason}); {FIX}",
                path.display()
            ),
            Self::ConfigDir { path, config_dir } => write!(
                f,
                "refusing {}: it overlaps the tm config directory {}, which holds the \
                 `[supervisor] projects` allowlist; {FIX}",
                path.display(),
                config_dir.display()
            ),
            Self::InsideWorkTree { path, work_tree } => write!(
                f,
                "refusing {}: it is inside the git work tree {}; {FIX} outside any repository",
                path.display(),
                work_tree.display()
            ),
            Self::HasRemote { path, remotes } => write!(
                f,
                "refusing {}: it is a git repository with remote(s) {remotes}; the Architect's \
                 repository must start with no remote; {FIX}",
                path.display()
            ),
            Self::GitProbeFailed { path, detail } => write!(
                f,
                "refusing {}: `git remote` failed ({detail}); repair the repository, or {FIX}",
                path.display()
            ),
        }
    }
}

impl std::error::Error for DirRefusal {}

/// Refuse `dir` unless fleet init may own it; return its canonical path.
///
/// Why: see the module doc. Every caller writes through the returned path, so
/// what was checked is what gets written, with no symlink left to re-resolve.
/// What: in order, refuses (1) a path that cannot be canonicalized; (2) `/` or
/// a mount root; (3) `home`; (4) an ancestor of `home`; (5) the tm config
/// directory (the parent of [`user_config_path`]), a path inside it, or one
/// holding it; (6) a workspace parent, meaning any candidate projects root
/// (`<home>/trusty-mpm-projects`, the configured root, the
/// `TRUSTY_MPM_WORKSPACE_ROOT` value) or an ancestor of one, or a directory
/// whose subtree holds a repository or cannot be scanned — except one private
/// repository at `<dir>/local` (see [`check_private_repo`]) and the
/// directory's own linked worktrees (see [`own_worktrees`]); (7) a path with
/// a `.git` entry in any strict ancestor; (8) a repository whose `git remote`
/// lists a remote or fails. A trusty-mpm config that exists but will not
/// load is a refusal; a missing one is the default. A fresh directory, and a
/// repository fleet init created (a no-remote repo at the target), pass.
/// Test: `the_home_directory_is_refused_however_it_is_spelled`,
/// `a_malformed_tm_config_refuses`, `the_tm_config_directory_is_refused`,
/// `a_repository_with_a_remote_is_refused`, `a_failing_remote_listing_refuses`,
/// `the_default_and_a_fresh_directory_pass`,
/// `a_private_local_repository_without_a_remote_passes`, and the rest of
/// `preflight_tests.rs`.
pub(crate) fn check(dir: &Path, home: &Path) -> Result<PathBuf, DirRefusal> {
    check_with(dir, home, device_of)
}

/// [`check`] with the `st_dev` reader the repository scan uses.
///
/// Why: #10457 — a test cannot mount a filesystem, so it fakes the device of
/// `<dir>/local` to prove a mount there refuses.
/// Test: `a_mount_point_at_local_is_refused`.
pub(crate) fn check_with(dir: &Path, home: &Path, device: DeviceOf) -> Result<PathBuf, DirRefusal> {
    let target = resolve(dir)?;
    if is_filesystem_root(&target)? {
        return Err(DirRefusal::FilesystemRoot(target));
    }
    let home = std::fs::canonicalize(home).map_err(|e| unresolvable(home, &e))?;
    if target == home {
        return Err(DirRefusal::Home(target));
    }
    if home.starts_with(&target) {
        return Err(DirRefusal::WorkspaceParent {
            reason: format!("it contains your home directory {}", home.display()),
            path: target,
        });
    }
    refuse_config_dir(&target, &home)?;
    refuse_workspace_parent(&target, &home, device)?;
    refuse_enclosing_work_tree(&target)?;
    refuse_remote(&target)?;
    Ok(target)
}

/// `path` made absolute with every symlink and `..` resolved.
///
/// What: canonicalizes the deepest existing ancestor and appends the missing
/// tail. A missing tail holding `..`, a dangling symlink, or any error other
/// than "not found" is [`DirRefusal::Unresolvable`].
fn resolve(path: &Path) -> Result<PathBuf, DirRefusal> {
    let abs = std::path::absolute(path).map_err(|e| unresolvable(path, &e))?;
    let mut tail = Vec::new();
    let mut cur = abs.as_path();
    loop {
        match std::fs::canonicalize(cur) {
            Ok(mut real) => {
                real.extend(tail.iter().rev());
                return Ok(real);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                // A dangling symlink exists as an entry but not as a target.
                if std::fs::symlink_metadata(cur).is_ok() {
                    return Err(DirRefusal::Unresolvable {
                        path: abs.clone(),
                        reason: format!("{} is a dangling symlink", cur.display()),
                    });
                }
                let (Some(parent), Some(Component::Normal(name))) =
                    (cur.parent(), cur.components().next_back())
                else {
                    return Err(DirRefusal::Unresolvable {
                        path: abs.clone(),
                        reason: "`..` under a directory that does not exist".to_owned(),
                    });
                };
                tail.push(name.to_owned());
                cur = parent;
            }
            Err(e) => return Err(unresolvable(&abs, &e)),
        }
    }
}

/// `/`, or an existing directory on a different device from its parent.
fn is_filesystem_root(target: &Path) -> Result<bool, DirRefusal> {
    let Some(parent) = target.parent() else {
        return Ok(true);
    };
    let own = match std::fs::metadata(target) {
        Ok(meta) => meta.dev(),
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(false),
        Err(e) => return Err(unresolvable(target, &e)),
    };
    let above = std::fs::metadata(parent).map_err(|e| unresolvable(parent, &e))?;
    Ok(own != above.dev())
}

/// Refuse the tm config directory, a path inside it, or one holding it.
// #8436 critic M1: `~/.trusty-mpm` holds the `[supervisor] projects` allowlist.
fn refuse_config_dir(target: &Path, home: &Path) -> Result<(), DirRefusal> {
    let config_path = user_config_path(home);
    let Some(config_dir) = config_path.parent() else {
        return Ok(());
    };
    let config_dir = resolve(config_dir)?;
    if target.starts_with(&config_dir) || config_dir.starts_with(target) {
        return Err(DirRefusal::ConfigDir {
            path: target.to_path_buf(),
            config_dir,
        });
    }
    Ok(())
}

/// Every projects root fleet init must not own: the built-in default, the
/// configured root and the env override, not only the one precedence picks.
///
/// What: reads `<home>/.trusty-tools/trusty-mpm/config.yaml` with the fallible
/// loader. A missing file adds no root; one that cannot be read or parsed is
/// [`DirRefusal::Unresolvable`] naming the file.
// #8436 critic H1: `TrustyToolsConfig::load` falls back to the default on a
// malformed file, which dropped the configured root from the check.
fn projects_roots(target: &Path, home: &Path) -> Result<Vec<PathBuf>, DirRefusal> {
    let expand = |raw: Option<&str>| {
        raw.map(str::trim)
            .filter(|raw| !raw.is_empty())
            .map(|raw| expand_tilde(raw, home))
    };
    let mut roots = vec![home.join(PROJECTS_ROOT_DIR)];
    let config = crate_config_path_at(home, CRATE_NAME);
    match load_at::<TrustyToolsConfig>(&config) {
        Ok(None) => {}
        Ok(Some(cfg)) => roots.extend(expand(cfg.workspace_root_template.as_deref())),
        Err(e) => {
            return Err(DirRefusal::Unresolvable {
                path: target.to_path_buf(),
                reason: format!("the trusty-mpm config did not load: {e}"),
            });
        }
    }
    roots.extend(expand(std::env::var(WORKSPACE_ROOT_ENV).ok().as_deref()));
    Ok(roots)
}

/// Refuse any projects root or an ancestor of one, or a directory with a
/// repository in its subtree (the #7673 workspace-parent scan).
///
/// What: the scan is the exhaustive rule (#10457): skip-listed trees are
/// walked, and an escaping symlink, a mount point or an exhausted budget is
/// [`DirRefusal::Unresolvable`].
/// Test: `a_repository_deep_in_a_skip_listed_dir_is_refused`,
/// `a_repository_behind_a_symlinked_dir_is_refused`,
/// `a_symlink_escaping_the_dir_refuses_without_a_repository`,
/// `a_large_node_modules_refuses_in_bounded_time`,
/// `a_pnpm_style_node_modules_inside_the_dir_passes`, `a_mount_point_at_local_is_refused`.
fn refuse_workspace_parent(target: &Path, home: &Path, device: DeviceOf) -> Result<(), DirRefusal> {
    let parent = |reason: String| DirRefusal::WorkspaceParent {
        path: target.to_path_buf(),
        reason,
    };
    for root in projects_roots(target, home)? {
        let root = resolve(&root)?;
        if root.starts_with(target) {
            return Err(parent(format!(
                "it holds the projects root {}",
                root.display()
            )));
        }
    }
    // #8878 R1: a private `local/` repository is checked on its own, and the
    // scan still looks at everything beside it.
    // #10355: fleet budget, not the shared 256; exhaustion still refuses below.
    // #10457: exhaustive rule; the shallow one missed deep and symlinked repos.
    let local = private_repo(target)?;
    // #10262: the target's own linked worktrees are left out like `local/`,
    // and each is scanned on its own below.
    let worktrees = own_worktrees(target);
    let except: Vec<&Path> = local
        .iter()
        .chain(&worktrees)
        .map(PathBuf::as_path)
        .collect();
    refuse_child_repo(target, target, &except, device, String::new())?;
    // #10457: after the outer scan, which compares `local`'s device with the
    // target's, so no `git` command runs inside a foreign mount.
    if let Some(local) = &local {
        check_private_repo(target, local, device)?;
    }
    for worktree in &worktrees {
        let within = format!(", nested in its linked worktree {}", worktree.display());
        refuse_child_repo(target, worktree, &except, device, within)?;
    }
    Ok(())
}

/// Refuse `target` when the exhaustive scan of `root`, leaving out `except`,
/// finds a repository or does not finish.
///
/// What: a found repository is [`DirRefusal::WorkspaceParent`] whose reason
/// ends in `within`, plus the `git worktree` fix when the repository is a
/// linked worktree; an unfinished scan is [`DirRefusal::Unresolvable`] of `root`.
fn refuse_child_repo(
    target: &Path,
    root: &Path,
    except: &[&Path],
    device: DeviceOf,
    within: String,
) -> Result<(), DirRefusal> {
    match scan_for_child_repo_exhaustive_with(root, except, FLEET_SCAN_BUDGET, device) {
        ChildRepoScan::Clear => Ok(()),
        ChildRepoScan::Found(repo) => Err(DirRefusal::WorkspaceParent {
            path: target.to_path_buf(),
            reason: format!(
                "it contains the repository {}{within}{}",
                repo.display(),
                worktree_fix(&repo)
            ),
        }),
        ChildRepoScan::Incomplete(why) => Err(DirRefusal::Unresolvable {
            path: root.to_path_buf(),
            reason: format!("the scan for repositories inside it did not finish: {why}"),
        }),
    }
}

/// The fix for a refused repository that is a linked worktree (its `.git` is
/// a file); empty for any other repository.
// #10262: the removal keeps the branch; `--force` would drop uncommitted work.
fn worktree_fix(repo: &Path) -> String {
    match std::fs::symlink_metadata(repo.join(".git")) {
        Ok(meta) if meta.is_file() => format!(
            "; it is a linked git worktree: find it with `git worktree list` in its repository \
             and remove it with `git worktree remove {}` (no --force; its branch is kept)",
            repo.display()
        ),
        _ => String::new(),
    }
}

/// Linked worktrees of `target`'s own repository that sit inside `target`.
///
/// Why: #10262 — Claude Code agent worktrees of the Architect's own repository
/// (`.claude/worktrees/agent-*`) are that project, not a child one, yet the
/// scan refused them, so a stopped Architect could not relaunch. The grant
/// guard is about other projects, so only a worktree that is provably the
/// target's own is left out of the scan.
/// What: for each record `<target>/.git/worktrees/<name>` (a real `.git`
/// directory only), reads its `gitdir` back pointer. The worktree counts when
/// it resolves to a directory strictly inside `target` other than `local/`,
/// and its `.git` is a regular file whose `gitdir:` resolves to that same
/// record. Anything unreadable or unmatched is left out, so the scan still
/// sees it and refuses: the check fails closed.
/// Test: `a_linked_worktree_of_the_dirs_own_repository_passes`,
/// `a_worktree_that_is_not_the_dirs_own_still_refuses`.
fn own_worktrees(target: &Path) -> Vec<PathBuf> {
    let git_dir = target.join(".git");
    if !std::fs::symlink_metadata(&git_dir).is_ok_and(|meta| meta.is_dir()) {
        return Vec::new();
    }
    let records = git_dir.join("worktrees");
    let Ok(entries) = std::fs::read_dir(&records) else {
        return Vec::new();
    };
    entries
        .filter_map(Result::ok)
        .filter_map(|entry| own_worktree(target, &records, &entry.path()))
        .collect()
}

/// The worktree `record` describes, when it passes [`own_worktrees`]' test.
fn own_worktree(target: &Path, records: &Path, record: &Path) -> Option<PathBuf> {
    let record = std::fs::canonicalize(record).ok()?;
    // A `.git/worktrees` symlinked elsewhere holds no record of this repository.
    if record.parent() != Some(records) {
        return None;
    }
    let back = std::fs::read_to_string(record.join("gitdir")).ok()?;
    let back = record.join(back.trim());
    if back.file_name()? != ".git" {
        return None;
    }
    let worktree = std::fs::canonicalize(back.parent()?).ok()?;
    let inside = worktree.strip_prefix(target).ok()?;
    if inside.as_os_str().is_empty() || inside == Path::new(PRIVATE_REPO_DIR) {
        return None;
    }
    let git = worktree.join(".git");
    if !std::fs::symlink_metadata(&git).ok()?.is_file() {
        return None;
    }
    let pointer = std::fs::read_to_string(&git).ok()?;
    let pointer = pointer.trim().strip_prefix("gitdir:")?.trim();
    (std::fs::canonicalize(worktree.join(pointer)).ok()? == record).then_some(worktree)
}

/// `<target>/local` when it is a real directory whose `.git` is a real
/// directory; `None` otherwise, which leaves it to the ordinary scan. A stat
/// error other than not-found is [`DirRefusal::Unresolvable`].
///
/// Why: #8878 R1 — the supervisor keeps a private, remote-less repository in
/// `local/`. A symlinked `local`, or a `.git` file or symlink (a worktree,
/// submodule or redirect), gets no exception, so the scan refuses it.
/// Test: `a_symlinked_local_repository_is_refused`,
/// `a_private_local_repository_without_a_remote_passes`,
/// `an_unstatable_private_repository_refuses`.
fn private_repo(target: &Path) -> Result<Option<PathBuf>, DirRefusal> {
    let local = target.join(PRIVATE_REPO_DIR);
    for path in [local.clone(), local.join(".git")] {
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if meta.is_dir() => {}
            Ok(_) => return Ok(None),
            Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(e) => return Err(unresolvable(&path, &e)),
        }
    }
    Ok(Some(local))
}

/// Refuse the private repository `local` of `target` unless it has no remote
/// and holds no repository of its own.
///
/// Why: #8878 R1 (owner ruling 2026-09-29 22:23Z) — `local/` is the one
/// child repository the Architect may own; a remote would publish it, and a
/// nested repository is a project the exception must not cover.
/// What: [`DirRefusal::WorkspaceParent`] of `target` when a scan of `local`
/// finds a repository or `git remote` lists one; an incomplete scan is
/// [`DirRefusal::Unresolvable`] and a failing listing
/// [`DirRefusal::GitProbeFailed`].
/// Test: `a_private_local_repository_with_a_remote_is_refused`,
/// `a_repository_nested_in_the_private_local_repository_is_refused`,
/// `a_failing_remote_listing_on_the_private_repository_refuses`.
fn check_private_repo(target: &Path, local: &Path, device: DeviceOf) -> Result<(), DirRefusal> {
    let parent = |reason: String| DirRefusal::WorkspaceParent {
        path: target.to_path_buf(),
        reason,
    };
    match scan_for_child_repo_exhaustive_with(local, &[], FLEET_SCAN_BUDGET, device) {
        ChildRepoScan::Clear => {}
        ChildRepoScan::Found(repo) => {
            return Err(parent(format!(
                "it contains the repository {}, nested in its private repository {}",
                repo.display(),
                local.display()
            )));
        }
        ChildRepoScan::Incomplete(why) => {
            return Err(DirRefusal::Unresolvable {
                path: local.to_path_buf(),
                reason: format!("the scan for repositories inside it did not finish: {why}"),
            });
        }
    }
    let remotes = list_remotes(local)?;
    if remotes.is_empty() {
        return Ok(());
    }
    Err(parent(format!(
        "its private repository {} has remote(s) {}; only a `{PRIVATE_REPO_DIR}/` repository \
         with no remote is accepted",
        local.display(),
        remotes.join(", ")
    )))
}

/// Refuse a target with a `.git` entry in any strict ancestor.
fn refuse_enclosing_work_tree(target: &Path) -> Result<(), DirRefusal> {
    for ancestor in target.ancestors().skip(1) {
        let git = ancestor.join(".git");
        match git.try_exists() {
            Ok(false) => {}
            Ok(true) => {
                return Err(DirRefusal::InsideWorkTree {
                    path: target.to_path_buf(),
                    work_tree: ancestor.to_path_buf(),
                });
            }
            Err(e) => return Err(unresolvable(&git, &e)),
        }
    }
    Ok(())
}

/// Refuse a repository at `target` whose `git remote` lists a remote or fails.
fn refuse_remote(target: &Path) -> Result<(), DirRefusal> {
    match target.join(".git").try_exists() {
        Ok(false) => return Ok(()),
        Ok(true) => {}
        Err(e) => return Err(unresolvable(&target.join(".git"), &e)),
    }
    let remotes = list_remotes(target)?;
    if remotes.is_empty() {
        return Ok(());
    }
    Err(DirRefusal::HasRemote {
        path: target.to_path_buf(),
        remotes: remotes.join(", "),
    })
}

/// The remotes `git remote` lists for the repository at `repo`.
fn list_remotes(repo: &Path) -> Result<Vec<String>, DirRefusal> {
    let probe_failed = |detail: String| DirRefusal::GitProbeFailed {
        path: repo.to_path_buf(),
        detail,
    };
    let out = git()
        .arg("-C")
        .arg(repo)
        .arg("remote")
        .output()
        .map_err(|e| probe_failed(format!("cannot run git: {e}")))?;
    if !out.status.success() {
        return Err(probe_failed(
            String::from_utf8_lossy(&out.stderr).trim().to_owned(),
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout)
        .split_whitespace()
        .map(str::to_owned)
        .collect())
}

/// A `git` command that ignores any inherited `GIT_DIR`-style redirect.
pub(super) fn git() -> Command {
    let mut cmd = Command::new("git");
    for var in GIT_REDIRECT_VARS {
        cmd.env_remove(var);
    }
    cmd
}

fn unresolvable(path: &Path, err: &io::Error) -> DirRefusal {
    DirRefusal::Unresolvable {
        path: path.to_path_buf(),
        reason: err.to_string(),
    }
}

#[cfg(test)]
#[path = "preflight_tests.rs"]
mod tests;
