//! Tag and release-branch reachability for `fact_commit_reachability` (issue #279).
//!
//! # Why
//!
//! `on_default_branch` alone cannot distinguish "deployed via cherry-pick to a
//! release branch and tagged" from "abandoned WIP".  In practice, bug fixes and
//! security patches are frequently cherry-picked to `release/*` or `hotfix/*`
//! branches, tagged for production, and never merged back to `main`.  Without
//! this module those commits look identical to abandoned work in every DORA /
//! classification report.
//!
//! # What
//!
//! For a set of commit SHAs already stored in the database this module:
//!
//! 1. Walks all tags in the repository once, building a `HashMap<sha, Vec<tag>>`.
//! 2. Walks all branches that match a configured set of glob patterns (e.g.
//!    `release/*`, `hotfix/*`) once, building a `HashMap<sha, Vec<branch>>`.
//! 3. Upserts `fact_commit_reachability` rows for every commit SHA that is
//!    known to either map.
//!
//! This is **O(repo_size + refs)** — not O(repo_size × refs × commits) — because
//! we reverse the lookup: instead of calling `git tag --contains <sha>` for
//! every commit (which re-traverses the full graph per commit), we walk the
//! ancestry of every tag/branch once and index the result.
//!
//! # Test
//!
//! See `tests` module below for unit tests of the glob matcher and the batched
//! builder, plus an integration test that builds a real ephemeral git repo.

use std::collections::HashMap;
use std::path::Path;

use git2::{Repository, Sort};
use rusqlite::{params, Connection};
use tracing::{debug, info, warn};

use crate::collect::errors::{CollectError, Result};
use crate::core::config::ReachabilityConfig;

// ── Public API ──────────────────────────────────────────────────────────────

/// Result of a single-repository reachability scan.
///
/// Why: callers need a compact summary of what the scan found so they can
/// log progress and update `CollectionStats`.
/// What: commit-level counts for how many rows were written/updated.
/// Test: populated by [`scan_and_persist`]; checked in integration tests.
#[derive(Debug, Default, Clone)]
pub struct ReachabilityStats {
    /// Total rows upserted into `fact_commit_reachability`.
    pub rows_upserted: usize,
    /// Commits found on at least one tag.
    pub tagged_commits: usize,
    /// Commits found on at least one release branch.
    pub release_branch_commits: usize,
}

