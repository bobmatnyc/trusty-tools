//! Keep the harness's own files out of a registered project's `git status`
//! (#8511).
//!
//! Why: a legacy in-tree `.trusty-mpm-worktree` marker, or a `.trusty-mpm/`
//! scratch directory, makes a tree dirty in any project that does not ignore
//! them — `git worktree remove` without `--force` then refuses, Claude Code's
//! cleanup refuses, and `git add -A` can commit the marker (#8368). Relying on
//! each project's own `.gitignore` left most of the fleet uncovered.
//! What: [`ensure_harness_files_excluded`] appends [`HARNESS_EXCLUDE_ENTRIES`]
//! to `$(git rev-parse --git-common-dir)/info/exclude` — shared by every
//! worktree of the repository, local to the clone, and never a committed file —
//! the same approach `super::worktree_naming::ensure_worktrees_gitignored`
//! takes for `.worktrees/`. Both patterns are anchored to the tree root: the
//! harness writes only there, and an unanchored pattern would also hide a
//! project's own nested `sub/.trusty-mpm/` from `git status` and from the
//! clean-tree reclaim gate. `/.trusty-mpm/` is skipped when the project tracks
//! files under the top-level `.trusty-mpm/` — the same scope the pattern
//! covers. [`ensure_and_log`] is the best-effort wrapper registration calls.
//! Test: `exclude_entries_are_added_once`,
//! `trusty_mpm_dir_is_skipped_when_it_holds_tracked_files`,
//! `nested_trusty_mpm_files_stay_visible`.

use std::path::{Path, PathBuf};

/// The in-tree marker's name as an exclude pattern, anchored to the tree root.
const MARKER_ENTRY: &str = "/.trusty-mpm-worktree";
/// The harness scratch directory as an exclude pattern, anchored to the tree root.
const SCRATCH_DIR_ENTRY: &str = "/.trusty-mpm/";

/// Every pattern this module maintains, in the order it appends them.
pub(crate) const HARNESS_EXCLUDE_ENTRIES: [&str; 2] = [MARKER_ENTRY, SCRATCH_DIR_ENTRY];

/// Run `git -C <repo> <args>` and return trimmed stdout, or a named error.
fn git_stdout(repo: &Path, args: &[&str]) -> Result<String, String> {
    let out = trusty_common::git::command_in(repo)
        .args(args)
        .output()
        .map_err(|e| format!("git {} failed to spawn: {e}", args.join(" ")))?;
    if !out.status.success() {
        return Err(format!(
            "git {} failed ({}): {}",
            args.join(" "),
            out.status,
            String::from_utf8_lossy(&out.stderr).trim()
        ));
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim().to_string())
}

/// `$(git rev-parse --git-common-dir)/info/exclude` for `repo`.
///
/// Why: `info/exclude` in the COMMON dir applies to every worktree of the
/// repository, so one write at registration covers every agent tree.
/// What: resolves a relative answer against `repo`, as git reports it relative
/// to the `-C` directory.
fn exclude_file(repo: &Path) -> Result<PathBuf, String> {
    let common = PathBuf::from(git_stdout(repo, &["rev-parse", "--git-common-dir"])?);
    let common = if common.is_absolute() {
        common
    } else {
        repo.join(common)
    };
    Ok(common.join("info").join("exclude"))
}

