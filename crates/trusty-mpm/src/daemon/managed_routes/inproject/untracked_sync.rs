//! Allowlisted untracked/secret file sync into session worktrees (#2196).
//!
//! Why: an operator's live checkout often carries files git never tracks —
//! most commonly `.env`/`.env.local` — that a session worktree still needs
//! (env vars for local tooling, API keys for tests, etc.). Managed sessions
//! intentionally never copy the operator's uncommitted work into a worktree
//! (see `tm launch`'s "uncommitted local changes are not carried into the
//! managed clone" warning), so without an explicit, narrow allowlist those
//! files are simply absent and every session has to be re-configured by hand.
//! This module is that narrow allowlist: it copies ONLY files matching
//! operator-declared patterns (default `.env*`, see
//! [`crate::core::trusty_tools_config::DEFAULT_UNTRACKED_SYNC_PATTERNS`]) —
//! never an unrestricted "copy all untracked files" — and never fails the
//! spawn it runs inside.
//! What: [`sync_untracked_files`] is the single entry point: for each
//! pattern, matches files at the SOURCE checkout root (or one explicit
//! subdirectory when a pattern contains `/`), copies matches into the
//! destination worktree at the same relative path, and appends each copied
//! path to the worktree's real `info/exclude` file (resolved via `git
//! rev-parse --git-path`, which correctly follows a linked worktree's gitlink
//! back to the shared common git dir) so the secret is gitignore-safe
//! regardless of the repo's own tracked `.gitignore`. Every step is
//! best-effort: a missing source root, an unreadable file, an oversized file,
//! or a failed exclude-append is `warn!`-logged and skipped — this function
//! NEVER returns an error and NEVER panics, matching the "sync failure must
//! not fail the spawn" contract from [`try_inproject_spawn`]'s call site.
//! Test: `tests` submodule — pattern matching, the size cap, the path-escape
//! guard, the `node_modules/`-is-a-directory-so-never-matches case, and the
//! `.git/info/exclude` append for a real linked worktree.

use std::fs;
use std::path::{Path, PathBuf};

use tracing::{info, warn};

/// Per-file size ceiling for untracked-sync copies.
///
/// Why: the allowlist is meant for small config/secret files; without a cap
/// a mis-declared pattern (or a `.env.*` matching an unexpectedly large file)
/// could copy something enormous into every session worktree.
/// What: 5 MiB. Files larger than this are skipped with a `warn!`.
/// Test: `tests::oversized_file_is_skipped`.
pub const MAX_SYNC_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// Sync allowlisted untracked/secret files from `source` into `dest_worktree`.
///
/// Why: the single seam that turns a resolved pattern allowlist into actual
/// copied files, called once per in-project spawn right after the session
/// worktree is created (`lifecycle::reserve_inproject_worktree`).
/// What: for each pattern in `patterns`, resolves candidate files via
/// [`collect_matches`] (top-level of `source`, or one explicit subdirectory
/// when the pattern contains `/`), skips anything over
/// [`MAX_SYNC_FILE_BYTES`], copies the rest to
/// `<dest_worktree>/<relative_path>` (creating parent dirs as needed), and
/// appends every successfully-copied relative path to the worktree's real
/// `info/exclude` file via [`append_to_git_exclude`]. Duplicate matches
/// across overlapping patterns are copied once. Never panics, never returns
/// an error — every failure is a `warn!` and the loop continues.
/// Test: `tests::syncs_matching_files_only`, `tests::disabled_copies_nothing`
/// (caller-side gate — this function itself has no `enabled` concept, the
/// caller skips calling it entirely when disabled),
/// `tests::missing_source_file_is_non_fatal`.
pub fn sync_untracked_files(source: &Path, dest_worktree: &Path, patterns: &[String]) {
    if patterns.is_empty() {
        return;
    }

    // Dedup by relative path so overlapping patterns (e.g. ".env" and
    // ".env.*" both matching some contrived name) only copy once.
    let mut candidates: Vec<(PathBuf, String)> = Vec::new();
    let mut seen = std::collections::HashSet::new();
    for pattern in patterns {
        for (abs, rel) in collect_matches(source, pattern) {
            if seen.insert(rel.clone()) {
                candidates.push((abs, rel));
            }
        }
    }

    let mut copied: Vec<String> = Vec::new();
    for (abs_path, rel_path) in candidates {
        let size = match fs::metadata(&abs_path) {
            Ok(m) => m.len(),
            Err(e) => {
                warn!(
                    file = %abs_path.display(),
                    "untracked_sync: could not stat candidate file (non-fatal, skipping): {e}"
                );
                continue;
            }
        };
        if size > MAX_SYNC_FILE_BYTES {
            warn!(
                file = %abs_path.display(),
                size,
                cap = MAX_SYNC_FILE_BYTES,
                "untracked_sync: file exceeds size cap, skipping"
            );
            continue;
        }

        let dest_path = dest_worktree.join(&rel_path);
        if let Some(parent) = dest_path.parent()
            && let Err(e) = fs::create_dir_all(parent)
        {
            warn!(
                dir = %parent.display(),
                "untracked_sync: could not create parent dir (non-fatal, skipping file): {e}"
            );
            continue;
        }

        match fs::copy(&abs_path, &dest_path) {
            Ok(_) => {
                info!(
                    from = %abs_path.display(),
                    to = %dest_path.display(),
                    "untracked_sync: copied allowlisted untracked file into session worktree"
                );
                copied.push(rel_path);
            }
            Err(e) => {
                warn!(
                    from = %abs_path.display(),
                    to = %dest_path.display(),
                    "untracked_sync: copy failed (non-fatal, skipping): {e}"
                );
            }
        }
    }

    if !copied.is_empty()
        && let Err(e) = append_to_git_exclude(dest_worktree, &copied)
    {
        warn!(
            worktree = %dest_worktree.display(),
            "untracked_sync: failed to append copied files to git exclude (non-fatal, \
             copied files may show as untracked in `git status`): {e}"
        );
    }
}