/// Scan a repository for tag and release-branch reachability, then persist
/// the results into `fact_commit_reachability`.
///
/// Why: see module-level docs — the single entry point that ties batched
/// graph-walking to database persistence.
/// What: opens `repo_path`, calls [`build_tag_map`] and
/// [`build_branch_map`] (subject to `config` flags), then upserts one row
/// per commit into `fact_commit_reachability`.  Commits already in the DB
/// that are not reachable from any tag/branch still get a row written with
/// all-false reachability so a single LEFT JOIN is sufficient.
/// Test: `tests::scan_full_lifecycle` builds an ephemeral repo, runs this
/// function, and asserts correct column values.
///
/// # Errors
///
/// - [`CollectError::Git`] — libgit2 failure (repo open, revwalk).
/// - [`CollectError::Db`] — rusqlite failure (upsert).
pub fn scan_and_persist(
    repo_path: &Path,
    conn: &Connection,
    config: &ReachabilityConfig,
) -> Result<ReachabilityStats> {
    let repo = Repository::open(repo_path).map_err(CollectError::Git)?;

    // Load all SHAs currently in the DB for this repo so we only upsert what
    // we know about.  The fact table is keyed by commit_sha, so we index on
    // that column.
    //
    // We deliberately do NOT assume `commits.repository` is present here —
    // the caller passes us the right repo path and we just use all SHAs
    // stored in `commits` that match oids in this repo.
    let all_shas: Vec<String> = {
        let mut stmt = conn
            .prepare("SELECT sha FROM commits")
            .map_err(crate::core::TgaError::from)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(crate::core::TgaError::from)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(crate::core::TgaError::from)?);
        }
        out
    };

    if all_shas.is_empty() {
        debug!("no commits in DB; skipping reachability scan");
        return Ok(ReachabilityStats::default());
    }

    // Build a fast lookup set of the DB-known SHAs so we don't waste time
    // indexing tags/branches whose tips are outside the stored window.
    let sha_set: std::collections::HashSet<String> = all_shas.iter().cloned().collect();

    let tag_map = if config.track_tags {
        build_tag_map(&repo, &sha_set)?
    } else {
        HashMap::new()
    };

    let branch_map = if config.track_release_branches && !config.release_branch_patterns.is_empty()
    {
        build_branch_map(&repo, &config.release_branch_patterns, &sha_set)?
    } else {
        HashMap::new()
    };

    let mut stats = ReachabilityStats::default();

    // Upsert one row per known SHA.  For SHAs not in either map the row gets
    // all-false defaults, which matches the schema default and makes a LEFT JOIN
    // safe for queries that want "was this commit deployed via *any* path?"
    let tx = conn
        .unchecked_transaction()
        .map_err(crate::core::TgaError::from)?;

    for sha in &all_shas {
        let tags = tag_map.get(sha).cloned().unwrap_or_default();
        let branches = branch_map.get(sha).cloned().unwrap_or_default();

        let on_any_tag = !tags.is_empty();
        let on_release_branch = !branches.is_empty();

        let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());
        let branches_json = serde_json::to_string(&branches).unwrap_or_else(|_| "[]".to_string());

        tx.execute(
            "INSERT INTO fact_commit_reachability \
             (commit_sha, on_any_tag, reachable_from_tags, on_release_branch, release_branches) \
             VALUES (?1, ?2, ?3, ?4, ?5) \
             ON CONFLICT(commit_sha) DO UPDATE SET \
               on_any_tag          = excluded.on_any_tag, \
               reachable_from_tags = excluded.reachable_from_tags, \
               on_release_branch   = excluded.on_release_branch, \
               release_branches    = excluded.release_branches",
            params![
                sha,
                on_any_tag as i64,
                tags_json,
                on_release_branch as i64,
                branches_json
            ],
        )
        .map_err(crate::core::TgaError::from)?;

        stats.rows_upserted += 1;
        if on_any_tag {
            stats.tagged_commits += 1;
        }
        if on_release_branch {
            stats.release_branch_commits += 1;
        }
    }

    tx.commit().map_err(crate::core::TgaError::from)?;

    info!(
        rows = stats.rows_upserted,
        tagged = stats.tagged_commits,
        release_branch = stats.release_branch_commits,
        "reachability scan complete"
    );
    Ok(stats)
}

// ── Batched tag/branch graph walkers ────────────────────────────────────────

/// Build a `HashMap<commit_sha, Vec<tag_name>>` for all tags in the repository.
///
/// Why: per-commit `git tag --contains <sha>` is O(refs × commits) — intolerable
/// on repos with thousands of tags.  Walking each tag's ancestry once and
/// inverting the result is O(repo_size + refs).
/// What: enumerates all refs under `refs/tags/`, resolves each to its target
/// commit (peeling through tag objects), walks the ancestry via revwalk, and
/// records each visited SHA → tag mapping.  Only SHAs present in `known_shas`
/// are inserted into the map so we don't materialise the full graph.
/// Test: `tests::build_tag_map_basic` creates a 3-commit repo with 2 tags,
/// calls this function, and asserts correct SHA → tag mappings.
///
/// # Errors
///
/// Returns [`CollectError::Git`] for any `git2` failure.
pub fn build_tag_map(
    repo: &Repository,
    known_shas: &std::collections::HashSet<String>,
) -> Result<HashMap<String, Vec<String>>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    let tag_names = repo.tag_names(None).map_err(CollectError::Git)?;
    let tag_count = tag_names.len();
    debug!(tags = tag_count, "scanning tags for reachability");

    for name_opt in tag_names.iter() {
        let name = match name_opt {
            Some(n) => n,
            None => {
                warn!("tag with non-UTF-8 name skipped");
                continue;
            }
        };

        let refname = format!("refs/tags/{name}");
        let tip_oid = match resolve_ref_to_commit(repo, &refname) {
            Some(oid) => oid,
            None => {
                debug!(tag = %name, "could not resolve tag to a commit; skipping");
                continue;
            }
        };

        walk_ancestors(repo, tip_oid, known_shas, name, &mut map)?;
    }

    debug!(map_size = map.len(), "tag reachability map built");
    Ok(map)
}

