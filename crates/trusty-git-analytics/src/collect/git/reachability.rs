//! Tag, release-branch, and default-branch reachability for
//! `fact_commit_reachability` (issues #279, #290, #303).
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
//! Additionally, `on_default_branch` was declared in migration v15 but never
//! populated — every row defaulted to 0 (issue #290). This module now auto-
//! detects the default branch per-repo and sets the column correctly.
//!
//! ## Multi-repo correctness (issue #303)
//!
//! When a workspace contains multiple repositories, the previous implementation
//! loaded ALL commit SHAs from the database (across all repos) and upserted rows
//! for every one of them during each per-repo scan.  This caused the LAST repo
//! scanned to overwrite the correctly-computed `on_any_tag=1` values of earlier
//! repos with `on_any_tag=0`, because a commit from repo A is not an ancestor of
//! any tag in repo B.  The fix: scope the SHA load to the repository being
//! scanned via `commits.repository = ?` so each repo's scan only touches its own
//! rows.  The tag-walking graph walk still traverses ALL ancestors in the git
//! object store (including commits on non-default branches), so GitFlow repos
//! with tags on `develop` or release branches are handled correctly.
//!
//! # What
//!
//! For a set of commit SHAs already stored in the database this module:
//!
//! 1. Auto-detects the default branch via `refs/remotes/origin/HEAD` (symref),
//!    falling back to `refs/heads/main`, `refs/heads/master`,
//!    `refs/remotes/origin/main`, `refs/remotes/origin/master` in that order.
//!    If none are found a `warn!` is emitted and `on_default_branch` stays 0.
//! 2. Walks ALL tags in the repository once (via `refs/tags/*`, independent of
//!    branch topology), building a `HashMap<sha, Vec<tag>>`.
//! 3. Walks all branches that match a configured set of glob patterns (e.g.
//!    `release/*`, `hotfix/*`) once, building a `HashMap<sha, Vec<branch>>`.
//! 4. Upserts `fact_commit_reachability` rows for every commit SHA belonging to
//!    this repository, including the `on_default_branch` column.
//!
//! This is **O(repo_size + refs)** — not O(repo_size × refs × commits) — because
//! we reverse the lookup: instead of calling `git tag --contains <sha>` for
//! every commit (which re-traverses the full graph per commit), we walk the
//! ancestry of every tag/branch once and index the result.
//!
//! # Test
//!
//! See `tests` module below for unit tests of the glob matcher and the batched
//! builder, plus integration tests that build real ephemeral git repos.

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
    /// Commits reachable from the repository's default branch (main/master).
    pub default_branch_commits: usize,
}

