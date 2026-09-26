//! Gitignored output a worktree removal would destroy (#8534).
//!
//! Why: every automatic removal route gates on
//! [`super::worktree_safety::inspect_dirt`], which counts modified tracked files
//! and untracked files that are NOT gitignored, and then runs `git worktree
//! remove` — which deletes every gitignored file, with or without `--force`. An
//! agent with zero commits whose run wrote its results into a gitignored
//! directory therefore read as clean, and the results went with the tree.
//!
//! What — THE RULE: a gitignored entry does not block removal when
//! - it is harness bookkeeping tm or Claude Code writes into every tree
//!   ([`is_harness_path`]: `.trusty-mpm/`, the ownership marker,
//!   `.claude/settings*.json` and their sidecars, and the scaffold paths);
//! - the directory git matched is build or tool output: its last component is
//!   in [`DISPOSABLE_DIR_NAMES`] or its trailing path in
//!   [`DISPOSABLE_DIR_PATHS`], or its basename is OS or bytecode litter
//!   ([`is_regenerable`]); or
//! - it is, or sits inside, a directory a cache tool tagged as its own
//!   ([`under_cache_dir`]: a `CACHEDIR.TAG`, or cargo's `.rustc_info.json`).
//!
//! Every other gitignored entry is kept output and blocks removal. Only the
//! entry's LAST component is matched by name, so `build/out.json` under a
//! tracked `build/` and a user's `target-analysis/` are kept.
//!
//! FAIL-SAFE: a git error, a path this check cannot spell, or an unreadable
//! directory is an `Err`, and every caller keeps the tree on `Err`.
//! Where it is enforced: the agent reap (gate 5a), the shared remover
//! `decommission::remove_registered_worktree` (decommission, merged-PR
//! reclaim, orphan prune), and the `tm pr cleanup` probe
//! [`inspect_dirt_with_ignored_output`].
//! Test: `worktree_ignored_output_tests`.

use std::io::Read;
use std::path::Path;

use tracing::warn;

use super::worktree_nested::DISPOSABLE_DIR_NAMES;
use super::worktree_safety::{DirtyWorktree, DirtyWorktreePolicy, git_stdout, inspect_dirt};
use crate::core::scaffold_gitignore::SCAFFOLD_IGNORED_PATHS;

/// Build-output directories named by more than one trailing component.
///
/// Why: `gen/` alone is too common a user directory name to excuse, but Tauri
/// regenerates `src-tauri/gen/` on every build.
const DISPOSABLE_DIR_PATHS: &[&str] = &["src-tauri/gen"];

/// File basenames that are OS or editor litter, never run output.
const REGENERABLE_FILES: &[&str] = &[".DS_Store", "Thumbs.db"];

/// Harness paths beyond [`SCAFFOLD_IGNORED_PATHS`] that tm writes into a tree.
/// A trailing `*` matches the rest of that path segment.
const HARNESS_PATHS: &[&str] = &[
    ".trusty-mpm/",
    ".claude/settings.json*",
    ".claude/settings.local.json*",
];

/// The Cache Directory Tagging signature (<https://bford.info/cachedir/>).
const CACHEDIR_TAG_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// The file cargo writes into the root of every target directory it builds in.
const CARGO_TARGET_MARKER: &str = ".rustc_info.json";

/// `git status` listing gitignored entries, collapsed to the matching directory.
const IGNORED_STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain",
    "--ignored=matching",
    "--untracked-files=normal",
    "--ignore-submodules=none",
];

/// Stop counting files inside one kept directory past this many.
const FILE_COUNT_CAP: usize = 100_000;

/// Gitignored output found in a worktree that the rule does not excuse.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct IgnoredOutput {
    /// Files under the kept entries, capped per directory at [`FILE_COUNT_CAP`].
    pub files: usize,
    /// The first kept entry, repo-relative, for the operator to look at.
    pub first: String,
}

/// Is the directory or file git matched, by its own name, regenerable output?
///
/// Why: matching any path component excused `build/out.json` under a tracked
/// `build/` (#8534 critic round 2). In `--ignored=matching` mode the entry IS
/// what git matched, so its last component is the one that decides.
/// Test: `rule_excuses_build_output_and_keeps_run_output`.
pub(crate) fn is_regenerable(entry: &str) -> bool {
    let entry = entry.trim_end_matches('/');
    let basename = entry.rsplit('/').next().unwrap_or(entry);
    DISPOSABLE_DIR_NAMES.contains(&basename)
        || DISPOSABLE_DIR_PATHS
            .iter()
            .any(|p| entry == *p || entry.ends_with(&format!("/{p}")))
        || REGENERABLE_FILES.contains(&basename)
        || basename.ends_with(".pyc")
}