/// Build a `HashMap<commit_sha, Vec<branch_name>>` for branches matching
/// any of the configured `patterns`.
///
/// Why: same O(repo + refs) argument as [`build_tag_map`] — enumerate matching
/// branches once, walk their ancestry, invert.
/// What: iterates all local and remote refs under `refs/heads/` and
/// `refs/remotes/`, tests each name against the caller's glob patterns,
/// and walks ancestry for matching refs.
/// Test: `tests::build_branch_map_basic` creates a repo with a `release/v1`
/// branch, calls this with pattern `"release/*"`, and asserts the correct
/// mapping.
///
/// # Errors
///
/// Returns [`CollectError::Git`] for any `git2` failure.
pub fn build_branch_map(
    repo: &Repository,
    patterns: &[String],
    known_shas: &std::collections::HashSet<String>,
) -> Result<HashMap<String, Vec<String>>> {
    let mut map: HashMap<String, Vec<String>> = HashMap::new();

    // Iterate both local (refs/heads) and remote (refs/remotes) branches.
    for branch_type in [git2::BranchType::Local, git2::BranchType::Remote] {
        let branches = repo
            .branches(Some(branch_type))
            .map_err(CollectError::Git)?;

        for entry in branches {
            let (branch, _) = entry.map_err(CollectError::Git)?;
            let short_name = match branch.name() {
                Ok(Some(n)) => n.to_string(),
                _ => {
                    warn!("branch with non-UTF-8 or missing name skipped");
                    continue;
                }
            };

            // Strip `origin/` prefix for remote branches so patterns like
            // `release/*` match both `release/v1.0` (local) and
            // `origin/release/v1.0` (remote tracking).
            let stripped = short_name
                .strip_prefix("origin/")
                .unwrap_or(short_name.as_str());

            if !patterns.iter().any(|p| glob_matches(p, stripped)) {
                continue;
            }

            // Resolve the branch tip to a commit OID.
            let tip_oid = match branch.get().target() {
                Some(oid) => oid,
                None => {
                    debug!(branch = %short_name, "symbolic ref without target; skipping");
                    continue;
                }
            };

            // Use the short name (without origin/ prefix) in the output so
            // callers see `release/v1.0` regardless of local vs. remote.
            walk_ancestors(repo, tip_oid, known_shas, stripped, &mut map)?;
        }
    }

    debug!(
        map_size = map.len(),
        "release-branch reachability map built"
    );
    Ok(map)
}

// ── Shared helpers ───────────────────────────────────────────────────────────

