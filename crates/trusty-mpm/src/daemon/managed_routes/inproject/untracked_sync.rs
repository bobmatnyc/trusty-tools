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
//! best-effort: a missing source root, an unreadable file, or an oversized file
//! is `warn!`-logged and skipped — this function NEVER returns an error and
//! NEVER panics, matching the "sync failure must not fail the spawn" contract
//! from [`try_inproject_spawn`]'s call site.
//!
//! 🔴 Best-effort applies to COPYING, never to the exclusion guarantee (#4733).
//! Until #4733 the exclude-append ran AFTER the copy and a failure was only a
//! `warn!`, so a `git rev-parse` that merely declined to answer — a stale
//! worktree gitlink, `detected dubious ownership`, an unreadable `.git` — left
//! the operator's `.env` sitting unregistered in a live worktree where a later
//! `git add -A && git commit` stages it into history. The order is now inverted:
//! register first, ask `git check-ignore` (git's own authority, mirroring
//! [`crate::core::session_launch::native_mcp::is_env_local_actually_ignored`])
//! whether each path is genuinely ignored, and copy only what it confirms. A
//! destination with no repository at all has no history to leak into and is
//! copied to freely; anything in between refuses.
//! Test: `tests` submodule — pattern matching, the size cap, the path-escape
//! guard, the `node_modules/`-is-a-directory-so-never-matches case, the
//! `.git/info/exclude` append for a real linked worktree, and the #4733
//! broken-repo / verification-failure refusals.

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
///
/// What: resolves candidate files via [`collect_matches`] for every pattern
/// (top-level of `source`, or one explicit subdirectory when the pattern
/// contains `/`), drops anything over [`MAX_SYNC_FILE_BYTES`], and then — before
/// writing a single byte into the worktree — settles the exclusion guarantee:
///
/// 1. [`resolve_git_exclude`] classifies the destination in three states.
///    [`ExcludeTarget::Unknown`] (git failed, or the destination is a repository
///    git could not read) copies NOTHING; [`ExcludeTarget::NoRepo`] has no
///    history to leak into, so it copies with no registration.
/// 2. For a real repository, every candidate path is appended to the worktree's
///    real `info/exclude` via [`append_paths_to_exclude`] FIRST — registration
///    is what MAKES a path ignored.
/// 3. Each path is then re-verified with [`is_path_git_ignored`] — `git
///    check-ignore`, git's own authority, which is what PROVES it, and which
///    also answers "not ignored" for a path the repo already TRACKS. Only
///    confirmed paths are copied, so a failure in step 2 costs at most the
///    paths the repo's own ignore rules did not already cover.
///
/// Duplicate matches across overlapping patterns are copied once. Never panics,
/// never returns an error — every failure is a `warn!` (#4733: refusing to copy
/// is the failure mode, never copying unregistered).
/// Test: `tests::syncs_matching_files_only`, `tests::disabled_copies_nothing`
/// (caller-side gate — this function itself has no `enabled` concept, the
/// caller skips calling it entirely when disabled),
/// `tests::missing_source_file_is_non_fatal`,
/// `tests::broken_repo_copies_nothing`,
/// `tests::tracked_secret_is_not_overwritten_in_worktree`.
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
    candidates.retain(|(abs_path, _)| within_size_cap(abs_path));
    if candidates.is_empty() {
        return;
    }

    // #4733: settle the exclusion guarantee BEFORE any secret is written.
    let in_repo = match resolve_git_exclude(dest_worktree) {
        ExcludeTarget::NoRepo => false,
        ExcludeTarget::Unknown(reason) => {
            warn!(
                worktree = %dest_worktree.display(),
                "untracked_sync: refusing to copy {} allowlisted file(s) — a git \
                 repository could not be ruled out and its exclude file could not be \
                 resolved ({reason}); copying now would leave secrets stageable by a \
                 later `git add -A` (issue #4733)",
                candidates.len(),
            );
            return;
        }
        ExcludeTarget::Registered(exclude_path) => {
            let rels: Vec<String> = candidates.iter().map(|(_, rel)| rel.clone()).collect();
            if let Err(e) = append_paths_to_exclude(&exclude_path, &rels) {
                // Not fatal on its own: registration is what MAKES a path
                // ignored, but `is_path_git_ignored` below is what PROVES it,
                // and a repo whose own `.gitignore` already covers the path
                // needs no entry here. Anything this failure leaves unignored
                // is refused per-file (#4733).
                warn!(
                    worktree = %dest_worktree.display(),
                    "untracked_sync: could not register {} allowlisted file(s) in the \
                     worktree's git exclude file ({e}) — each will be copied only if \
                     `git check-ignore` confirms it is already ignored (issue #4733)",
                    rels.len(),
                );
            }
            true
        }
    };

    for (abs_path, rel_path) in candidates {
        // #4733: git itself confirms the registration took effect. A `.gitignore`
        // negation elsewhere, or a path the repo already TRACKS, both report
        // "not ignored" here — and both would be staged by `git add -A`.
        if in_repo && !is_path_git_ignored(dest_worktree, &rel_path) {
            warn!(
                worktree = %dest_worktree.display(),
                file = %rel_path,
                "untracked_sync: refusing to copy — `git check-ignore` does not \
                 recognise this path as ignored in the session worktree (it may \
                 already be tracked, or an ignore-negation overrides the exclude \
                 entry); this session launches without it (issue #4733)"
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
            Ok(_) => info!(
                from = %abs_path.display(),
                to = %dest_path.display(),
                "untracked_sync: copied allowlisted untracked file into session worktree"
            ),
            Err(e) => warn!(
                from = %abs_path.display(),
                to = %dest_path.display(),
                "untracked_sync: copy failed (non-fatal, skipping): {e}"
            ),
        }
    }
}

