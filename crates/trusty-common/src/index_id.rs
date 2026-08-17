//! Canonical trusty-search index-id derivation from a project path.
//!
//! Why: trusty-search derives an index id from the current project (the
//! git-root basename, fallback to the cwd basename) when serving CLI queries,
//! but the MCP `serve` path never reached that logic — so trusty-mpm, which
//! injects a contextless `trusty-search serve` MCP stub, left index selection
//! to the LLM and routinely resolved the WRONG index (issue #1373). To
//! register-and-pin the correct project index, BOTH trusty-mpm (at session
//! launch) and trusty-search (in `detect_project`) must derive the *identical*
//! id from the same project root. Centralising the one rule here in
//! `trusty-common` — which both crates already depend on, and which avoids a
//! trusty-mpm → trusty-search dependency edge (trusty-search pulls the heavy
//! ONNX/usearch stack) — makes it the single source of truth so the two cannot
//! silently diverge.
//!
//! What: [`resolve_project_root`] walks up from a starting directory to the
//! nearest `.git` root (fallback: the start dir itself), and
//! [`derive_index_id`] turns a project root into its index id (the path
//! basename, preserved verbatim for backward-compatibility with already-indexed
//! projects). [`identifies_same_path`] answers "do these two paths name the same
//! directory tree?" for every registration guard that has to compare one. No
//! global state; pure functions.
//!
//! Test: `cargo test -p trusty-common --features unconditional-only --
//! index_id::tests` covers basename derivation, the git-root walk, the
//! no-marker fallback, and same-tree identity across case variants.

use std::path::{Path, PathBuf};

/// Walk up from `start` to the nearest directory containing a `.git` entry.
///
/// Why: a trusty-search index is keyed to a project root, and the canonical
/// project root is the git repository root. Both trusty-mpm (resolving the
/// session's project) and trusty-search (`detect_project`) must agree on which
/// directory is "the project root" so they derive the same index id (#1373).
/// What: returns the first ancestor of `start` (inclusive) that contains a
/// `.git` directory or file; when none is found, returns `start` itself
/// (a path with no enclosing git repo is still indexable by its own basename).
///
/// Known limitation: this returns the FIRST (innermost) ancestor with a `.git`,
/// so in a nested-repo / monorepo layout where a parent directory above the
/// intended project also has `.git`, the *inner* repo wins — and if the project
/// itself has no `.git` but a parent does, that parent `.git` would win. This
/// matches trusty-search's prior `detect_project` semantics (the two must agree
/// to derive the same index id), so it is intentional, not a bug; documented
/// here so a future monorepo-aware override is a conscious change, not a surprise.
/// Test: `resolve_project_root_finds_git_root` and
/// `resolve_project_root_falls_back_to_start` in `tests`.
pub fn resolve_project_root(start: &Path) -> PathBuf {
    find_git_root(start).unwrap_or_else(|| start.to_path_buf())
}

/// Walk up from `start` to the nearest `.git` root, returning `None` when no
/// enclosing git repository exists.
///
/// Why: some callers must DISTINGUISH "inside a git repo" from "no repo at all"
/// — [`resolve_project_root`] can't, because it collapses both cases to a
/// `PathBuf` (returning `start` itself on a miss). trusty-code's task-start
/// index hook uses this to cheaply short-circuit the bake-off/scratch case: a
/// throwaway directory with no `.git` has nothing worth registering with
/// trusty-search, so indexing is skipped entirely rather than creating an index
/// keyed to a directory that will be deleted moments later.
/// What: returns `Some(first_ancestor_with_.git)` (inclusive of `start`), or
/// `None` when the walk reaches the filesystem root without finding one. `.git`
/// is matched via `exists()` so both a normal clone (`.git` directory) and a git
/// worktree/submodule (`.git` file) resolve. This is the exact walk
/// [`resolve_project_root`] performs, exposed as an `Option` — the two share one
/// implementation so they can never disagree on which directory is the root.
/// Test: `find_git_root_some_when_repo`, `find_git_root_none_when_no_repo`.
pub fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut current = start.to_path_buf();
    loop {
        // `.git` is a directory in a normal clone and a file in a git worktree
        // / submodule; `exists()` matches both so worktrees resolve correctly.
        if current.join(".git").exists() {
            return Some(current);
        }
        if !current.pop() {
            break;
        }
    }
    None
}

