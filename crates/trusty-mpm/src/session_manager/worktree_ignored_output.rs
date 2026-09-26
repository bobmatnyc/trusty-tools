//! Gitignored output a worktree removal would destroy (#8534).
//!
//! Why: the `git worktree remove` routes gate on
//! [`super::worktree_safety::inspect_dirt`], which counts modified tracked files
//! and untracked files that are NOT gitignored, and then run `git worktree
//! remove` — which deletes every gitignored file, with or without `--force`. An
//! agent with zero commits whose run wrote its results into a gitignored
//! directory therefore read as clean, and the results went with the tree.
//!
//! What — THE RULE: a gitignored entry does not block removal when
//! - it is harness bookkeeping tm or Claude Code writes into every tree
//!   ([`is_harness_path`]: `.trusty-mpm/`, the ownership marker,
//!   `.claude/settings*.json` and their sidecars, the deploy ledgers, and the
//!   scaffold paths outside `.claude/agents/` and `.claude/skills/`);
//! - it is an agent or skill file tm deployed and nobody edited since
//!   ([`super::worktree_deployed_assets`]);
//! - the directory git matched is build or tool output: its last component is
//!   a disposable name ([`super::worktree_nested::is_disposable_dir_name`]) or
//!   its trailing path is in [`DISPOSABLE_DIR_PATHS`], or its basename is OS,
//!   bytecode or tool-cache litter ([`is_regenerable`]); or
//! - it is, or sits inside, a directory a cache tool tagged as its own
//!   ([`under_cache_dir`]: a `CACHEDIR.TAG` carrying the spec's signature).
//!
//! The last two excuses never apply under `.claude/agents/` or
//! `.claude/skills/`, where only the ledger decides.
//!
//! Every other gitignored entry is kept output and blocks removal. Only the
//! entry's LAST component is matched by name, so `build/out.json` under a
//! tracked `build/` and a user's `target-analysis/` are kept.
//!
//! FAIL-SAFE: a git error or an unreadable directory is an `Err`, and every
//! caller keeps the tree on `Err`. An unreadable deploy ledger counts every
//! file it would have excused.
//! Where it is enforced: the agent reap (gate 5a), the `git worktree remove`
//! step of the shared remover `decommission::remove_registered_worktree`
//! (decommission, merged-PR reclaim, orphan prune), and the `tm pr cleanup`
//! probe [`inspect_dirt_with_ignored_output`]. The two `remove_dir_all` routes
//! — decommission of an SM-owned workspace and
//! `decommission::remove_unclaimed_directory` — apply the same rule to a
//! directory git holds no state for through [`kept_unversioned_content`]
//! (#8663).
//! Test: `worktree_ignored_output_tests`.

use std::io::Read;
use std::path::Path;

use tracing::warn;

use super::worktree_deployed_assets::{DeployedAssets, is_asset_path};
use super::worktree_nested::is_disposable_dir_name;
use super::worktree_safety::{DirtyWorktree, DirtyWorktreePolicy, git_stdout, inspect_dirt};
use crate::core::scaffold_gitignore::SCAFFOLD_IGNORED_PATHS;

/// Build-output directories named by more than one trailing component.
///
/// Why: `gen/` alone is too common a user directory name to excuse, but Tauri
/// regenerates `src-tauri/gen/` on every build.
const DISPOSABLE_DIR_PATHS: &[&str] = &["src-tauri/gen"];

/// File basenames that are OS litter or a tool's own cache, never run output.
const REGENERABLE_FILES: &[&str] = &[".DS_Store", "Thumbs.db", ".eslintcache", ".coverage"];

/// File-name suffixes of bytecode and incremental-build caches.
const REGENERABLE_SUFFIXES: &[&str] = &[".pyc", ".tsbuildinfo"];

/// Harness paths beyond [`SCAFFOLD_IGNORED_PATHS`] that tm writes into a tree.
/// A trailing `*` matches the rest of that path segment.
const HARNESS_PATHS: &[&str] = &[
    ".trusty-mpm/",
    ".claude/settings.json*",
    ".claude/settings.local.json*",
    // #8534 critic round 3: the deploy ledgers with their lock and temp
    // sidecars, and the project-tier stamp.
    ".claude/skills/.trusty-mpm-skills-manifest.json*",
    ".claude/skills/.trusty-mpm-project-tier-stamp",
    ".claude/agents/.trusty-mpm-manifest.json*",
];