/// Whether `path` is a statable file at or under [`MAX_SYNC_FILE_BYTES`].
///
/// Why: the size cap has to run before the exclude registration (#4733) so a
/// file that will never be copied does not earn an exclude entry.
/// What: `false` for an unstatable path or one over the cap, each `warn!`-logged.
/// Test: `tests::oversized_file_is_skipped`.
fn within_size_cap(path: &Path) -> bool {
    match fs::metadata(path) {
        Ok(m) if m.len() <= MAX_SYNC_FILE_BYTES => true,
        Ok(m) => {
            warn!(
                file = %path.display(),
                size = m.len(),
                cap = MAX_SYNC_FILE_BYTES,
                "untracked_sync: file exceeds size cap, skipping"
            );
            false
        }
        Err(e) => {
            warn!(
                file = %path.display(),
                "untracked_sync: could not stat candidate file (non-fatal, skipping): {e}"
            );
            false
        }
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

/// What the destination worktree's git exclude file resolved to.
///
/// Why: #4733 — "there is no repository here" and "git could not be asked" are
/// different facts with opposite safe answers. A destination with no repository
/// has no history for a secret to leak into, so copying is fine; a destination
/// that IS (or may be) a repository whose exclude file could not be resolved
/// must be left alone entirely.
/// What: [`Registered`](Self::Registered) carries the real `info/exclude` path
/// (shared common dir for a linked worktree); [`NoRepo`](Self::NoRepo) is a
/// corroborated absence; [`Unknown`](Self::Unknown) carries the reason no
/// conclusion was available.
/// Test: `tests::broken_repo_copies_nothing`,
/// `tests::plain_directory_destination_still_copies`.
enum ExcludeTarget {
    /// A repository was found; this is its real `info/exclude` path.
    Registered(PathBuf),
    /// Corroborated: no git repository at or above the destination.
    NoRepo,
    /// git could not be consulted. Carries the reason for the operator log.
    Unknown(String),
}

/// The only git stderr CONSISTENT with "there is genuinely no repository here".
///
/// Why: the parenthesised clause is load-bearing. Git emits
/// `fatal: not a git repository: (null)` for a STALE WORKTREE POINTER, so the
/// shorter phrase `not a git repository` matches a broken repo and a
/// genuinely-absent one alike — and the broken repo is the one with real history
/// a secret can be committed into.
///
/// Consistent with, NOT proof of: git emits the same text for an unreadable
/// `.git`, where the repository is real. [`classify_rev_parse_failure`]
/// corroborates it with a filesystem witness first.
///
/// What: verified byte-identical against git 2.54.0. Any wording drift falls
/// through to [`ExcludeTarget::Unknown`] — the fail-closed direction. Mirrors
/// `trusty-agents-common`'s `vcs_claim::NO_REPO_STDERR` (#4448/#4727); #4735
/// extracts the shared probe both will call.
/// Test: `tests::broken_repo_copies_nothing`.
const NO_REPO_STDERR: &str = "not a git repository (or any of the parent directories)";

/// Resolve the destination's real `info/exclude` path, in three states (#4733).
///
/// Why: this is the gate the whole exclusion guarantee turns on, so it must
/// answer WHY a `rev-parse` failed rather than collapsing every failure into
/// "just warn and carry on copying".
///
/// It asks git rather than joining `<dest_worktree>/.git/info/exclude` because a
/// linked worktree's `.git` is a FILE (a gitlink to
/// `<common-git-dir>/worktrees/<name>`) while `info/exclude` is repo-wide and
/// lives in the SHARED common git dir — the naive join would try to create a
/// directory inside that gitlink file and fail. This mirrors how
/// [`super::ensure_worktrees_gitignored`] reaches the analogous file for the
/// base clone.
///
/// 🔴 THE EXIT CODE ALONE IS NOT A CLASSIFIER. Git exits 128 for "not a
/// repository" AND for `detected dubious ownership`, a stale worktree gitlink,
/// a broken `repositoryformatversion`, or any failing `git` shim on `PATH`.
///
/// What: runs `git -C <dest_worktree> rev-parse --git-path info/exclude`. A
/// spawn failure or an empty stdout is [`ExcludeTarget::Unknown`]; a non-zero
/// exit is delegated to [`classify_rev_parse_failure`]; success resolves a
/// relative result (plain repo) against `dest_worktree` and uses an absolute one
/// (linked worktree, pointing at the shared common dir) as-is.
/// Test: `tests::copied_files_are_added_to_shared_worktree_exclude`,
/// `tests::broken_repo_copies_nothing`,
/// `tests::plain_directory_destination_still_copies`,
/// `tests::missing_git_binary_copies_nothing`.
fn resolve_git_exclude(dest_worktree: &Path) -> ExcludeTarget {
    resolve_git_exclude_with(dest_worktree, "git")
}

/// [`resolve_git_exclude`] with an injectable git program name.
///
/// Why: the spawn-failure arm (no `git` on `PATH`) is a real, security-relevant
/// branch — it must refuse to copy — and the only alternative for reaching it is
/// mutating `PATH`, which is process-global and racy under a parallel test
/// runner. A program-name parameter makes it reachable hermetically.
/// What: identical to [`resolve_git_exclude`]; `git_bin` names the program.
/// Test: `tests::missing_git_binary_copies_nothing`.
fn resolve_git_exclude_with(dest_worktree: &Path, git_bin: &str) -> ExcludeTarget {
    let out = match std::process::Command::new(git_bin)
        .arg("-C")
        .arg(dest_worktree)
        .args(["rev-parse", "--git-path", "info/exclude"])
        .output()
    {
        Ok(out) => out,
        Err(e) => {
            return ExcludeTarget::Unknown(format!(
                "untracked_sync: git rev-parse failed to spawn: {e}"
            ));
        }
    };

    if !out.status.success() {
        return classify_rev_parse_failure(dest_worktree, &String::from_utf8_lossy(&out.stderr));
    }

    let raw_path = String::from_utf8_lossy(&out.stdout).trim().to_string();
    if raw_path.is_empty() {
        return ExcludeTarget::Unknown(
            "untracked_sync: git rev-parse returned an empty path".to_string(),
        );
    }
    let p = PathBuf::from(&raw_path);
    ExcludeTarget::Registered(if p.is_absolute() {
        p
    } else {
        dest_worktree.join(p)
    })
}

/// Why a failed `rev-parse` failed — the gate copying turns on.
///
/// Why: git's "no repository" message is not proof there is no repository. It
/// emits [`NO_REPO_STDERR`] whenever discovery never got far enough to conclude
/// otherwise — an unreadable `.git`, an unreadable `.git/HEAD`, or
/// `GIT_CEILING_DIRECTORIES` stopping the upward walk. In every one of those the
/// repository, and the history a committed secret would land in, are real.
/// `symlink_metadata` on an ancestor `.git` needs only the parent's search bit,
/// so it is a witness git does not use; a disagreement between the two IS the
/// "cannot be asked" state.
/// What: [`ExcludeTarget::NoRepo`] only when the message matches AND no ancestor
/// carries a `.git` entry; [`ExcludeTarget::Unknown`] otherwise.
///
/// 🔴 The `.canonicalize()` is load-bearing, not tidiness. `Path::ancestors`
/// walks the path LEXICALLY, so an uncanonicalised relative path or one reached
/// through a symlink yields a chain that is not the real one — the project's
/// `.git` is never visited and the permissive answer wins, putting a secret in a
/// live worktree. A canonicalisation failure is itself
/// [`ExcludeTarget::Unknown`].
/// Test: `tests::classify_rev_parse_failure_corroborates_the_no_repo_message`,
/// `tests::classify_rev_parse_failure_canonicalises_before_walking_ancestors`,
/// `tests::broken_repo_copies_nothing`.
fn classify_rev_parse_failure(dest_worktree: &Path, stderr: &str) -> ExcludeTarget {
    if !stderr.contains(NO_REPO_STDERR) {
        return ExcludeTarget::Unknown(format!(
            "untracked_sync: git rev-parse --git-path info/exclude failed: {}",
            stderr.trim()
        ));
    }
    match dest_worktree.canonicalize() {
        Ok(abs)
            if !abs
                .ancestors()
                .any(|p| p.join(".git").symlink_metadata().is_ok()) =>
        {
            ExcludeTarget::NoRepo
        }
        _ => ExcludeTarget::Unknown(
            "untracked_sync: git reported no repository, but a `.git` entry exists at or \
             above the destination — the repository is real and could not be read"
                .to_string(),
        ),
    }
}

/// Ask git itself whether `relative_path` is ignored inside `dest_worktree`.
///
/// Why: appending to `info/exclude` succeeds unconditionally and proves nothing
/// — a `.gitignore` negation elsewhere can override it, and a path the repo
/// already TRACKS is never ignored no matter what the exclude file says (git
/// deliberately reports tracked paths as not-ignored here). `git check-ignore`
/// is git's OWN authority on whether `git add -A` would stage the path, which is
/// exactly the question. Mirrors
/// [`crate::core::session_launch::native_mcp::is_env_local_actually_ignored`]
/// rather than inventing a second mechanism (#4733).
/// What: runs `git -C <dest_worktree> check-ignore -q -- <relative_path>` via
/// `.output()` (NOT `.status()`) so nothing the child writes can reach this
/// process's stdout, which must stay clean for MCP JSON-RPC framing. Any
/// non-zero exit — "not ignored", not a repo, no `git` binary — is `false`.
/// Never panics.
/// Test: `tests::copied_files_are_added_to_shared_worktree_exclude`,
/// `tests::tracked_secret_is_not_overwritten_in_worktree`.
fn is_path_git_ignored(dest_worktree: &Path, relative_path: &str) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dest_worktree)
        .args(["check-ignore", "-q", "--", relative_path])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// Append `relative_paths` to an already-resolved `info/exclude` file.
///
/// Why: split from [`append_to_git_exclude`] so the sync flow resolves the
/// exclude path exactly once (#4733 moved that resolution ahead of the copy).
/// What: creates the file's parent dir and appends each path not already
/// present, preserving the read-then-append idempotency
/// [`super::ensure_worktrees_gitignored`] uses.
/// Test: `tests::append_to_git_exclude_is_idempotent`.
fn append_paths_to_exclude(exclude_path: &Path, relative_paths: &[String]) -> Result<(), String> {
    if let Some(parent) = exclude_path.parent() {
        fs::create_dir_all(parent)
            .map_err(|e| format!("untracked_sync: could not create {}: {e}", parent.display()))?;
    }

    let existing = fs::read_to_string(exclude_path).unwrap_or_default();
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
        .open(exclude_path)
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