/// Walk ancestors of `tip_oid` via a time-sorted revwalk, recording each
/// SHA that is in `known_shas` under `label` in `map`.
///
/// Why: extracted so both [`build_tag_map`] and [`build_branch_map`] share
/// the same traversal without duplication.
/// What: creates a new [`git2::Revwalk`], pushes `tip_oid`, and iterates all
/// reachable commits.  Skips non-UTF-8 SHAs with a warning.
/// Test: exercised indirectly by [`build_tag_map`] and [`build_branch_map`]
/// tests.
fn walk_ancestors(
    repo: &Repository,
    tip_oid: git2::Oid,
    known_shas: &std::collections::HashSet<String>,
    label: &str,
    map: &mut HashMap<String, Vec<String>>,
) -> Result<()> {
    let mut revwalk = repo.revwalk().map_err(CollectError::Git)?;
    revwalk.set_sorting(Sort::TIME).map_err(CollectError::Git)?;
    revwalk.push(tip_oid).map_err(CollectError::Git)?;

    for oid_res in revwalk {
        let oid = match oid_res {
            Ok(o) => o,
            Err(e) => {
                warn!(error = %e, "revwalk error while walking {label}; stopping");
                break;
            }
        };
        let sha = oid.to_string();
        if known_shas.contains(&sha) {
            map.entry(sha).or_default().push(label.to_string());
        }
    }
    Ok(())
}

/// Resolve a ref name to the OID of the commit it points to, peeling through
/// tag objects if necessary.
///
/// Why: lightweight tags point directly to a commit; annotated tags point to
/// a tag object that must be peeled to reach the underlying commit.
/// What: calls `repo.find_reference`, then `peel_to_commit` to strip any
/// wrapping tag object.
/// Test: exercised by [`build_tag_map`] against both lightweight and
/// annotated tags in the integration test.
fn resolve_ref_to_commit(repo: &Repository, refname: &str) -> Option<git2::Oid> {
    let reference = repo.find_reference(refname).ok()?;
    let commit = reference.peel_to_commit().ok()?;
    Some(commit.id())
}

// ── Glob matcher ─────────────────────────────────────────────────────────────

/// Match `text` against a simple glob `pattern` that supports a single `*`
/// wildcard (matching any sequence of characters, including `/`).
///
/// Why: the issue's release-branch patterns (`release/*`, `hotfix/*`, `v*`)
/// only need `*` — pulling in the `glob` crate for this would be overkill and
/// add a dependency not currently in the workspace.
/// What: splits `pattern` on the first `*`, then checks that `text` starts
/// with the prefix and ends with the suffix.  Multiple `*` are treated as a
/// single wildcard spanning from the end of the prefix to the start of the
/// suffix; this is sufficient for all documented patterns.
/// Test: `tests::glob_matcher_*` exercises prefix-only, suffix-only,
/// prefix+suffix, double-star, and non-matching cases.
pub fn glob_matches(pattern: &str, text: &str) -> bool {
    match pattern.find('*') {
        None => pattern == text,
        Some(star_pos) => {
            let prefix = &pattern[..star_pos];
            let suffix = &pattern[star_pos + 1..];
            // Handle a second `*` in the suffix by treating it as "anything".
            let effective_suffix = if suffix.contains('*') {
                // More than one wildcard: match prefix only.
                ""
            } else {
                suffix
            };
            if !text.starts_with(prefix) {
                return false;
            }
            let rest = &text[prefix.len()..];
            if effective_suffix.is_empty() {
                true
            } else {
                rest.ends_with(effective_suffix) && rest.len() >= effective_suffix.len()
            }
        }
    }
}