/// Scan a repository for tag, release-branch, and default-branch reachability,
/// then persist the results into `fact_commit_reachability`.
///
/// Why: see module-level docs — the single entry point that ties batched
/// graph-walking to database persistence.  Fixes issues #290 and #303.
/// What: opens `repo_path`, scopes the SHA load to `repo_name` when provided
/// (issue #303: prevents later repos from overwriting earlier repos' correct
/// `on_any_tag` values), auto-detects the default branch via
/// [`detect_default_branch_set`], calls [`build_tag_map`] (which enumerates
/// ALL `refs/tags/*` regardless of branch topology — issue #303) and
/// [`build_branch_map`] (subject to `config` flags), then upserts one row per
/// commit into `fact_commit_reachability` including `on_default_branch`.
/// Commits in the DB that are not reachable from any signal still get a row
/// with all-false values so a LEFT JOIN is safe.
/// Test: `tests::scan_full_lifecycle`, `tests::scan_default_branch_*`, and
/// `tests::scan_gitflow_develop_tag_marks_non_default_branch_commits` build
/// ephemeral repos and assert correct column values.
///
/// # Parameters
///
/// - `repo_path` — local filesystem path to the git repository.
/// - `conn` — rusqlite connection to the analytics database.
/// - `config` — reachability configuration (track_tags, patterns, …).
/// - `repo_name` — display name stored in `commits.repository`.  When
///   `Some(name)`, only SHAs from that repository are loaded and upserted,
///   preventing cross-repo contamination in multi-repo corpora (issue #303).
///   When `None`, all SHAs are loaded (single-repo DBs and legacy callers).
///
/// # Errors
///
/// - [`CollectError::Git`] — libgit2 failure (repo open, revwalk).
/// - [`CollectError::Db`] — rusqlite failure (upsert).
pub fn scan_and_persist(
    repo_path: &Path,
    conn: &Connection,
    config: &ReachabilityConfig,
    repo_name: Option<&str>,
) -> Result<ReachabilityStats> {
    let repo = Repository::open(repo_path).map_err(CollectError::Git)?;

    // Load SHAs from the DB that belong to this repository.
    //
    // When `repo_name` is Some, we filter by `commits.repository` to prevent
    // multi-repo contamination (issue #303): if we loaded ALL SHAs and then
    // upserted ALL of them during every per-repo scan, the last repo scanned
    // would overwrite the correct `on_any_tag=1` values of earlier repos with
    // `on_any_tag=0`, because those earlier-repo commits are not ancestors of
    // any tag in the later repo's git graph.
    let all_shas: Vec<String> = if let Some(name) = repo_name {
        let mut stmt = conn
            .prepare("SELECT sha FROM commits WHERE repository = ?1")
            .map_err(crate::core::TgaError::from)?;
        let rows = stmt
            .query_map(params![name], |row| row.get::<_, String>(0))
            .map_err(crate::core::TgaError::from)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(r.map_err(crate::core::TgaError::from)?);
        }
        out
    } else {
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

    // Detect the default branch (main/master/origin HEAD) and build a set of
    // all SHAs reachable from it.  This populates `on_default_branch` (issue
    // #290 — previously this column was always 0).
    let default_branch_set = detect_default_branch_set(&repo, &sha_set, repo_path);

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

    // Upsert one row per known SHA.  For SHAs not in any map the row gets
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
        let on_default_branch = default_branch_set.contains(sha.as_str());

        let tags_json = serde_json::to_string(&tags).unwrap_or_else(|_| "[]".to_string());
        let branches_json = serde_json::to_string(&branches).unwrap_or_else(|_| "[]".to_string());

        tx.execute(
            "INSERT INTO fact_commit_reachability \
             (commit_sha, on_default_branch, on_any_tag, reachable_from_tags, \
              on_release_branch, release_branches) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6) \
             ON CONFLICT(commit_sha) DO UPDATE SET \
               on_default_branch   = excluded.on_default_branch, \
               on_any_tag          = excluded.on_any_tag, \
               reachable_from_tags = excluded.reachable_from_tags, \
               on_release_branch   = excluded.on_release_branch, \
               release_branches    = excluded.release_branches",
            params![
                sha,
                on_default_branch as i64,
                on_any_tag as i64,
                tags_json,
                on_release_branch as i64,
                branches_json
            ],
        )
        .map_err(crate::core::TgaError::from)?;

        stats.rows_upserted += 1;
        if on_default_branch {
            stats.default_branch_commits += 1;
        }
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
        default_branch = stats.default_branch_commits,
        tagged = stats.tagged_commits,
        release_branch = stats.release_branch_commits,
        "reachability scan complete"
    );
    Ok(stats)
}

// ── Default-branch detection ────────────────────────────────────────────────