/// Which [`HARNESS_EXCLUDE_ENTRIES`] `repo` still lacks, and the file they go in.
///
/// Why: the doctor repair must preview exactly what the apply writes.
/// What: an entry is pending when no line of the exclude file equals it
/// (trimmed). `/.trusty-mpm/` is never pending while `git ls-files` reports
/// tracked files under the top-level `.trusty-mpm/` (`:(top)`, the pattern's
/// own anchored scope) — excluding it there would hide the project's own new
/// files in that directory.
/// Test: `exclude_entries_are_added_once`,
/// `trusty_mpm_dir_is_skipped_when_it_holds_tracked_files`.
pub(crate) fn pending_excludes(repo: &Path) -> Result<(PathBuf, Vec<&'static str>), String> {
    let path = exclude_file(repo)?;
    let existing = match std::fs::read_to_string(&path) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => String::new(),
        Err(e) => return Err(format!("could not read {}: {e}", path.display())),
    };
    let mut pending = Vec::new();
    for entry in HARNESS_EXCLUDE_ENTRIES {
        if existing.lines().any(|l| l.trim() == entry) {
            continue;
        }
        if entry == SCRATCH_DIR_ENTRY
            && !git_stdout(repo, &["ls-files", "--", ":(top).trusty-mpm/"])?.is_empty()
        {
            continue;
        }
        pending.push(entry);
    }
    Ok((path, pending))
}

/// Append every pending [`HARNESS_EXCLUDE_ENTRIES`] line to `repo`'s shared
/// `info/exclude`, idempotently (#8511).
///
/// Why: see the module doc.
/// What: creates `info/` when absent, then appends only the pending entries,
/// adding a leading newline when the file does not end in one. Returns the
/// entries it added — empty when nothing was pending. Two concurrent first
/// calls can both append; git treats a repeated pattern as one, as
/// `ensure_worktrees_gitignored` documents for its own entry.
/// Test: `exclude_entries_are_added_once`,
/// `trusty_mpm_dir_is_skipped_when_it_holds_tracked_files`.
pub(crate) fn ensure_harness_files_excluded(repo: &Path) -> Result<Vec<&'static str>, String> {
    let (path, pending) = pending_excludes(repo)?;
    if pending.is_empty() {
        return Ok(pending);
    }
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)
            .map_err(|e| format!("could not create {}: {e}", dir.display()))?;
    }
    let existing = std::fs::read_to_string(&path).unwrap_or_default();
    let mut text = String::new();
    if !existing.is_empty() && !existing.ends_with('\n') {
        text.push('\n');
    }
    for entry in &pending {
        text.push_str(entry);
        text.push('\n');
    }
    use std::io::Write as _;
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&path)
        .and_then(|mut f| f.write_all(text.as_bytes()))
        .map_err(|e| format!("could not write {}: {e}", path.display()))?;
    Ok(pending)
}