/// Is this repo-relative path harness bookkeeping, not run output?
///
/// Why: a managed tree always holds tm's gitignored harness files; without
/// this excuse every decommission of one would refuse (#8534 critic round 2).
/// Test: `harness_paths_are_excused_and_look_alikes_are_not`.
pub(crate) fn is_harness_path(rel: &str) -> bool {
    let rel = rel.trim_end_matches('/');
    rel == super::decommission::WORKTREE_SENTINEL_FILE
        || HARNESS_PATHS
            .iter()
            .chain(SCAFFOLD_IGNORED_PATHS)
            .any(|pattern| path_matches(pattern, rel))
}

/// `rel` is `pattern` or lies beneath it; a `*` matches the rest of one segment.
fn path_matches(pattern: &str, rel: &str) -> bool {
    let pattern = pattern.trim_end_matches('/');
    match pattern.split_once('*') {
        None => rel
            .strip_prefix(pattern)
            .is_some_and(|rest| rest.is_empty() || rest.starts_with('/')),
        Some((head, tail)) => rel.strip_prefix(head).is_some_and(|rest| {
            let segment = rest.split('/').next().unwrap_or(rest);
            segment.ends_with(tail)
        }),
    }
}

/// Is `rel`, or a directory above it inside `root`, a tagged cache directory?
///
/// Why: pytest, mypy and ruff tag their caches and ignore their own contents,
/// so git lists `.pytest_cache/README.md` rather than the directory; and an
/// agent's `CARGO_TARGET_DIR=target-<n>` is cargo output under a name no list
/// can hold. The tag is written by the tool, never by a user.
/// Test: `tagged_cache_dirs_are_excused_by_content_not_name`.
fn under_cache_dir(root: &Path, rel: &str) -> bool {
    let mut prefix = root.to_path_buf();
    rel.split('/').filter(|p| !p.is_empty()).any(|part| {
        prefix.push(part);
        is_tagged_cache_dir(&prefix)
    })
}

/// Does `dir` hold a valid `CACHEDIR.TAG` or cargo's target-root marker?
/// Anything unreadable answers `false`, which keeps the entry.
fn is_tagged_cache_dir(dir: &Path) -> bool {
    let regular = |p: &Path| std::fs::symlink_metadata(p).is_ok_and(|m| m.is_file());
    if regular(&dir.join(CARGO_TARGET_MARKER)) {
        return true;
    }
    let tag = dir.join("CACHEDIR.TAG");
    if !regular(&tag) {
        return false;
    }
    let mut head = Vec::with_capacity(CACHEDIR_TAG_SIGNATURE.len());
    std::fs::File::open(&tag)
        .and_then(|f| {
            f.take(CACHEDIR_TAG_SIGNATURE.len() as u64)
                .read_to_end(&mut head)
        })
        .is_ok_and(|_| head == CACHEDIR_TAG_SIGNATURE)
}

/// The gitignored output under `path` that removal would destroy (#8534).
///
/// Why: see the module doc — `git worktree remove` deletes what `git status`
/// hides.
/// What: lists `!!` entries from [`IGNORED_STATUS_ARGS`], drops the ones the
/// rule excuses, and counts the files left, skipping harness files inside a
/// wholly ignored directory such as `.claude/`. `Ok(None)` means nothing is
/// kept. Any failure — git, a quoted path, an unreadable file — is `Err`.
/// Test: `kept_output_counts_files_in_an_ignored_results_dir`,
/// `kept_output_is_none_for_build_output_only`,
/// `kept_output_errors_when_git_cannot_answer`,
/// `kept_output_errors_on_a_quoted_path`.
pub(crate) fn kept_ignored_output(path: &Path) -> Result<Option<IgnoredOutput>, String> {
    let status = git_stdout(path, IGNORED_STATUS_ARGS)?;
    let mut kept: Option<IgnoredOutput> = None;
    for line in status.lines() {
        let Some(entry) = line.strip_prefix("!! ") else {
            continue;
        };
        let entry = entry.trim();
        if entry.starts_with('"') {
            return Err(format!(
                "`{entry}` has a quoted path this check cannot classify"
            ));
        }
        let bare = entry.trim_end_matches('/');
        if is_harness_path(bare) || is_regenerable(bare) || under_cache_dir(path, bare) {
            continue;
        }
        let files = count_files(path, bare)?;
        if files == 0 {
            continue; // nothing but harness files, or an empty directory
        }
        let found = kept.get_or_insert_with(|| IgnoredOutput {
            files: 0,
            first: entry.to_string(),
        });
        found.files += files;
    }
    Ok(kept)
}