/// Derive the trusty-search index id for a project root.
///
/// Why: the index id is the stable handle every search/grep call targets. It
/// MUST be derived identically wherever it is computed (trusty-mpm's
/// register-and-pin at launch, trusty-search's `detect_project`) or a session
/// would create/pin one id while querying another (#1373).
/// What: returns the final path component of `project_root` as a `String`
/// (lossy on non-UTF-8). The basename is preserved verbatim — NOT slugified —
/// so the derived id byte-for-byte matches the ids trusty-search already
/// assigned to existing on-disk indexes (changing the casing/punctuation would
/// orphan every previously-indexed project). An empty / root path yields the
/// empty string; callers that need a non-empty id must guard that case.
/// Test: `derive_index_id_uses_basename` and `derive_index_id_empty_for_root`
/// in `tests`.
pub fn derive_index_id(project_root: &Path) -> String {
    project_root
        .file_name()
        .unwrap_or_default()
        .to_string_lossy()
        .into_owned()
}

/// Decide whether `a` and `b` name the same on-disk directory tree.
///
/// Why: a trusty-search index identifies a searchable DIRECTORY TREE, so every
/// registration guard has to answer "is this the tree I already have?" — and on
/// macOS APFS (case-insensitive, case-preserving) the obvious answer is wrong.
/// `canonicalize` preserves the case each path was spelled with rather than
/// normalising it, so `/Users/bob/Duetto/CTO` and `/Users/bob/Duetto/cto`
/// canonicalize to two different strings over ONE inode and string equality
/// misses the match entirely. Two guards need this same answer — trusty-search's
/// `find_root_path_collision` (same tree, different id) and trusty-common's
/// `best_effort_create_index` (same id, different tree) — so per this
/// workspace's common-entry-point rule it is one implementation here, not two.
/// Filesystem-only by construction: no git remote, no repo identity, so a
/// non-git tree (trusty-agents indexes an OKF store this way) compares exactly
/// like a checkout.
/// What: compares `(dev, ino)` from `std::fs::metadata` when BOTH paths exist,
/// which also catches symlink aliases, bind mounts and hard-linked directories.
/// When either path cannot be stat'd — it was deleted, a volume was unmounted,
/// or the target is not unix — falls back to plain `Path` equality. That
/// fallback is deliberately the weaker pre-existing behaviour rather than a
/// refusal: a caller comparing against a root that has since vanished should
/// still get an answer, and the dominant case (both trees present) never
/// reaches it.
/// Test: `same_path_spelled_two_ways_is_the_same_tree` (macOS-gated),
/// `distinct_trees_are_not_the_same`, `missing_paths_fall_back_to_equality`.
pub fn identifies_same_path(a: &Path, b: &Path) -> bool {
    match same_filesystem_entry(a, b) {
        Some(same) => same,
        None => a == b,
    }
}

/// Compare `a` and `b` by `(dev, ino)`, or `None` when either cannot be stat'd.
///
/// Why/What/Test: see [`identifies_same_path`], the only caller.
#[cfg(unix)]
fn same_filesystem_entry(a: &Path, b: &Path) -> Option<bool> {
    use std::os::unix::fs::MetadataExt;
    let meta_a = std::fs::metadata(a).ok()?;
    let meta_b = std::fs::metadata(b).ok()?;
    Some(meta_a.dev() == meta_b.dev() && meta_a.ino() == meta_b.ino())
}