/// Best-effort [`ensure_harness_files_excluded`] for registration and
/// provisioning: logs, never fails the caller (#8511).
pub(crate) fn ensure_and_log(repo: &Path) {
    match ensure_harness_files_excluded(repo) {
        Ok(added) if added.is_empty() => {}
        Ok(added) => tracing::info!(
            repo = %repo.display(),
            "added {} to the shared info/exclude (#8511)",
            added.join(", ")
        ),
        Err(e) => tracing::warn!(
            repo = %repo.display(),
            "harness files NOT excluded (non-fatal, #8511): {e}"
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn git(dir: &Path, args: &[&str]) {
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(dir)
            .args(args)
            .output()
            .expect("spawn git");
        assert!(
            out.status.success(),
            "git {args:?}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    /// A repository with one commit and one linked worktree.
    fn repo_with_worktree() -> (tempfile::TempDir, PathBuf, PathBuf) {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonicalize");
        let repo = root.join("repo");
        std::fs::create_dir(&repo).expect("mkdir");
        git(&repo, &["init", "-q", "--initial-branch=main"]);
        git(&repo, &["config", "user.email", "ci@test.invalid"]);
        git(&repo, &["config", "user.name", "CI"]);
        git(&repo, &["config", "commit.gpgsign", "false"]);
        std::fs::write(repo.join("README.md"), "x\n").expect("write");
        git(&repo, &["add", "README.md"]);
        git(&repo, &["commit", "-q", "-m", "base"]);
        let wt = root.join("wt");
        git(
            &repo,
            &[
                "worktree",
                "add",
                "-q",
                "-b",
                "agent",
                wt.to_str().expect("utf8"),
            ],
        );
        (tmp, repo, wt)
    }

    fn count(path: &Path, entry: &str) -> usize {
        std::fs::read_to_string(path)
            .unwrap_or_default()
            .lines()
            .filter(|l| l.trim() == entry)
            .count()
    }

    /// Both entries land once in the COMMON dir's exclude file, a second run
    /// adds nothing, and a linked worktree then ignores both.
    #[test]
    fn exclude_entries_are_added_once() {
        let (_tmp, repo, wt) = repo_with_worktree();
        let added = ensure_harness_files_excluded(&wt).expect("first run");
        assert_eq!(added, HARNESS_EXCLUDE_ENTRIES.to_vec());
        let again = ensure_harness_files_excluded(&repo).expect("second run");
        assert!(again.is_empty(), "a second run must add nothing: {again:?}");
        let exclude = repo.join(".git").join("info").join("exclude");
        for entry in HARNESS_EXCLUDE_ENTRIES {
            assert_eq!(
                count(&exclude, entry),
                1,
                "{entry} must appear exactly once"
            );
        }
        std::fs::write(wt.join(".trusty-mpm-worktree"), b"{}").expect("write marker");
        std::fs::create_dir(wt.join(".trusty-mpm")).expect("mkdir");
        std::fs::write(wt.join(".trusty-mpm").join("note.md"), b"x").expect("write");
        let status = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(["status", "--porcelain"])
            .output()
            .expect("status");
        assert!(
            status.stdout.is_empty(),
            "both entries must be ignored in a linked worktree: {}",
            String::from_utf8_lossy(&status.stdout)
        );
    }

    /// A project that tracks files under `.trusty-mpm/` keeps seeing new ones.
    #[test]
    fn trusty_mpm_dir_is_skipped_when_it_holds_tracked_files() {
        let (_tmp, repo, _wt) = repo_with_worktree();
        std::fs::create_dir(repo.join(".trusty-mpm")).expect("mkdir");
        std::fs::write(repo.join(".trusty-mpm").join("INSTRUCTIONS.md"), b"x").expect("write");
        git(&repo, &["add", ".trusty-mpm/INSTRUCTIONS.md"]);
        git(&repo, &["commit", "-q", "-m", "track"]);
        let added = ensure_harness_files_excluded(&repo).expect("run");
        assert_eq!(added, vec![MARKER_ENTRY]);
        let exclude = repo.join(".git").join("info").join("exclude");
        assert_eq!(count(&exclude, SCRATCH_DIR_ENTRY), 0);
    }

    /// Finding 4 (#8511 review): the patterns cover the tree root only, so a
    /// project's nested `.trusty-mpm/` and marker-named files still show as
    /// untracked work.
    #[test]
    fn nested_trusty_mpm_files_stay_visible() {
        let (_tmp, _repo, wt) = repo_with_worktree();
        ensure_harness_files_excluded(&wt).expect("run");
        let sub = wt.join("sub");
        std::fs::create_dir_all(sub.join(".trusty-mpm")).expect("mkdir");
        std::fs::write(sub.join(".trusty-mpm").join("work.md"), b"x").expect("write");
        std::fs::write(sub.join(".trusty-mpm-worktree"), b"x").expect("write");
        std::fs::create_dir(wt.join(".trusty-mpm")).expect("mkdir");
        std::fs::write(wt.join(".trusty-mpm").join("note.md"), b"x").expect("write");
        let out = std::process::Command::new("git")
            .arg("-C")
            .arg(&wt)
            .args(["status", "--porcelain", "--untracked-files=all"])
            .output()
            .expect("status");
        let status = String::from_utf8_lossy(&out.stdout);
        assert!(
            status.contains("sub/.trusty-mpm/work.md"),
            "nested scratch dir hidden: {status}"
        );
        assert!(
            status.contains("sub/.trusty-mpm-worktree"),
            "nested marker-named file hidden: {status}"
        );
        assert!(
            !status.contains(" .trusty-mpm/"),
            "top-level scratch dir not excluded: {status}"
        );
    }
}
