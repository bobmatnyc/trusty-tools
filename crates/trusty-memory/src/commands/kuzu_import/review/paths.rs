//! Where a review export may land, and the guard that refuses the rest (#8624).
//!
//! Why: the masked export still derives from private memory, so it must never
//! land where git could commit it or where the palace would read it. Ruling Q4
//! fixes the root under `$HOME`; no flag moves it.
//! What: [`default_root`] names the root, [`run_name`] one run's directory,
//! and [`guard_destination`] resolves a destination that may not exist yet and
//! refuses it inside a git work tree or a protected root.
//! Test: `review::paths_tests`.

use std::ffi::OsString;
use std::io;
use std::path::{Component, Path, PathBuf};

use super::ReviewError;

/// `~/.trusty-tools/kuzu-review`, the only root a run directory is made under.
///
/// Test: `default_root_is_under_home`.
pub(crate) fn default_root() -> Result<PathBuf, ReviewError> {
    let home = dirs::home_dir().ok_or(ReviewError::NoHome)?;
    Ok(home.join(".trusty-tools").join("kuzu-review"))
}

/// A new run directory name: UTC time plus 8 random hex digits.
///
/// Test: `run_names_are_distinct`.
pub(crate) fn run_name() -> String {
    let stamp = chrono::Utc::now().format("%Y%m%dT%H%M%SZ");
    let tag = uuid::Uuid::new_v4().simple().to_string();
    format!("{stamp}-{}", tag.get(..8).unwrap_or(&tag))
}

fn refuse(path: &Path, reason: impl Into<String>) -> ReviewError {
    ReviewError::Guard {
        path: path.to_path_buf(),
        reason: reason.into(),
    }
}

/// `path` with every symlink and `..` resolved, though its tail may not exist.
///
/// Why (#8624): `canonicalize` fails on a path that does not exist yet, and a
/// run directory never exists before the export makes it.
/// What: canonicalizes the nearest existing ancestor, then appends the
/// missing tail. The tail must be plain names: a `.` or `..` there cannot be
/// resolved against the disk, so it is refused. An entry that exists but does
/// not resolve (a dangling symlink, a loop) is refused, never skipped.
/// Test: `guard_refuses_dotdot_into_a_protected_root`,
/// `guard_refuses_dotdot_in_the_missing_tail`,
/// `guard_refuses_a_dangling_symlink`.
pub(crate) fn resolve(path: &Path) -> Result<PathBuf, ReviewError> {
    if !path.is_absolute() {
        return Err(refuse(path, "not an absolute path"));
    }
    let mut tail: Vec<OsString> = Vec::new();
    let mut cur = path;
    loop {
        match std::fs::symlink_metadata(cur) {
            Ok(_) => {
                let mut base = std::fs::canonicalize(cur).map_err(|e| {
                    refuse(path, format!("{} does not resolve: {e}", cur.display()))
                })?;
                base.extend(tail.iter().rev());
                return Ok(base);
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                match cur.components().next_back() {
                    Some(Component::Normal(name)) => tail.push(name.to_os_string()),
                    _ => return Err(refuse(path, "a `.` or `..` in a part that does not exist")),
                }
                cur = cur
                    .parent()
                    .ok_or_else(|| refuse(path, "no existing ancestor"))?;
            }
            Err(e) => return Err(refuse(path, format!("{}: {e}", cur.display()))),
        }
    }
}

/// The nearest ancestor of `path` (itself included) that holds a `.git`.
fn git_work_tree(path: &Path) -> Option<&Path> {
    path.ancestors()
        .find(|a| a.join(".git").symlink_metadata().is_ok())
}

/// `dest` resolved, or an error when it lies where an export must not go.
///
/// Why (#8624): an export inside a repo can be committed, and one inside the
/// palace data root can be read back as palace data.
/// What: [`resolve`]s `dest` and each of `protected`, then refuses `dest` when
/// any ancestor holds a `.git` entry (a work tree or a linked worktree) or it
/// lies inside a protected root. Comparison is by path components, after
/// symlinks and `..` are resolved on both sides.
/// Test: `guard_refuses_a_destination_inside_a_git_work_tree_before_it_exists`,
/// `guard_refuses_a_symlink_into_a_protected_root`,
/// `guard_refuses_dotdot_into_a_protected_root`,
/// `guard_admits_a_plain_destination_and_resolves_it`.
pub(crate) fn guard_destination(
    dest: &Path,
    protected: &[PathBuf],
) -> Result<PathBuf, ReviewError> {
    let resolved = resolve(dest)?;
    if let Some(top) = git_work_tree(&resolved) {
        let reason = format!("it is inside the git work tree {}", top.display());
        return Err(refuse(&resolved, reason));
    }
    for root in protected {
        let root = resolve(root)?;
        if resolved.starts_with(&root) {
            let reason = format!("it is inside the palace data root {}", root.display());
            return Err(refuse(&resolved, reason));
        }
    }
    Ok(resolved)
}