/// Why `path` must be kept for its gitignored output, or `None` to proceed.
///
/// Why: a refusal wants one sentence naming the path and the count, and a
/// failed check must refuse, never permit.
/// What: `Some` for kept output and for every error; `None` only when the
/// check completed and found nothing the rule does not excuse.
/// Test: `reap_keeps_gitignored_run_output_in_a_zero_commit_tree`,
/// `refusal_keeps_the_tree_when_the_check_fails`.
pub(crate) fn ignored_output_refusal(path: &Path) -> Option<String> {
    match kept_ignored_output(path) {
        Ok(None) => None,
        Ok(Some(out)) => Some(format!(
            "{} holds {} gitignored file(s) that are not build output (first: `{}`); \
             `git worktree remove` would delete them (#8534)",
            path.display(),
            out.files,
            out.first
        )),
        Err(e) => Some(format!(
            "{}: the gitignored-output check failed ({e}); keeping it (#8534)",
            path.display()
        )),
    }
}

/// The shared remover's gate: why to keep `path`, honouring `policy` (#8534).
///
/// Why: decommission, the merged-PR reclaim and the orphan prune all end in one
/// remover. Only `prune-worktrees --discard-dirty` asks to lose unsaved work,
/// and gitignored output is unsaved work like any other.
/// What: [`ignored_output_refusal`], logged. Under
/// [`DirtyWorktreePolicy::ForceDiscard`] a refusal becomes a `warn!` naming
/// what is about to be destroyed, and `None`.
/// Test: `decommission_keeps_gitignored_run_output`,
/// `force_discard_prune_removes_gitignored_output`.
pub(crate) fn ignored_output_blocks_removal(
    path: &Path,
    policy: DirtyWorktreePolicy,
) -> Option<String> {
    let refusal = ignored_output_refusal(path)?;
    if policy == DirtyWorktreePolicy::Skip {
        warn!(path = %path.display(), "worktree removal refused — {refusal}");
        return Some(refusal);
    }
    warn!(
        path = %path.display(),
        "worktree removal: DISCARDING gitignored output — explicit force-discard opt-in \
         was supplied: {refusal}"
    );
    None
}

/// [`inspect_dirt`], with kept gitignored output counted as dirt (#8534).
///
/// Why: `tm pr cleanup` and its supervisor sweep run plain `git worktree
/// remove`, which deletes gitignored files as `--force` does. They take their
/// unsaved-work answer from an injected probe, so the probe carries the gate.
/// What: kept files add to `dirty_files`, so a squash-landed tree's
/// ahead-only reading no longer passes. A failed check answers both counts
/// zero, the "could not read" arm every caller refuses on.
/// Test: `probe_counts_ignored_output_as_dirt`,
/// `probe_fails_toward_dirty_when_the_ignored_check_fails`.
pub fn inspect_dirt_with_ignored_output(path: &Path) -> Option<DirtyWorktree> {
    let dirt = inspect_dirt(path);
    match (dirt, kept_ignored_output(path)) {
        (dirt, Ok(None)) => dirt,
        (dirt, Ok(Some(out))) => {
            let why = format!(
                "{} gitignored file(s) that are not build output (first: `{}`)",
                out.files, out.first
            );
            Some(match dirt {
                Some(mut d) => {
                    d.dirty_files += out.files;
                    d.reason = format!("{}; {why}", d.reason);
                    d
                }
                None => DirtyWorktree::new(path, why, out.files, 0),
            })
        }
        (dirt, Err(e)) => {
            let why = format!("the gitignored-output check failed: {e}");
            let reason = match dirt {
                Some(d) => format!("{}; {why}", d.reason),
                None => why,
            };
            Some(DirtyWorktree::new(path, reason, 0, 0))
        }
    }
}

/// Count the files at `root/rel`: 1 for a file or symlink, the regular files
/// and symlinks beneath it for a directory, skipping harness paths (symlinks
/// are not followed).
fn count_files(root: &Path, rel: &str) -> Result<usize, String> {
    let top = root.join(rel);
    let meta = std::fs::symlink_metadata(&top)
        .map_err(|e| format!("`{}` is unreadable: {e}", top.display()))?;
    if !meta.is_dir() {
        return Ok(1);
    }
    let mut count = 0usize;
    let mut stack = vec![(top, rel.to_string())];
    while let Some((dir, dir_rel)) = stack.pop() {
        let entries = std::fs::read_dir(&dir)
            .map_err(|e| format!("`{}` is unreadable: {e}", dir.display()))?;
        for entry in entries {
            let entry = entry.map_err(|e| format!("`{}` is unreadable: {e}", dir.display()))?;
            let kind = entry
                .file_type()
                .map_err(|e| format!("`{}` is unreadable: {e}", entry.path().display()))?;
            let name = entry.file_name();
            let child_rel = format!("{dir_rel}/{}", name.to_string_lossy());
            if is_harness_path(&child_rel) {
                continue;
            }
            if kind.is_dir() {
                stack.push((entry.path(), child_rel));
            } else {
                count += 1;
                if count >= FILE_COUNT_CAP {
                    return Ok(count);
                }
            }
        }
    }
    Ok(count)
}

#[cfg(test)]
#[path = "worktree_ignored_output_tests.rs"]
mod worktree_ignored_output_tests;