/// Resolve the candidate `(absolute_path, relative_path)` matches for one
/// allowlist pattern.
///
/// Why: a pattern with no `/` matches top-level files only (the `.env*`
/// case); a pattern containing `/` names an explicit subdirectory, which is
/// scanned non-recursively at that one level. Splitting the two cases here
/// keeps [`sync_untracked_files`] a flat copy loop.
/// What: returns `(source-relative absolute path, relative path string)`
/// pairs for every regular file directly inside the resolved directory whose
/// filename matches the leaf pattern via [`glob_match`]. Returns an empty
/// vec (never an error) when the resolved directory is unreadable/absent, or
/// when the pattern's directory component escapes `source` (see
/// [`is_unsafe_dir_component`]).
/// Test: `tests::top_level_pattern_matches_only_root_files`,
/// `tests::subdirectory_pattern_matches_one_level`,
/// `tests::path_escape_pattern_matches_nothing`.
fn collect_matches(source: &Path, pattern: &str) -> Vec<(PathBuf, String)> {
    let (dir, dir_prefix, leaf) = match pattern.rsplit_once('/') {
        Some((dir_part, leaf)) => {
            if is_unsafe_dir_component(dir_part) {
                warn!(
                    pattern,
                    "untracked_sync: rejecting pattern with an absolute or `..`-escaping \
                     directory component"
                );
                return Vec::new();
            }
            (source.join(dir_part), format!("{dir_part}/"), leaf)
        }
        None => (source.to_path_buf(), String::new(), pattern),
    };

    let read_dir = match fs::read_dir(&dir) {
        Ok(rd) => rd,
        Err(e) => {
            // Absent/unreadable source directory is expected (e.g. no `.env`
            // files at all) and must never be treated as a hard failure.
            warn!(
                dir = %dir.display(),
                "untracked_sync: could not read directory for pattern {pattern:?} (non-fatal): {e}"
            );
            return Vec::new();
        }
    };

    read_dir
        .flatten()
        .filter_map(|entry| {
            let path = entry.path();
            if !path.is_file() {
                return None;
            }
            let name = path.file_name()?.to_str()?;
            if glob_match(leaf, name) {
                Some((path.clone(), format!("{dir_prefix}{name}")))
            } else {
                None
            }
        })
        .collect()
}

/// Whether a pattern's directory component would escape `source`.
///
/// Why: [`collect_matches`] must refuse a pattern like `"../secrets/.env"` or
/// `"/etc/.env"` BEFORE ever joining it onto `source` and reading a
/// directory outside the checkout — the allowlist-only / no-escape security
/// guard (#2196).
/// What: `true` when `dir_part` is absolute (starts with `/`) or any `/`-
/// separated segment is `".."` or empty (`"a//b"`, a leading/trailing `/`).
/// Test: `tests::path_escape_pattern_matches_nothing`.
fn is_unsafe_dir_component(dir_part: &str) -> bool {
    dir_part.starts_with('/') || dir_part.split('/').any(|seg| seg == ".." || seg.is_empty())
}