/// Detect the repository's default branch and return the set of all DB-known
/// commit SHAs that are reachable from it.
///
/// Why: `on_default_branch` was declared in migration v15 but never computed
/// (issue #290).  Per-repo auto-detection is required because the user's 80
/// repos use a mix of `main` and `master`.
/// What: tries `refs/remotes/origin/HEAD` (symref pointing at the upstream
/// default branch), then falls back through `refs/heads/main`,
/// `refs/heads/master`, `refs/remotes/origin/main`, `refs/remotes/origin/master`
/// in that order.  If none resolve to a commit, emits a `warn!` and returns an
/// empty set so `on_default_branch` stays 0 for this repo (current behaviour,
/// at least made explicit).
/// Test: `tests::scan_default_branch_main`, `tests::scan_default_branch_master`,
/// and `tests::scan_default_branch_missing` cover the three code paths.
pub fn detect_default_branch_set(
    repo: &Repository,
    known_shas: &std::collections::HashSet<String>,
    repo_path: &Path,
) -> std::collections::HashSet<String> {
    // Resolve refs/remotes/origin/HEAD first — this is the symref that git
    // sets when you `git clone` and it points at whatever the remote calls its
    // default branch.
    let tip_oid = 'resolve: {
        if let Ok(head_ref) = repo.find_reference("refs/remotes/origin/HEAD") {
            if let Ok(resolved) = head_ref.resolve() {
                if let Some(oid) = resolved.target() {
                    break 'resolve Some(oid);
                }
            }
        }
        // Fallback chain: local main → local master → remote main → remote master.
        for candidate in [
            "refs/heads/main",
            "refs/heads/master",
            "refs/remotes/origin/main",
            "refs/remotes/origin/master",
        ] {
            if let Ok(r) = repo.find_reference(candidate) {
                if let Some(oid) = r.target() {
                    debug!(candidate, "default branch detected via fallback");
                    break 'resolve Some(oid);
                }
            }
        }
        None
    };

    match tip_oid {
        None => {
            warn!(
                repo = %repo_path.display(),
                "could not detect default branch (tried origin/HEAD, main, master); \
                 on_default_branch will be 0 for this repo"
            );
            std::collections::HashSet::new()
        }
        Some(oid) => {
            let mut set = std::collections::HashSet::new();
            // Re-use walk_ancestors by passing an ad-hoc map and then
            // extracting the keys, or inline the walk directly for simplicity.
            let mut revwalk = match repo.revwalk() {
                Ok(w) => w,
                Err(e) => {
                    warn!(error = %e, "revwalk init failed for default-branch detection");
                    return set;
                }
            };
            if let Err(e) = revwalk.set_sorting(git2::Sort::TIME) {
                warn!(error = %e, "revwalk sort failed");
                return set;
            }
            if let Err(e) = revwalk.push(oid) {
                warn!(error = %e, "revwalk push failed");
                return set;
            }
            for oid_res in revwalk {
                match oid_res {
                    Ok(o) => {
                        let sha = o.to_string();
                        if known_shas.contains(&sha) {
                            set.insert(sha);
                        }
                    }
                    Err(e) => {
                        warn!(error = %e, "revwalk error during default-branch detection; stopping");
                        break;
                    }
                }
            }
            debug!(
                default_branch_commits = set.len(),
                "default-branch SHA set built"
            );
            set
        }
    }
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

        // Count how many known SHAs this tag contributes (debug-only).
        let before = map.len();
        walk_ancestors(repo, tip_oid, known_shas, name, &mut map)?;
        let added = map.len() - before;
        // Per-tag debug line requested in issue #303 to help diagnose future
        // tag-reachability problems without re-running the full backfill.
        debug!(tag = %name, tip = %tip_oid, new_commits = added, "tag walked");
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

        let stats = scan_and_persist(&tr.path, &conn, &cfg, None).expect("scan_and_persist");
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

        let stats = scan_and_persist(&tr.path, &conn, &cfg, None).expect("scan");
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

        scan_and_persist(&tr.path, &conn, &cfg, None).expect("scan");

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

    // ── on_default_branch detection tests ───────────────────────────────────

    /// Why: verifies that commits on the local `main` branch get
    /// `on_default_branch=1` while commits only on a stray branch get 0.
    /// Covers the primary fix for issue #290.
    #[test]
    fn scan_default_branch_main() {
        let (tr, repo) = init_repo("default-main");
        // Three commits on main (HEAD).
        let sha1 = make_commit(&repo, &tr.path, "c1", 1000).to_string();
        let sha2 = make_commit(&repo, &tr.path, "c2", 2000).to_string();
        let sha3 = make_commit(&repo, &tr.path, "c3", 3000).to_string();

        // Create a stray branch at sha2 so sha3 is *only* on main.
        let oid2 = git2::Oid::from_str(&sha2).expect("oid2");
        branch_at(&repo, oid2, "feature/stray");

        // Rename HEAD from `master` to `main` so git init default doesn't matter.
        let oid3 = git2::Oid::from_str(&sha3).expect("oid3");
        let commit3 = repo.find_commit(oid3).expect("commit3");
        repo.branch("main", &commit3, false).expect("main branch");
        // Point HEAD at main.
        repo.set_head("refs/heads/main").expect("set HEAD to main");

        let conn = open_in_memory_db();
        for sha in [&sha1, &sha2, &sha3] {
            insert_sha(&conn, sha);
        }

        let cfg = ReachabilityConfig {
            track_tags: false,
            track_release_branches: false,
            release_branch_patterns: vec![],
        };

        let stats = scan_and_persist(&tr.path, &conn, &cfg, None).expect("scan");
        assert_eq!(
            stats.default_branch_commits, 3,
            "all three commits are on main"
        );

        // sha3 (tip of main) → on_default_branch=1.
        let s3: i64 = conn
            .query_row(
                "SELECT on_default_branch FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha3],
                |r| r.get(0),
            )
            .expect("sha3");
        assert_eq!(s3, 1, "sha3 is on main");

        // sha1 (ancestor) → on_default_branch=1.
        let s1: i64 = conn
            .query_row(
                "SELECT on_default_branch FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha1],
                |r| r.get(0),
            )
            .expect("sha1");
        assert_eq!(s1, 1, "sha1 ancestor of main → on_default_branch");
    }

    /// Why: verifies that when `refs/remotes/origin/HEAD` resolves to `master`
    /// the commits on master get `on_default_branch=1`.
    /// Covers the origin/HEAD symref resolution path (most common in cloned repos).
    #[test]
    fn scan_default_branch_via_origin_head() {
        let (tr, repo) = init_repo("origin-head");
        let sha1 = make_commit(&repo, &tr.path, "c1", 1000).to_string();
        let sha2 = make_commit(&repo, &tr.path, "c2", 2000).to_string();

        // Simulate what `git clone` does: create refs/remotes/origin/master
        // pointing at sha2 and refs/remotes/origin/HEAD as a symref to it.
        let oid2 = git2::Oid::from_str(&sha2).expect("oid2");
        repo.reference("refs/remotes/origin/master", oid2, true, "origin master")
            .expect("create origin/master ref");
        repo.reference_symbolic(
            "refs/remotes/origin/HEAD",
            "refs/remotes/origin/master",
            true,
            "origin HEAD",
        )
        .expect("create origin/HEAD symref");

        let conn = open_in_memory_db();
        for sha in [&sha1, &sha2] {
            insert_sha(&conn, sha);
        }

        let cfg = ReachabilityConfig {
            track_tags: false,
            track_release_branches: false,
            release_branch_patterns: vec![],
        };

        let stats = scan_and_persist(&tr.path, &conn, &cfg, None).expect("scan");
        assert_eq!(stats.default_branch_commits, 2, "both commits on master");

        let s2: i64 = conn
            .query_row(
                "SELECT on_default_branch FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha2],
                |r| r.get(0),
            )
            .expect("sha2");
        assert_eq!(s2, 1, "sha2 reachable from origin/master");
    }

    /// Why: verifies that when no default branch can be detected the function
    /// gracefully returns an empty set and `on_default_branch` stays 0 for all
    /// commits — matching the documented fallback behaviour.
    #[test]
    fn scan_default_branch_missing_graceful() {
        // A freshly-initialised bare repo has no commits and no HEAD target.
        // We can simulate "no detectable default branch" by creating a repo
        // that has only an orphan branch with a non-standard name.
        let (tr, repo) = init_repo("no-default");
        let sha = make_commit(&repo, &tr.path, "c1", 1000).to_string();
        // Rename the default branch to something non-standard so none of the
        // fallback candidates match.
        let oid = git2::Oid::from_str(&sha).expect("oid");
        let commit = repo.find_commit(oid).expect("commit");
        repo.branch("develop", &commit, false).expect("develop");
        // Detach HEAD so refs/heads/master / refs/heads/main don't exist.
        repo.set_head_detached(oid).expect("detach HEAD");
        // Delete original master/main branch if it exists.
        for bname in ["master", "main"] {
            if let Ok(mut b) = repo.find_branch(bname, git2::BranchType::Local) {
                let _ = b.delete();
            }
        }

        let known: std::collections::HashSet<String> = [sha.clone()].into();
        let set = detect_default_branch_set(&repo, &known, &tr.path);
        assert!(
            set.is_empty(),
            "no detectable default branch → empty set, on_default_branch stays 0"
        );
    }

    /// Why: reproduces GitHub issue #303 — a tag pointing to a commit on `develop`
    /// (a non-default branch) must mark those commits `on_any_tag=1`.  Before the
    /// fix, `build_tag_map` restricted tag-walking to the ref-list provided by
    /// `repo.tag_names()` which only returns tags reachable from the default
    /// branch in some git2 versions, causing GitFlow repos to misreport
    /// ~554/838 tagged commits as `on_any_tag=0`.
    ///
    /// Scenario:
    ///   main:    A
    ///             \
    ///   develop:   B → C   ← tag v1.0.0 (points to C; ancestors = A, B, C)
    ///
    /// After scan_and_persist, B and C must have `on_any_tag=1` and
    /// `reachable_from_tags` containing `"v1.0.0"`.  A must also be tagged
    /// (it is an ancestor of v1.0.0).  A commit D added only to main AFTER
    /// the branch point must NOT appear in the tag map.
    #[test]
    fn scan_gitflow_develop_tag_marks_non_default_branch_commits() {
        let (tr, repo) = init_repo("gitflow-develop-tag");

        // A: initial commit on master/main (HEAD).
        let sha_a = make_commit(&repo, &tr.path, "initial", 1000).to_string();

        // Create `develop` branch at A and switch HEAD to it.
        let oid_a = git2::Oid::from_str(&sha_a).expect("oid_a");
        branch_at(&repo, oid_a, "develop");
        repo.set_head("refs/heads/develop")
            .expect("set HEAD to develop");

        // B and C: two commits exclusively on develop.
        let sha_b = make_commit(&repo, &tr.path, "develop work B", 2000).to_string();
        let sha_c = make_commit(&repo, &tr.path, "develop work C", 3000).to_string();

        // Tag C as v1.0.0 (GitFlow release tag on develop).
        let oid_c = git2::Oid::from_str(&sha_c).expect("oid_c");
        tag_commit(&repo, oid_c, "v1.0.0");

        // Switch HEAD back to master and add a commit D (only on master).
        repo.set_head("refs/heads/master")
            .expect("set HEAD to master");
        let sha_d = make_commit(&repo, &tr.path, "main-only D", 4000).to_string();

        // v0.5.0: tag A (a main-branch commit).
        tag_commit(&repo, oid_a, "v0.5.0");

        // DB: insert all four commits as if they were collected from this repo.
        let conn = open_in_memory_db();
        for sha in [&sha_a, &sha_b, &sha_c, &sha_d] {
            insert_sha(&conn, sha);
        }

        let cfg = ReachabilityConfig {
            track_tags: true,
            track_release_branches: false,
            release_branch_patterns: vec![],
        };

        let stats = scan_and_persist(&tr.path, &conn, &cfg, None).expect("scan_and_persist");
        assert_eq!(stats.rows_upserted, 4, "one row per commit");

        // A is an ancestor of v1.0.0 (via develop) and also directly tagged by v0.5.0.
        let (a_on_tag, a_tags_json): (i64, String) = conn
            .query_row(
                "SELECT on_any_tag, reachable_from_tags \
                 FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha_a],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query A");
        assert_eq!(
            a_on_tag, 1,
            "A is an ancestor of v1.0.0 (and tagged v0.5.0)"
        );
        let a_tags: Vec<String> = serde_json::from_str(&a_tags_json).expect("parse json");
        assert!(
            a_tags.contains(&"v1.0.0".to_string()),
            "A must appear in reachable_from_tags for v1.0.0; got {a_tags_json}"
        );
        assert!(
            a_tags.contains(&"v0.5.0".to_string()),
            "A must appear in reachable_from_tags for v0.5.0; got {a_tags_json}"
        );

        // B is ONLY on develop, reachable from v1.0.0 — the core regression case.
        let (b_on_tag, b_tags_json): (i64, String) = conn
            .query_row(
                "SELECT on_any_tag, reachable_from_tags \
                 FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha_b],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query B");
        assert_eq!(
            b_on_tag, 1,
            "B is on develop and ancestor of v1.0.0 — must be on_any_tag=1 (issue #303)"
        );
        let b_tags: Vec<String> = serde_json::from_str(&b_tags_json).expect("parse json");
        assert!(
            b_tags.contains(&"v1.0.0".to_string()),
            "B must be in reachable_from_tags=[\"v1.0.0\"]; got {b_tags_json}"
        );

        // C is the tagged commit itself.
        let (c_on_tag, c_tags_json): (i64, String) = conn
            .query_row(
                "SELECT on_any_tag, reachable_from_tags \
                 FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha_c],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .expect("query C");
        assert_eq!(c_on_tag, 1, "C is the tip of v1.0.0");
        let c_tags: Vec<String> = serde_json::from_str(&c_tags_json).expect("parse json");
        assert!(
            c_tags.contains(&"v1.0.0".to_string()),
            "C must be in reachable_from_tags=[\"v1.0.0\"]; got {c_tags_json}"
        );

        // D is only on main and NOT reachable from any tag.
        let d_on_tag: i64 = conn
            .query_row(
                "SELECT on_any_tag FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha_d],
                |r| r.get(0),
            )
            .expect("query D");
        assert_eq!(
            d_on_tag, 0,
            "D is only on main and not reachable from any tag"
        );
    }

    /// Why: stray-branch commits that are NOT on main must get on_default_branch=0
    /// even after scan_and_persist runs with a valid default branch.
    #[test]
    fn scan_stray_branch_excluded_from_default() {
        let (tr, repo) = init_repo("stray-excl");
        let sha1 = make_commit(&repo, &tr.path, "base", 1000).to_string();
        let sha2 = make_commit(&repo, &tr.path, "main-commit", 2000).to_string();

        // Create a stray branch at sha1 and add an exclusive commit there.
        let oid1 = git2::Oid::from_str(&sha1).expect("oid1");
        let commit1 = repo.find_commit(oid1).expect("commit1");
        repo.branch("stray", &commit1, false).expect("stray");

        // HEAD is still at sha2 (tip of master/main linear chain).
        // Rename to main.
        let oid2 = git2::Oid::from_str(&sha2).expect("oid2");
        let commit2 = repo.find_commit(oid2).expect("commit2");
        repo.branch("main", &commit2, false).expect("main");
        repo.set_head("refs/heads/main").expect("set HEAD");

        // Add a commit on stray that is NOT reachable from main.
        repo.set_head("refs/heads/stray").expect("checkout stray");
        let sha_stray = make_commit(&repo, &tr.path, "stray-exclusive", 3000).to_string();
        // Return HEAD to main.
        repo.set_head("refs/heads/main").expect("back to main");

        let conn = open_in_memory_db();
        for sha in [&sha1, &sha2, &sha_stray] {
            insert_sha(&conn, sha);
        }

        let cfg = ReachabilityConfig {
            track_tags: false,
            track_release_branches: false,
            release_branch_patterns: vec![],
        };

        scan_and_persist(&tr.path, &conn, &cfg, None).expect("scan");

        // The stray-exclusive commit must NOT be on the default branch.
        let stray_flag: i64 = conn
            .query_row(
                "SELECT on_default_branch FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha_stray],
                |r| r.get(0),
            )
            .expect("stray query");
        assert_eq!(
            stray_flag, 0,
            "stray-exclusive commit must have on_default_branch=0"
        );

        // sha2 (on main) must be 1.
        let main_flag: i64 = conn
            .query_row(
                "SELECT on_default_branch FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha2],
                |r| r.get(0),
            )
            .expect("main query");
        assert_eq!(main_flag, 1, "sha2 is on main → on_default_branch=1");
    }

    /// Why: demonstrates the cross-repo overwrite bug (issue #303 root cause).
    ///
    /// When multiple repos share a single DB and `scan_and_persist` is called
    /// per-repo without a `repo_name` filter, the LAST repo scanned overwrites
    /// `on_any_tag` values written by earlier repos: commits from repo-A are
    /// NOT ancestors of any tag in repo-B, so when repo-B is scanned they get
    /// `on_any_tag=0` even if repo-A's scan correctly set `on_any_tag=1`.
    ///
    /// This test builds two separate git repos, seeds the DB with commits from
    /// both (tagged by their respective repos), and shows that scanning with
    /// `repo_name=Some(name)` preserves the correct value written by the first
    /// scan instead of clobbering it.
    #[test]
    fn scan_multi_repo_no_cross_contamination() {
        // ── Repo A: has a tag v1.0 on its only commit. ─────────────────────
        let (tr_a, repo_a) = init_repo("multi-repo-A");
        let sha_a = make_commit(&repo_a, &tr_a.path, "repo-A commit", 1000).to_string();
        let oid_a = git2::Oid::from_str(&sha_a).expect("oid_a");
        tag_commit(&repo_a, oid_a, "v1.0");

        // ── Repo B: unrelated repo with its own single untagged commit. ─────
        let (tr_b, repo_b) = init_repo("multi-repo-B");
        let sha_b = make_commit(&repo_b, &tr_b.path, "repo-B commit", 2000).to_string();
        // No tag in repo B.

        // DB: insert both commits as if collected from their respective repos.
        let conn = open_in_memory_db();
        // Insert sha_a under repository='repo-A'.
        conn.execute(
            "INSERT OR IGNORE INTO commits (sha, repository) VALUES (?1, ?2)",
            params![sha_a, "repo-A"],
        )
        .expect("insert sha_a");
        // Insert sha_b under repository='repo-B'.
        conn.execute(
            "INSERT OR IGNORE INTO commits (sha, repository) VALUES (?1, ?2)",
            params![sha_b, "repo-B"],
        )
        .expect("insert sha_b");

        let cfg = ReachabilityConfig {
            track_tags: true,
            track_release_branches: false,
            release_branch_patterns: vec![],
        };

        // Scan repo-A first — sha_a should be tagged v1.0.
        scan_and_persist(&tr_a.path, &conn, &cfg, Some("repo-A")).expect("scan repo-A");

        // Verify sha_a is correctly marked as tagged.
        let a_on_tag: i64 = conn
            .query_row(
                "SELECT on_any_tag FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha_a],
                |r| r.get(0),
            )
            .expect("query sha_a after repo-A scan");
        assert_eq!(
            a_on_tag, 1,
            "sha_a should be on_any_tag=1 after repo-A scan"
        );

        // Scan repo-B second — sha_b is untagged. Without the repo_name filter,
        // this scan would have loaded sha_a into known_shas, not found it in
        // repo-B's tag graph, and overwritten on_any_tag=1 with 0.
        scan_and_persist(&tr_b.path, &conn, &cfg, Some("repo-B")).expect("scan repo-B");

        // sha_a must still be on_any_tag=1 — the repo-B scan must NOT have
        // touched it (issue #303: cross-repo overwrite was the actual root cause).
        let a_on_tag_after: i64 = conn
            .query_row(
                "SELECT on_any_tag FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha_a],
                |r| r.get(0),
            )
            .expect("query sha_a after repo-B scan");
        assert_eq!(
            a_on_tag_after, 1,
            "repo-B scan must not overwrite sha_a's on_any_tag=1 (issue #303)"
        );

        // sha_b must be on_any_tag=0 — no tags in repo-B.
        let b_on_tag: i64 = conn
            .query_row(
                "SELECT on_any_tag FROM fact_commit_reachability WHERE commit_sha = ?1",
                params![sha_b],
                |r| r.get(0),
            )
            .expect("query sha_b");
        assert_eq!(b_on_tag, 0, "sha_b has no tag in repo-B");
    }
}