/// Non-unix targets have no wired-up `(dev, ino)` equivalent, so
/// [`identifies_same_path`] always falls back to path equality there.
#[cfg(not(unix))]
fn same_filesystem_entry(_a: &Path, _b: &Path) -> Option<bool> {
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::fs;

    fn scratch_dir(tag: &str) -> PathBuf {
        let pid = std::process::id();
        let nanos = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or(0);
        let p = std::env::temp_dir().join(format!("trusty-index-id-{tag}-{pid}-{nanos}"));
        let _ = fs::remove_dir_all(&p);
        p
    }

    #[test]
    fn derive_index_id_uses_basename() {
        assert_eq!(
            derive_index_id(Path::new("/Users/me/code/trusty-tools")),
            "trusty-tools"
        );
        // Casing and punctuation are preserved verbatim (NOT slugified) so the
        // id matches what trusty-search already stored for existing indexes.
        assert_eq!(
            derive_index_id(Path::new("/Users/me/code/MyProject")),
            "MyProject"
        );
        assert_eq!(
            derive_index_id(Path::new("/srv/Repo_With_Underscores")),
            "Repo_With_Underscores"
        );
    }

    #[test]
    fn derive_index_id_empty_for_root() {
        assert_eq!(derive_index_id(Path::new("/")), "");
    }

    #[test]
    fn resolve_project_root_finds_git_root() {
        let tmp = scratch_dir("git");
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let nested = tmp.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();

        let root = resolve_project_root(&nested);
        assert_eq!(root, tmp);
        // And the derived id is the git-root basename, not the nested dir.
        assert_eq!(derive_index_id(&root), derive_index_id(&tmp));

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn resolve_project_root_falls_back_to_start() {
        let tmp = scratch_dir("no-git");
        fs::create_dir_all(&tmp).unwrap();

        let root = resolve_project_root(&tmp);
        assert_eq!(root, tmp);

        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn find_git_root_some_when_repo() {
        let tmp = scratch_dir("fgr-git");
        fs::create_dir_all(tmp.join(".git")).unwrap();
        let nested = tmp.join("a/b/c");
        fs::create_dir_all(&nested).unwrap();

        assert_eq!(find_git_root(&nested), Some(tmp.clone()));

        let _ = fs::remove_dir_all(&tmp);
    }

    /// macOS APFS is case-insensitive but case-preserving, so `canonicalize`
    /// returns the spelling it was GIVEN — two cases of one directory produce
    /// two unequal strings over one inode. This is the exact pair that made a
    /// string-equality guard useless; `identifies_same_path` must see through it.
    #[cfg(target_os = "macos")]
    #[test]
    fn same_path_spelled_two_ways_is_the_same_tree() {
        let tmp = scratch_dir("Case-Variant");
        fs::create_dir_all(&tmp).unwrap();

        let name = tmp.file_name().unwrap().to_str().unwrap();
        let flipped: String = name
            .chars()
            .map(|c| {
                if c.is_ascii_uppercase() {
                    c.to_ascii_lowercase()
                } else {
                    c.to_ascii_uppercase()
                }
            })
            .collect();
        let variant = tmp.with_file_name(flipped);

        assert_ne!(variant, tmp, "the two spellings must differ as strings");
        assert!(
            identifies_same_path(&tmp, &variant),
            "{} and {} are one inode and must compare equal",
            tmp.display(),
            variant.display()
        );

        let _ = fs::remove_dir_all(&tmp);
    }

    /// Two genuinely different trees must not be conflated — the guard has to
    /// stay usable, not just safe.
    #[test]
    fn distinct_trees_are_not_the_same() {
        let a = scratch_dir("distinct-a");
        let b = scratch_dir("distinct-b");
        fs::create_dir_all(&a).unwrap();
        fs::create_dir_all(&b).unwrap();

        assert!(!identifies_same_path(&a, &b));

        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }

    /// When a path cannot be stat'd (deleted root, unmounted volume) there is no
    /// `(dev, ino)` to compare, so the answer degrades to path equality rather
    /// than to a refusal.
    #[test]
    fn missing_paths_fall_back_to_equality() {
        let gone = scratch_dir("never-created");
        let other = scratch_dir("also-never-created");

        assert!(identifies_same_path(&gone, &gone));
        assert!(!identifies_same_path(&gone, &other));
    }

    #[test]
    fn find_git_root_none_when_no_repo() {
        // A scratch dir with no `.git` anywhere up the chain: the tcode
        // short-circuit relies on this returning None so nothing is indexed.
        let tmp = scratch_dir("fgr-no-git");
        fs::create_dir_all(&tmp).unwrap();

        assert_eq!(find_git_root(&tmp), None);

        let _ = fs::remove_dir_all(&tmp);
    }
}