/// Match `name` against a simple glob `pattern` (literal characters plus `*`
/// wildcards only — no `?`, no `**`, no character classes).
///
/// Why: the allowlist's real-world patterns are exact names (`.env`), simple
/// prefixes (`.env.*`), or simple suffixes (`*.env`); a hand-rolled matcher
/// avoids pulling in a glob crate for a handful of trivial shapes while still
/// handling an arbitrary number/position of `*` correctly.
/// What: `true` iff `pattern`, with each `*` treated as "zero or more
/// characters", matches `name` in full. Case-sensitive (filesystem names on
/// the platforms this runs on are case-sensitive or case-preserving).
/// Test: `tests::glob_match_exact`, `tests::glob_match_prefix`,
/// `tests::glob_match_suffix`, `tests::glob_match_no_match`.
fn glob_match(pattern: &str, name: &str) -> bool {
    let p: Vec<char> = pattern.chars().collect();
    let n: Vec<char> = name.chars().collect();
    glob_match_from(&p, &n)
}

/// Recursive worker for [`glob_match`].
///
/// Why: isolated so the wildcard branch's backtracking search reads as a
/// one-liner over the two slices.
/// What: standard `*`-only glob matching: an empty pattern matches only an
/// empty remainder; a literal char must match the next char exactly; a `*`
/// tries consuming `0..=n.len()` characters before matching the rest of the
/// pattern against the rest of the name.
/// Test: covered transitively via [`glob_match`]'s tests.
fn glob_match_from(p: &[char], n: &[char]) -> bool {
    match p.split_first() {
        None => n.is_empty(),
        Some((&'*', rest)) => (0..=n.len()).any(|i| glob_match_from(rest, &n[i..])),
        Some((c, rest)) => match n.split_first() {
            Some((nc, nrest)) if nc == c => glob_match_from(rest, nrest),
            _ => false,
        },
    }
}

/// Append copied relative paths to the worktree's real `info/exclude` file.
///
/// Why: a linked git worktree's `.git` is a FILE (a gitlink to
/// `<common-git-dir>/worktrees/<name>`), not a directory — `info/exclude` is
/// a repo-wide (not per-worktree) file that lives in the SHARED common git
/// dir. Naively joining `<worktree>/.git/info/exclude` would try to create a
/// directory inside that gitlink file and fail. `git rev-parse --git-path
/// info/exclude`, run with the worktree as cwd, is git's own resolver for
/// this — it returns the correct shared path for both a plain repo and a
/// linked worktree, mirroring how [`super::ensure_worktrees_gitignored`]
/// appends to the analogous file for the base clone itself.
/// What: resolves the real exclude-file path via `git -C <dest_worktree>
/// rev-parse --git-path info/exclude`, creates its parent dir, and appends
/// each of `relative_paths` that is not already present (idempotent, mirrors
/// [`super::ensure_worktrees_gitignored`]'s read-then-append style).
/// Test: `tests::append_to_git_exclude_resolves_shared_worktree_path`,
/// `tests::append_to_git_exclude_is_idempotent`.
fn append_to_git_exclude(dest_worktree: &Path, relative_paths: &[String]) -> Result<(), String> {
    let out = std::process::Command::new("git")
        .arg("-C")
        .arg(dest_worktree)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
        .map_err(|e| format!("untracked_sync: git rev-parse failed to spawn: {e}"))?;

    if !out.status.success() {
        let stderr = String::from_utf8_lossy(&out.stderr);
        return Err(format!(
            "untracked_sync: git rev-parse --git-path info/exclude failed ({}): {stderr}",
            out.status
        ));
    }

    let raw_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw_path.is_empty() {
        return Err("untracked_sync: git rev-parse returned an empty path".to_string());
    }
    // `--git-path` may return a path relative to the worktree cwd (plain-repo
    // case) or an absolute path (linked-worktree case, pointing at the
    // shared common dir) — resolve relative results against dest_worktree.
    let exclude_path = {
        let p = PathBuf::from(&raw_path);
        if p.is_absolute() {
            p
        } else {
            dest_worktree.join(p)
        }
    };

    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("untracked_sync: could not create {}: {e}", parent.display()))?;
    }

    let existing = fs::read_to_string(&exclude_path).unwrap_or_default();
    let mut to_append = String::new();
    for rel in relative_paths {
        if !existing.lines().any(|line| line.trim() == rel.as_str()) {
            to_append.push_str(rel);
            to_append.push('\n');
        }
    }
    if to_append.is_empty() {
        return Ok(());
    }

    let entry = if existing.is_empty() || existing.ends_with('\n') {
        to_append
    } else {
        format!("\n{to_append}")
    };

    use std::io::Write as _;
    let mut file = fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(&exclude_path)
        .map_err(|e| {
            format!(
                "untracked_sync: could not open {}: {e}",
                exclude_path.display()
            )
        })?;
    file.write_all(entry.as_bytes()).map_err(|e| {
        format!(
            "untracked_sync: could not write {}: {e}",
            exclude_path.display()
        )
    })?;

    Ok(())
}

#[cfg(test)]
mod tests;