/// The Cache Directory Tagging signature (<https://bford.info/cachedir/>).
const CACHEDIR_TAG_SIGNATURE: &[u8] = b"Signature: 8a477f597d28d172789f06886806bc55";

/// `git status` listing gitignored entries, collapsed to the matching directory.
/// #8534 critic round 3: `-z`. Even under the pinned `core.quotePath=false`,
/// git quotes a name holding `"`, `\` or a control character, and a quoted
/// name kept the tree forever.
const IGNORED_STATUS_ARGS: &[&str] = &[
    "status",
    "--porcelain",
    "-z",
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
    /// #8663: every kept entry with its file count, in listing order.
    pub entries: Vec<(String, usize)>,
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
    is_disposable_dir_name(basename)
        || DISPOSABLE_DIR_PATHS
            .iter()
            .any(|p| entry == *p || entry.ends_with(&format!("/{p}")))
        || REGENERABLE_FILES.contains(&basename)
        || REGENERABLE_SUFFIXES.iter().any(|s| basename.ends_with(s))
}

/// Is this repo-relative path harness bookkeeping, not run output?
///
/// Why: a managed tree always holds tm's gitignored harness files; without
/// this excuse every decommission of one would refuse (#8534 critic round 2).
/// #8534 critic round 3: the scaffold's `.claude/agents/` and `.claude/skills/*`
/// are left out; a user's agent or skill lives there too, so a ledger decides.
/// Test: `harness_paths_are_excused_and_look_alikes_are_not`.
pub(crate) fn is_harness_path(rel: &str) -> bool {
    let rel = rel.trim_end_matches('/');
    rel == super::decommission::WORKTREE_SENTINEL_FILE
        || HARNESS_PATHS
            .iter()
            .chain(SCAFFOLD_IGNORED_PATHS.iter().filter(|p| !is_asset_path(p)))
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
/// can hold. The tag is written by the tool, never by a user. Cargo tags only a
/// target directory it creates itself, so a pre-created one is kept.
/// Test: `tagged_cache_dirs_are_excused_by_content_not_name`,
/// `a_rustc_info_file_alone_does_not_excuse_a_directory`.
fn under_cache_dir(root: &Path, rel: &str) -> bool {
    let mut prefix = root.to_path_buf();
    rel.split('/').filter(|p| !p.is_empty()).any(|part| {
        prefix.push(part);
        is_tagged_cache_dir(&prefix)
    })
}

/// Does `dir` hold a `CACHEDIR.TAG` that opens with the spec's signature?
/// Anything unreadable answers `false`, which keeps the entry.
/// #8534 critic round 3: cargo's `.rustc_info.json` no longer counts; any
/// directory can hold a file of that name.
fn is_tagged_cache_dir(dir: &Path) -> bool {
    let tag = dir.join("CACHEDIR.TAG");
    if !std::fs::symlink_metadata(&tag).is_ok_and(|m| m.is_file()) {
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
/// rule excuses, and counts the files left, skipping harness files and tm's
/// deployed agents and skills inside a directory such as `.claude/`.
/// `Ok(None)` means nothing is kept. A git failure or an unreadable file is
/// `Err`.
/// Test: `kept_output_counts_files_in_an_ignored_results_dir`,
/// `kept_output_is_none_for_build_output_only`,
/// `kept_output_errors_when_git_cannot_answer`,
/// `non_ascii_names_are_classified_not_refused`,
/// `a_user_skill_with_a_disposable_name_keeps_the_tree`,
/// `a_user_agent_with_a_disposable_name_keeps_the_tree`,
/// `a_vercel_dir_holding_an_env_file_keeps_the_tree`.
pub(crate) fn kept_ignored_output(path: &Path) -> Result<Option<IgnoredOutput>, String> {
    let status = git_stdout(path, IGNORED_STATUS_ARGS)?;
    kept_among(path, ignored_entries(&status))
}

/// The content of a directory git holds no state for that deleting it would
/// lose (#8663).
///
/// Why: two routes delete a directory with `remove_dir_all` rather than `git
/// worktree remove` — an SM-owned workspace that is not a repository, and a
/// leftover git has disowned — so there is no `git status` to ask. The same
/// rule has to decide, or an agent that deleted `.git` loses its results.
/// What: every top-level entry of `path` goes through the rule
/// [`kept_ignored_output`] applies to a `!!` entry. `Ok(None)` means `path`
/// holds only harness files, tm's unedited deployed assets and regenerable
/// output. An unreadable directory or entry is `Err`.
/// Test: `unversioned_content_keeps_run_output_and_excuses_harness_files`,
/// `unclaimed_directory_with_results_is_kept`.
pub(crate) fn kept_unversioned_content(path: &Path) -> Result<Option<IgnoredOutput>, String> {
    let unreadable = |e: std::io::Error| format!("`{}` is unreadable: {e}", path.display());
    let mut names = Vec::new();
    for entry in std::fs::read_dir(path).map_err(unreadable)? {
        names.push(entry.map_err(unreadable)?.file_name());
    }
    // A non-UTF-8 name maps to a path that does not exist, so `count_files`
    // fails on it and the directory is kept.
    let names: Vec<String> = names
        .iter()
        .map(|n| n.to_string_lossy().into_owned())
        .collect();
    kept_among(path, names.iter().map(String::as_str))
}

/// The rule of the module doc over `entries`, repo-relative to `path`.
fn kept_among<'e>(
    path: &Path,
    entries: impl IntoIterator<Item = &'e str>,
) -> Result<Option<IgnoredOutput>, String> {
    let mut assets = DeployedAssets::new(path);
    let mut kept: Option<IgnoredOutput> = None;
    for entry in entries {
        let bare = entry.trim_end_matches('/');
        // #8534 final round: under the deploy directories only a ledger
        // decides; a user skill named `build/` or `dist/` is not excused.
        if is_harness_path(bare)
            || (!is_asset_path(bare) && (is_regenerable(bare) || under_cache_dir(path, bare)))
        {
            continue;
        }
        let files = count_files(path, bare, &mut assets)?;
        if files == 0 {
            continue; // nothing but harness files, or an empty directory
        }
        let found = kept.get_or_insert_with(|| IgnoredOutput {
            files: 0,
            first: entry.to_string(),
            entries: Vec::new(),
        });
        found.files += files;
        found.entries.push((entry.to_string(), files));
    }
    Ok(kept)
}

/// The `!!` entries of a `git status --porcelain -z` listing.
///
/// Why: `-z` records end in NUL and are never quoted. A rename or copy record
/// carries its source path as the NEXT record, which is not an entry.
/// Test: `ignored_entries_skips_a_rename_source`.
fn ignored_entries(status: &str) -> Vec<&str> {
    let mut entries = Vec::new();
    let mut records = status.split('\0');
    while let Some(record) = records.next() {
        let xy = record.as_bytes().get(..2).unwrap_or_default();
        if xy.iter().any(|c| matches!(c, b'R' | b'C')) {
            records.next();
        } else if let Some(entry) = record.strip_prefix("!! ") {
            entries.push(entry);
        }
    }
    entries
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

/// Why `path`, a directory git holds no state for, must be kept, or `None`
/// to proceed (#8663).
///
/// What: [`kept_unversioned_content`] as one operator-facing sentence naming
/// the path. `Some` for kept content and for every error.
/// Test: `unclaimed_directory_with_results_is_kept`,
/// `unclaimed_directory_is_kept_when_the_content_check_fails`.
pub(crate) fn unversioned_content_refusal(path: &Path) -> Option<String> {
    match kept_unversioned_content(path) {
        Ok(None) => None,
        Ok(Some(out)) => Some(format!(
            "{} is not tracked by git and holds {} file(s) that are not harness files or \
             build output (first: `{}`); deleting it would lose them (#8663)",
            path.display(),
            out.files,
            out.first
        )),
        Err(e) => Some(format!(
            "{}: the content check failed ({e}); keeping it (#8663)",
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
/// and symlinks beneath it for a directory, skipping harness paths and tm's
/// deployed agents and skills (symlinks are not followed).
fn count_files(root: &Path, rel: &str, assets: &mut DeployedAssets) -> Result<usize, String> {
    let top = root.join(rel);
    let meta = std::fs::symlink_metadata(&top)
        .map_err(|e| format!("`{}` is unreadable: {e}", top.display()))?;
    if !meta.is_dir() {
        return Ok(usize::from(!assets.is_tm_deployed(rel)));
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
            } else if !assets.is_tm_deployed(&child_rel) {
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