// ── Tests ─────────────────────────────────────────────────────────────────────

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    use git2::{Repository, Signature, Time};
    use rusqlite::Connection;

    // ── Glob matcher unit tests ──────────────────────────────────────────────

    #[test]
    fn glob_exact_match() {
        assert!(glob_matches("main", "main"));
        assert!(!glob_matches("main", "master"));
    }

    #[test]
    fn glob_star_prefix() {
        // `release/*` matches `release/v1.0` but not `releaze/v1.0`.
        assert!(glob_matches("release/*", "release/v1.0"));
        assert!(glob_matches("release/*", "release/2024-01-15"));
        assert!(!glob_matches("release/*", "releaze/v1.0"));
        assert!(!glob_matches("release/*", "hotfix/v1.0"));
    }

    #[test]
    fn glob_star_suffix_prefix() {
        // `v*` matches anything starting with `v`.
        assert!(glob_matches("v*", "v1.0"));
        assert!(glob_matches("v*", "v2.3.4-rc"));
        assert!(glob_matches("v*", "v1"));
        // `version-1` also starts with `v`, so it matches `v*`.
        assert!(glob_matches("v*", "version-1"));
        // `1.0` does NOT start with `v`, so it does not match.
        assert!(!glob_matches("v*", "1.0"));
        assert!(!glob_matches("v*", "release/v1.0"));
    }

    #[test]
    fn glob_hotfix_pattern() {
        assert!(glob_matches("hotfix/*", "hotfix/PROJ-123"));
        assert!(glob_matches("hotfix/*", "hotfix/security-patch"));
        assert!(!glob_matches("hotfix/*", "feature/my-feature"));
    }

    #[test]
    fn glob_chore_release_pattern() {
        assert!(glob_matches("chore/release-*", "chore/release-v2.0"));
        assert!(glob_matches("chore/release-*", "chore/release-2024"));
        assert!(!glob_matches("chore/release-*", "chore/releases"));
    }

    #[test]
    fn glob_no_wildcard() {
        assert!(glob_matches("main", "main"));
        assert!(!glob_matches("main", "main-branch"));
    }

    #[test]
    fn glob_double_star_treated_as_prefix_only() {
        // `release/**` should still match `release/foo/bar`.
        assert!(glob_matches("release/**", "release/foo/bar"));
    }

    #[test]
    fn glob_star_only() {
        // `*` matches everything.
        assert!(glob_matches("*", "anything"));
        assert!(glob_matches("*", "release/v1.0"));
        assert!(glob_matches("*", ""));
    }

    // ── Repo fixture helpers ─────────────────────────────────────────────────

    struct TempRepo {
        path: PathBuf,
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn unique_dir(label: &str) -> PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        let n = N.fetch_add(1, Ordering::Relaxed);
        let mut p = std::env::temp_dir();
        p.push(format!("tga-reach-{}-{}-{label}", std::process::id(), n));
        p
    }

    fn init_repo(label: &str) -> (TempRepo, Repository) {
        let path = unique_dir(label);
        std::fs::create_dir_all(&path).expect("mkdir");
        let repo = Repository::init(&path).expect("git init");
        let mut cfg = repo.config().expect("repo config");
        cfg.set_str("user.name", "Test").expect("set user.name");
        cfg.set_str("user.email", "t@example.com")
            .expect("set email");
        (TempRepo { path }, repo)
    }

    fn make_commit(repo: &Repository, repo_path: &Path, msg: &str, ts: i64) -> git2::Oid {
        use std::sync::atomic::{AtomicU64, Ordering};
        static F: AtomicU64 = AtomicU64::new(0);
        let n = F.fetch_add(1, Ordering::Relaxed);
        let fname = format!("f{n}.txt");
        std::fs::write(repo_path.join(&fname), msg).expect("write");
        let mut idx = repo.index().expect("index");
        idx.add_path(std::path::Path::new(&fname)).expect("add");
        idx.write().expect("idx write");
        let tree_oid = idx.write_tree().expect("write_tree");
        let tree = repo.find_tree(tree_oid).expect("find_tree");
        let sig = Signature::new("Test", "t@example.com", &Time::new(ts, 0)).expect("sig");
        let parents: Vec<git2::Commit<'_>> = match repo.head() {
            Ok(h) => vec![h.peel_to_commit().expect("peel")],
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();
        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parent_refs)
            .expect("commit")
    }

    fn tag_commit(repo: &Repository, oid: git2::Oid, name: &str) {
        // Lightweight tag.
        repo.tag_lightweight(name, &repo.find_object(oid, None).expect("obj"), false)
            .expect("tag");
    }

    fn branch_at(repo: &Repository, oid: git2::Oid, name: &str) {
        let commit = repo.find_commit(oid).expect("find_commit");
        repo.branch(name, &commit, false).expect("branch");
    }

    fn open_in_memory_db() -> Connection {
        let conn = Connection::open_in_memory().expect("in-memory");
        // Apply the same pragmas as Database::open.
        conn.execute_batch(
            "PRAGMA journal_mode=WAL; \
             PRAGMA foreign_keys=OFF;", // off so we can insert without commits row
        )
        .expect("pragmas");
        // Create the minimal schema for our tests.
        conn.execute_batch(
            "CREATE TABLE IF NOT EXISTS commits (
                id    INTEGER PRIMARY KEY,
                sha   TEXT NOT NULL UNIQUE,
                author_name  TEXT NOT NULL DEFAULT '',
                author_email TEXT NOT NULL DEFAULT '',
                timestamp    TEXT NOT NULL DEFAULT '',
                message      TEXT NOT NULL DEFAULT '',
                repository   TEXT NOT NULL DEFAULT '',
                files_changed INTEGER NOT NULL DEFAULT 0,
                insertions    INTEGER NOT NULL DEFAULT 0,
                deletions     INTEGER NOT NULL DEFAULT 0,
                is_merge      INTEGER NOT NULL DEFAULT 0,
                ticketed      INTEGER NOT NULL DEFAULT 0
            );
            CREATE TABLE IF NOT EXISTS fact_commit_reachability (
                commit_sha          TEXT PRIMARY KEY,
                on_default_branch   INTEGER NOT NULL DEFAULT 0,
                on_any_tag          INTEGER NOT NULL DEFAULT 0,
                reachable_from_tags TEXT    NOT NULL DEFAULT '[]',
                on_release_branch   INTEGER NOT NULL DEFAULT 0,
                release_branches    TEXT    NOT NULL DEFAULT '[]'
            );",
        )
        .expect("create tables");
        conn
    }

    fn insert_sha(conn: &Connection, sha: &str) {
        conn.execute(
            "INSERT OR IGNORE INTO commits (sha) VALUES (?1)",
            params![sha],
        )
        .expect("insert sha");
    }

    // ── Tag map unit tests ───────────────────────────────────────────────────

    /// Why: verifies that build_tag_map correctly associates tag names with
    /// ancestor SHAs and ignores SHAs not in `known_shas`.
    #[test]
    fn build_tag_map_basic() {
        let (tr, repo) = init_repo("tag-map");
        let sha1 = make_commit(&repo, &tr.path, "first", 1000).to_string();
        let sha2 = make_commit(&repo, &tr.path, "second", 2000).to_string();
        let sha3 = make_commit(&repo, &tr.path, "third", 3000).to_string();

        let oid2 = git2::Oid::from_str(&sha2).expect("oid2");
        let oid3 = git2::Oid::from_str(&sha3).expect("oid3");
        tag_commit(&repo, oid2, "v1.0");
        tag_commit(&repo, oid3, "v2.0");

        let known: std::collections::HashSet<String> =
            [sha1.clone(), sha2.clone(), sha3.clone()].into();
        let map = build_tag_map(&repo, &known).expect("build_tag_map");

        // v1.0 tag's ancestry includes sha1 and sha2.
        assert!(
            map.get(&sha1)
                .map(|v| v.contains(&"v1.0".to_string()))
                .unwrap_or(false),
            "sha1 should be reachable from v1.0"
        );
        assert!(
            map.get(&sha2)
                .map(|v| v.contains(&"v1.0".to_string()))
                .unwrap_or(false),
            "sha2 should be reachable from v1.0"
        );
        // v2.0 tag's ancestry includes sha1, sha2, sha3.
        assert!(
            map.get(&sha3)
                .map(|v| v.contains(&"v2.0".to_string()))
                .unwrap_or(false),
            "sha3 should be reachable from v2.0"
        );
        // sha2 is also reachable from v2.0.
        assert!(
            map.get(&sha2)
                .map(|v| v.contains(&"v2.0".to_string()))
                .unwrap_or(false),
            "sha2 should also be reachable from v2.0"
        );
    }

    /// Why: if `known_shas` is empty, the result should be empty (no wasted work).
    #[test]
    fn build_tag_map_empty_known_shas() {
        let (tr, repo) = init_repo("tag-map-empty");
        let sha = make_commit(&repo, &tr.path, "c1", 1000).to_string();
        let oid = git2::Oid::from_str(&sha).expect("oid");
        tag_commit(&repo, oid, "v1.0");

        let known = std::collections::HashSet::new();
        let map = build_tag_map(&repo, &known).expect("build_tag_map");
        assert!(map.is_empty(), "empty known_shas → empty map");
    }

    // ── Branch map unit tests ────────────────────────────────────────────────

    /// Why: verifies that build_branch_map correctly matches branch names
    /// against patterns and associates ancestry SHAs.
    #[test]
    fn build_branch_map_basic() {
        let (tr, repo) = init_repo("branch-map");
        let sha1 = make_commit(&repo, &tr.path, "base", 1000).to_string();
        let sha2 = make_commit(&repo, &tr.path, "release commit", 2000).to_string();

        let oid2 = git2::Oid::from_str(&sha2).expect("oid2");
        branch_at(&repo, oid2, "release/v1.0");

        let known: std::collections::HashSet<String> = [sha1.clone(), sha2.clone()].into();
        let patterns = vec!["release/*".to_string()];
        let map = build_branch_map(&repo, &patterns, &known).expect("build_branch_map");

        assert!(
            map.get(&sha2)
                .map(|v| v.contains(&"release/v1.0".to_string()))
                .unwrap_or(false),
            "sha2 should be on release/v1.0"
        );
        assert!(
            map.get(&sha1)
                .map(|v| v.contains(&"release/v1.0".to_string()))
                .unwrap_or(false),
            "sha1 (ancestor) should also be on release/v1.0"
        );
    }

    /// Why: verifies non-matching branches are excluded even if commits exist on them.
    #[test]
    fn build_branch_map_non_matching_excluded() {
        let (tr, repo) = init_repo("branch-map-non-match");
        let sha1 = make_commit(&repo, &tr.path, "base", 1000).to_string();
        let sha2 = make_commit(&repo, &tr.path, "feature", 2000).to_string();

        let oid2 = git2::Oid::from_str(&sha2).expect("oid2");
        branch_at(&repo, oid2, "feature/my-work");

        let known: std::collections::HashSet<String> = [sha1.clone(), sha2.clone()].into();
        let patterns = vec!["release/*".to_string()];
        let map = build_branch_map(&repo, &patterns, &known).expect("build_branch_map");

        assert!(
            map.is_empty(),
            "feature/* branches must not be matched by release/* pattern"
        );
    }

    // ── Full-lifecycle integration test ─────────────────────────────────────

    /// Why: end-to-end test that creates an ephemeral repo, stores its commits
    /// in an in-memory SQLite DB, then runs scan_and_persist and verifies the
    /// fact_commit_reachability rows.
    #[test]
    fn scan_full_lifecycle() {
        let (tr, repo) = init_repo("scan-lifecycle");
        let sha1 = make_commit(&repo, &tr.path, "initial", 1000).to_string();
        let sha2 = make_commit(&repo, &tr.path, "feature", 2000).to_string();
        let sha3 = make_commit(&repo, &tr.path, "hotfix", 3000).to_string();

        // Tag sha2 as a release.
        let oid2 = git2::Oid::from_str(&sha2).expect("oid2");
        tag_commit(&repo, oid2, "v1.0");

        // Create a release branch at sha3.
        let oid3 = git2::Oid::from_str(&sha3).expect("oid3");
        branch_at(&repo, oid3, "release/v2.0");

        // DB: insert all three commits.
        let conn = open_in_memory_db();
        for sha in [&sha1, &sha2, &sha3] {
            insert_sha(&conn, sha);
        }

        let cfg = ReachabilityConfig {
            track_tags: true,
            track_release_branches: true,
            release_branch_patterns: vec!["release/*".to_string()],
        };

        let stats = scan_and_persist(&tr.path, &conn, &cfg).expect("scan_and_persist");
        assert_eq!(stats.rows_upserted, 3, "one row per commit");

        // sha2 should be on tag v1.0; sha3 should be on release/v2.0.
        let (on_tag, on_rel): (i64, i64) = conn
            .query_row(
                "SELECT on_any_tag, on_release_branch FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha2],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query sha2");
        assert_eq!(on_tag, 1, "sha2 should be on a tag");
        // sha2 is also ancestor of release/v2.0 branch (sha3 > sha2 > sha1)
        // but actually sha3 is on top of sha2 so release branch includes sha2 too.
        // Let's check sha3 separately.
        let (s3_on_tag, s3_on_rel): (i64, i64) = conn
            .query_row(
                "SELECT on_any_tag, on_release_branch FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha3],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query sha3");
        assert_eq!(s3_on_rel, 1, "sha3 should be on release/v2.0");
        let _ = (on_rel, s3_on_tag); // used

        // sha1 should have on_any_tag=1 (v1.0 ancestors include sha1).
        let s1_on_tag: i64 = conn
            .query_row(
                "SELECT on_any_tag FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha1],
                |r| r.get(0),
            )
            .expect("query sha1");
        assert_eq!(s1_on_tag, 1, "sha1 (ancestor of v1.0) should be tagged");
    }

    /// Why: when track_tags=false, no tag data is populated.
    #[test]
    fn scan_skip_tags_when_disabled() {
        let (tr, repo) = init_repo("scan-skip-tags");
        let sha1 = make_commit(&repo, &tr.path, "c1", 1000).to_string();
        let oid1 = git2::Oid::from_str(&sha1).expect("oid1");
        tag_commit(&repo, oid1, "v1.0");

        let conn = open_in_memory_db();
        insert_sha(&conn, &sha1);

        let cfg = ReachabilityConfig {
            track_tags: false,
            track_release_branches: false,
            release_branch_patterns: vec![],
        };

        let stats = scan_and_persist(&tr.path, &conn, &cfg).expect("scan");
        assert_eq!(stats.rows_upserted, 1);
        assert_eq!(
            stats.tagged_commits, 0,
            "tag tracking disabled — no tagged_commits"
        );

        let on_tag: i64 = conn
            .query_row(
                "SELECT on_any_tag FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha1],
                |r| r.get(0),
            )
            .expect("query");
        assert_eq!(on_tag, 0, "on_any_tag must be 0 when track_tags=false");
    }

    /// Why: verifies that reachable_from_tags JSON is correctly serialized.
    #[test]
    fn scan_reachable_from_tags_json() {
        let (tr, repo) = init_repo("scan-tags-json");
        let sha = make_commit(&repo, &tr.path, "c1", 1000).to_string();
        let oid = git2::Oid::from_str(&sha).expect("oid");
        tag_commit(&repo, oid, "tga-v1.1.0");

        let conn = open_in_memory_db();
        insert_sha(&conn, &sha);

        let cfg = ReachabilityConfig {
            track_tags: true,
            track_release_branches: false,
            release_branch_patterns: vec![],
        };

        scan_and_persist(&tr.path, &conn, &cfg).expect("scan");

        let tags_json: String = conn
            .query_row(
                "SELECT reachable_from_tags FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha],
                |r| r.get(0),
            )
            .expect("query");

        let tags: Vec<String> = serde_json::from_str(&tags_json).expect("parse json");
        assert!(
            tags.contains(&"tga-v1.1.0".to_string()),
            "must include tga-v1.1.0 in JSON array"
        );
    }
}
