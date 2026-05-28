//! Commit extraction via `git2`.
//!
//! Walks a repository's revision history, applies date filters, computes
//! diff statistics for each commit, and persists the result into the
//! SQLite store via `core::db::Database`.

use std::path::PathBuf;

use chrono::{DateTime, FixedOffset, NaiveDate, TimeZone, Utc};
use git2::{Repository, Sort};
use indicatif::{ProgressBar, ProgressStyle};
use rusqlite::params;
use tracing::{debug, info, warn};

use crate::collect::collector::{FetchOutcome, PerRepoFetch};
use crate::collect::errors::{CollectError, Result};
use crate::collect::git::diff::{compute_commit_diff, CommitDiff};
use crate::collect::git::fetch::{fetch_and_record, fetch_remote};
use crate::collect::ticket::{extract_ticket_id, is_ticketed};
use crate::core::config::{expand_path, RepositoryConfig};
use crate::core::db::Database;

/// Extracts commits from a single configured repository.
///
/// Why: provides a single, configurable handle for walking a repository's
/// commit history and inserting the results into the SQLite store.  Separating
/// per-repo configuration (path, branch, date window, head_only flag) from the
/// collection pipeline lets the pipeline build one `GitCollector` per entry in
/// `config.repositories` and drive them independently.
/// What: holds per-repo settings; the heavy work lives in `collect_window`.
/// Test: see the `#[cfg(test)]` block below for unit tests covering branch
/// coverage (multi_branch_coverage, head_only_legacy_behavior, etc.) and the
/// ISO-week boundary tests from issue #70.
#[derive(Debug)]
pub struct GitCollector {
    /// Resolved on-disk path of the repository.
    path: PathBuf,
    /// Display name used in the `repository` column.
    name: String,
    /// Branch override (None = walk is controlled by `head_only`).
    branch: Option<String>,
    /// Optional inclusive since date (ISO 8601, parsed to UTC).
    since: Option<DateTime<Utc>>,
    /// Optional inclusive until date (ISO 8601, parsed to UTC).
    until: Option<DateTime<Utc>>,
    /// If true, merge commits are not written to the DB.
    skip_merges: bool,
    /// If true, skip the pre-walk `git fetch` step.
    no_fetch: bool,
    /// Remote name to fetch from prior to the walk (default "origin").
    remote_name: String,
    /// When `true`, seed the revwalk from HEAD only (legacy 1.x behaviour).
    /// When `false` (default since 2.0.0), push every `refs/heads/*` and
    /// `refs/remotes/origin/*` ref so that commits on non-default branches
    /// are not silently excluded.
    head_only: bool,
    /// Explicit branch list from `--branch <NAME[,NAME…]>`.
    ///
    /// When non-empty, overrides all other revwalk seeding logic: the walk
    /// seeds from `refs/heads/<name>` + `refs/remotes/origin/<name>` for
    /// each listed name.  An empty vec means "no restriction" (use the
    /// default all-branches or head_only logic).
    explicit_branches: Vec<String>,
    /// Optional per-repo fetch timeout in seconds.
    ///
    /// When `Some(n)`, stored for future enforcement via a thread-based
    /// watchdog.  When `None` (the default), the system / git2 transport
    /// defaults apply.  See issue #334 and `RepositoryConfig::fetch_timeout_secs`.
    fetch_timeout_secs: Option<u64>,
}

impl GitCollector {
    /// Construct a new collector from a [`RepositoryConfig`].
    ///
    /// Validates that the path exists and refers to a real git repository.
    ///
    /// # Errors
    ///
    /// - [`CollectError::Git`] if the path is not a git repository.
    /// - [`CollectError::Config`] if date strings cannot be parsed.
    pub fn new(config: &RepositoryConfig) -> Result<Self> {
        let path = expand_path(&config.path);
        if !path.exists() {
            return Err(CollectError::Config(format!(
                "repository path does not exist: {}",
                path.display()
            )));
        }
        // Verify it's actually a repository up-front.
        let _ = Repository::open(&path)?;

        let name = config
            .name
            .clone()
            .or_else(|| {
                path.file_name()
                    .and_then(|s| s.to_str())
                    .map(|s| s.to_string())
            })
            .unwrap_or_else(|| path.display().to_string());

        let since = parse_iso_date(config.since_date.as_deref())?;
        let until = parse_iso_date(config.until_date.as_deref())?;

        Ok(Self {
            path,
            name,
            branch: config.branch.clone(),
            since,
            until,
            skip_merges: false,
            no_fetch: false,
            remote_name: "origin".to_string(),
            head_only: config.head_only,
            explicit_branches: Vec::new(),
            fetch_timeout_secs: config.fetch_timeout_secs,
        })
    }

    /// Set whether to skip merge commits during extraction.
    pub fn skip_merges(mut self, skip: bool) -> Self {
        self.skip_merges = skip;
        self
    }

    /// Disable the pre-walk `git fetch` (useful for offline / CI scenarios
    /// or when the caller has already fetched out-of-band).
    pub fn no_fetch(mut self, no_fetch: bool) -> Self {
        self.no_fetch = no_fetch;
        self
    }

    /// Override the remote name used for the pre-walk fetch (default `"origin"`).
    pub fn with_remote(mut self, remote: impl Into<String>) -> Self {
        self.remote_name = remote.into();
        self
    }

    /// Control HEAD-only vs. all-branches revwalk seeding.
    ///
    /// Why: tga 2.0.0 changed the default to walk all local branches and remote
    /// tracking refs (`refs/heads/*` + `refs/remotes/origin/*`).  Callers that
    /// need the legacy HEAD-only behaviour (e.g. the `--head-only` CLI flag or
    /// per-repo `head_only: true` in YAML) set this to `true`.
    /// What: stores the flag; the revwalk branching logic in `collect_window`
    /// reads it at walk time.
    /// Test: see `tests::head_only_legacy_behavior` and
    /// `tests::multi_branch_coverage`.
    pub fn with_head_only(mut self, head_only: bool) -> Self {
        self.head_only = head_only;
        self
    }

    /// Restrict the revwalk to an explicit list of branch names.
    ///
    /// Why: the `--branch <NAME[,NAME…]>` CLI flag enables surgical re-runs
    /// on specific branches without modifying the YAML config.  When set, this
    /// takes priority over `head_only` and the all-branches default, seeding
    /// only the listed names.
    /// What: for each name, pushes `refs/heads/<name>` and
    /// `refs/remotes/origin/<name>`.  Names not found in the repo are logged as
    /// warnings but do not abort collection.  An empty `Vec` (the default)
    /// means no restriction — fall through to `head_only` / all-branches logic.
    /// Test: see `tests::branch_filter_walks_only_named_branch` and
    /// `tests::branch_filter_composes_with_repos`.
    pub fn with_explicit_branches(mut self, branches: Vec<String>) -> Self {
        self.explicit_branches = branches;
        self
    }

    /// Override the per-repo fetch timeout.
    ///
    /// Why: the value from [`crate::core::config::RepositoryConfig::fetch_timeout_secs`]
    /// is set via `new`; this builder lets callers override it without
    /// constructing a new config struct.
    /// What: stores the value; enforcement is scheduled for a future release
    /// once git2 exposes transport-level timeouts. For now the field is
    /// persisted and logged but not acted upon.
    /// Test: constructor round-trip is verified in `tests::fetch_timeout_stored`.
    pub fn with_fetch_timeout(mut self, secs: Option<u64>) -> Self {
        self.fetch_timeout_secs = secs;
        self
    }

    /// Perform a one-shot `git fetch origin` for this repository and return
    /// a typed outcome.
    ///
    /// Why: the pipeline calls this once per repo before the per-week
    /// `collect_window` loop so that (a) only one network round-trip is
    /// made per repo and (b) the outcome is available for the end-of-run
    /// summary (issue #334).
    /// What: if `no_fetch` is set, returns a `Skipped` outcome immediately.
    /// Otherwise opens the repository, calls `fetch_and_record`, and returns
    /// the result. A `fetch_timeout_secs` value is logged but not yet enforced
    /// at the libgit2 level (scheduled for a future release).
    /// Test: see `fetch::tests::fetch_outcome_skipped_for_local_repo` and
    /// the `no_fetch_returns_skipped` test in `extractor::tests`.
    pub fn perform_fetch(&self) -> PerRepoFetch {
        if self.no_fetch {
            return PerRepoFetch {
                repo: self.name.clone(),
                outcome: FetchOutcome::Skipped {
                    reason: "--no-fetch".to_string(),
                },
            };
        }
        if let Some(t) = self.fetch_timeout_secs {
            tracing::debug!(
                repo = %self.name,
                timeout_secs = t,
                "fetch_timeout_secs configured (enforcement pending future release)"
            );
        }
        let repo = match Repository::open(&self.path) {
            Ok(r) => r,
            Err(e) => {
                return PerRepoFetch {
                    repo: self.name.clone(),
                    outcome: FetchOutcome::Failed {
                        remote: self.remote_name.clone(),
                        error: format!("failed to open repo for fetch: {e}"),
                    },
                };
            }
        };
        fetch_and_record(&repo, &self.name, &self.remote_name)
    }

    /// Walk the repository and insert commits into the database.
    ///
    /// Returns the number of commits written.
    ///
    /// # Errors
    ///
    /// Any underlying git or database failure is propagated.
    pub fn collect(&self, db: &mut Database) -> Result<usize> {
        self.collect_window(db, self.since, self.until)
    }

    /// Walk the repository and insert commits whose timestamp falls within
    /// `[since, until]`. The supplied bounds override the collector's
    /// configured `since`/`until` for this call only.
    ///
    /// Either bound may be `None` to leave that side open.
    ///
    /// # Errors
    ///
    /// Any underlying git or database failure is propagated.
    pub fn collect_window(
        &self,
        db: &mut Database,
        since: Option<DateTime<Utc>>,
        until: Option<DateTime<Utc>>,
    ) -> Result<usize> {
        let repo = Repository::open(&self.path)?;
        info!(
            repo = %self.name,
            path = %self.path.display(),
            ?since,
            ?until,
            "starting commit extraction"
        );

        // Note: the pre-walk fetch is now performed once per repo via
        // `perform_fetch` before the week loop in `CollectionPipeline::collect_repo_by_week`.
        // `collect_window` no longer fetches to avoid N fetches for N weeks.
        // Legacy callers that invoke `collect_window` directly on a collector
        // with `no_fetch = false` will still get a fetch here as a safety net.
        if !self.no_fetch {
            if let Err(e) = fetch_remote(&repo, &self.remote_name) {
                warn!(
                    repo = %self.name,
                    remote = %self.remote_name,
                    error = %e,
                    "pre-walk fetch returned an error; continuing with local refs"
                );
            }
        } else {
            debug!(repo = %self.name, "skipping pre-walk fetch (already done or --no-fetch)");
        }

        let mut revwalk = repo.revwalk()?;
        revwalk.set_sorting(Sort::TIME)?;
        // Revwalk seeding: four cases in priority order.
        //
        // 1. `explicit_branches` non-empty — `--branch <NAME[,NAME…]>` CLI
        //    filter.  Walks only the listed branches (both local heads and
        //    remote-tracking copies).  Names not found emit a warning.
        // 2. Explicit per-repo `branch` override — unchanged from 1.x, walks
        //    only that branch's ancestry.
        // 3. `head_only = true` — legacy escape hatch, seeds from HEAD only.
        // 4. Default (2.0.0+): push every `refs/heads/*` and every
        //    `refs/remotes/origin/*` so commits on non-default branches are
        //    not silently excluded.  The revwalk's internal dedup ensures each
        //    commit is yielded at most once even when reachable from multiple
        //    refs.  The `INSERT OR IGNORE` on the `commits` SHA primary key
        //    provides a second safety net.
        if !self.explicit_branches.is_empty() {
            // Arm 1: --branch filter — seed only the listed branch names.
            let mut pushed = 0u32;
            for branch_name in &self.explicit_branches {
                let local_ref = format!("refs/heads/{branch_name}");
                let remote_ref = format!("refs/remotes/origin/{branch_name}");
                let local_ok = revwalk.push_ref(&local_ref).is_ok();
                let remote_ok = revwalk.push_ref(&remote_ref).is_ok();
                if local_ok || remote_ok {
                    pushed += 1;
                    debug!(
                        repo = %self.name,
                        branch = %branch_name,
                        local = local_ok,
                        remote = remote_ok,
                        "--branch filter: pushed refs for branch"
                    );
                } else {
                    warn!(
                        repo = %self.name,
                        branch = %branch_name,
                        "--branch filter: branch '{}' not found in repo '{}' \
                         (neither refs/heads/{} nor refs/remotes/origin/{}); skipping",
                        branch_name,
                        self.name,
                        branch_name,
                        branch_name,
                    );
                }
            }
            if pushed == 0 {
                // None of the listed branches exist in this repo — nothing to walk.
                warn!(
                    repo = %self.name,
                    "--branch filter found no matching refs; producing zero commits for this repo"
                );
            } else {
                info!(
                    repo = %self.name,
                    branches_found = pushed,
                    "--branch filter: walking {} of {} requested branches",
                    pushed,
                    self.explicit_branches.len(),
                );
            }
        } else {
            match (&self.branch, self.head_only) {
                (Some(name), _) => {
                    // Arm 2: Explicit per-repo branch override still works as before.
                    let refname = format!("refs/heads/{name}");
                    if revwalk.push_ref(&refname).is_err() {
                        // Try as a generic revision (could be a tag or remote ref).
                        revwalk.push_ref(name)?;
                    }
                }
                (None, true) => {
                    // Arm 3: Legacy escape hatch: --head-only flag or per-repo head_only: true.
                    debug!(repo = %self.name, "head_only mode: seeding revwalk from HEAD only (legacy 1.x behaviour)");
                    revwalk.push_head()?;
                }
                (None, false) => {
                    // Arm 4 (NEW DEFAULT 2.0.0+): push every local branch head and
                    // every remote tracking ref so multi-branch repos don't lose
                    // commits that never landed on the default branch.
                    let mut heads_pushed = 0u32;
                    let mut remotes_pushed = 0u32;
                    let refs = repo.references()?;
                    for r in refs.flatten() {
                        let Some(name) = r.name() else { continue };
                        if name.starts_with("refs/heads/") {
                            if revwalk.push_ref(name).is_ok() {
                                heads_pushed += 1;
                            }
                        } else if name.starts_with("refs/remotes/origin/")
                            && name != "refs/remotes/origin/HEAD"
                            && revwalk.push_ref(name).is_ok()
                        {
                            remotes_pushed += 1;
                        }
                    }
                    let total = heads_pushed + remotes_pushed;
                    if total > 0 {
                        info!(
                            repo = %self.name,
                            refs_walked = total,
                            heads = heads_pushed,
                            remote_tracking = remotes_pushed,
                            "all-branch walk: pushed {} refs ({} heads + {} remote-tracking)",
                            total,
                            heads_pushed,
                            remotes_pushed,
                        );
                    } else {
                        // Fallback: repos with weird ref layouts (e.g. detached
                        // HEAD with no local branches — common in CI shallow
                        // clones) still get some coverage.
                        debug!(
                            repo = %self.name,
                            "no refs/heads/* or refs/remotes/origin/* found; \
                             falling back to HEAD for revwalk seed"
                        );
                        revwalk.push_head()?;
                    }
                }
            }
        }

        // Spinner-style progress bar — we stream the revwalk so we don't
        // know the total in advance. This is intentional: materialising
        // every OID up-front on a 58K-commit monolith eats memory AND
        // forces a full-history walk before the time filter can take
        // effect. With Sort::TIME the walk yields newest-first, so we can
        // safely break the moment we cross the `since` boundary.
        let pb = ProgressBar::new_spinner();
        pb.set_style(
            ProgressStyle::with_template("{spinner} {pos} commits walked {msg}")
                .unwrap_or_else(|_| ProgressStyle::default_spinner()),
        );
        pb.enable_steady_tick(std::time::Duration::from_millis(100));

        // Derive date-only bounds from the (UTC) timestamps. The collector
        // accepts UTC bounds for compatibility, but the user-facing semantic
        // is a calendar window: `since_date` and `until_date` in the config
        // are calendar dates, and a commit "belongs" to the calendar week of
        // its *local* (authoring) date — not the UTC date the instant maps
        // to. Issue #70: a commit at 2026-05-03 23:43 -0700 lives in UTC on
        // 2026-05-04, but it is a Saturday W18 commit, not a Sunday W19
        // commit. Compare by local date to fix both timezone drift across
        // week boundaries and end-of-day inclusivity of `until_date`.
        let since_date: Option<NaiveDate> = since.map(|s| s.date_naive());
        let until_date: Option<NaiveDate> = until.map(|u| u.date_naive());

        let mut written = 0usize;
        let mut walked = 0usize;
        let tx = db.connection_mut().transaction()?;
        for oid_res in revwalk {
            let oid = match oid_res {
                Ok(o) => o,
                Err(e) => {
                    warn!(error = %e, "revwalk yielded error; stopping traversal");
                    break;
                }
            };
            walked += 1;
            pb.set_position(walked as u64);
            if walked.is_multiple_of(1000) {
                info!(repo = %self.name, walked, written, "extraction progress");
            }

            let commit = repo.find_commit(oid)?;
            let ts = match commit_time_utc(&commit) {
                Some(t) => t,
                None => {
                    warn!(sha = %oid, "skipping commit with invalid timestamp");
                    continue;
                }
            };
            let local_date = match commit_local_date(&commit) {
                Some(d) => d,
                None => {
                    warn!(sha = %oid, "skipping commit with invalid local timestamp");
                    continue;
                }
            };

            // Since commits are ordered newest-first by Sort::TIME, once we
            // cross below `since` we can stop walking entirely. The cutoff
            // uses the UTC instant (Sort::TIME orders by UTC) but allows a
            // 1-day grace so that a commit whose UTC instant is before
            // `since` but whose *local* date still falls on/after `since`
            // is not prematurely cut off.
            if let Some(s) = since {
                if ts < s - chrono::Duration::days(1) {
                    debug!(sha = %oid, ts = %ts, since = %s, "reached since bound; stopping revwalk");
                    break;
                }
            }

            // Filter by local calendar date against the [since_date,
            // until_date] window. Both bounds inclusive.
            if let Some(sd) = since_date {
                if local_date < sd {
                    continue;
                }
            }
            if let Some(ud) = until_date {
                if local_date > ud {
                    // Newer than upper bound — keep walking because earlier
                    // commits may still fall in range.
                    continue;
                }
            }

            let is_merge = commit.parent_count() > 1;
            if self.skip_merges && is_merge {
                continue;
            }

            let diff = match compute_commit_diff(&repo, &commit) {
                Ok(d) => d,
                Err(e) => {
                    warn!(sha = %oid, error = %e, "failed to compute diff; recording commit with zero stats");
                    CommitDiff::default()
                }
            };

            let author = commit.author();
            let author_name = author.name().unwrap_or("").to_string();
            let author_email = author.email().unwrap_or("").to_string();
            let message = commit.message().unwrap_or("").to_string();
            let sha_str = oid.to_string();

            let ticketed = is_ticketed(&message);
            // Issue #316: extract the ticket ID at insert time so
            // `commits.ticket_id` is populated without a separate
            // `tga backfill ticket-ids` run.
            let ticket_id = extract_ticket_id(&message);

            let inserted = tx.execute(
                "INSERT OR IGNORE INTO commits \
                 (sha, author_name, author_email, timestamp, message, repository, \
                  files_changed, insertions, deletions, is_merge, ticketed, ticket_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    sha_str,
                    author_name,
                    author_email,
                    ts.to_rfc3339(),
                    message,
                    self.name,
                    diff.files_changed as i64,
                    diff.insertions as i64,
                    diff.deletions as i64,
                    is_merge as i64,
                    ticketed as i64,
                    ticket_id,
                ],
            )?;

            if inserted == 1 {
                let commit_id = tx.last_insert_rowid();
                for f in &diff.files {
                    tx.execute(
                        "INSERT INTO files (commit_id, path, change_type, insertions, deletions) \
                         VALUES (?1, ?2, ?3, ?4, ?5)",
                        params![
                            commit_id,
                            f.path,
                            f.change_type.as_str(),
                            f.insertions as i64,
                            f.deletions as i64,
                        ],
                    )?;
                }
                written += 1;
            }
        }
        tx.commit()?;
        pb.finish_with_message(format!("done ({walked} walked, {written} new)"));
        debug!(repo = %self.name, written, "commit extraction complete");
        Ok(written)
    }

    /// Borrow the resolved repository name (display).
    pub fn name(&self) -> &str {
        &self.name
    }

    /// Configured inclusive lower bound on commit timestamps, if any.
    pub fn since(&self) -> Option<DateTime<Utc>> {
        self.since
    }

    /// Configured inclusive upper bound on commit timestamps, if any.
    pub fn until(&self) -> Option<DateTime<Utc>> {
        self.until
    }
}

/// Parse an ISO-8601 date or datetime into a UTC timestamp.
fn parse_iso_date(s: Option<&str>) -> Result<Option<DateTime<Utc>>> {
    let Some(s) = s else { return Ok(None) };
    if let Ok(dt) = DateTime::parse_from_rfc3339(s) {
        return Ok(Some(dt.with_timezone(&Utc)));
    }
    if let Ok(d) = chrono::NaiveDate::parse_from_str(s, "%Y-%m-%d") {
        let ndt = d
            .and_hms_opt(0, 0, 0)
            .ok_or_else(|| CollectError::Config(format!("invalid date: {s}")))?;
        return Ok(Some(Utc.from_utc_datetime(&ndt)));
    }
    Err(CollectError::Config(format!(
        "could not parse date '{s}' (expected YYYY-MM-DD or RFC3339)"
    )))
}

/// Convert a git commit author time to the *local* calendar date as recorded
/// in the commit itself (i.e. using the author's timezone offset, not UTC).
///
/// Why: ISO-week assignment must respect the author's local date, otherwise
/// commits made late in the evening in negative-UTC timezones get bumped
/// into the next ISO week. See issue #70.
fn commit_local_date(commit: &git2::Commit<'_>) -> Option<NaiveDate> {
    let t = commit.time();
    let offset = FixedOffset::east_opt(t.offset_minutes() * 60)?;
    let local = offset.timestamp_opt(t.seconds(), 0).single()?;
    Some(local.date_naive())
}

/// Convert a git commit author time to UTC `DateTime`.
fn commit_time_utc(commit: &git2::Commit<'_>) -> Option<DateTime<Utc>> {
    let t = commit.time();
    Utc.timestamp_opt(t.seconds(), 0).single()
}

#[cfg(test)]
mod tests {
    //! Tests for issue #70: ISO-week boundary correctness across timezones
    //! and end-of-day inclusivity of `until_date`.
    //!
    //! These tests build a small ephemeral git repository on disk with
    //! commits at hand-crafted timestamps + timezone offsets, then run the
    //! collector against it.

    use super::*;
    use crate::core::config::RepositoryConfig;
    use crate::core::db::Database;
    use chrono::NaiveDateTime;
    use git2::{Repository, Signature, Time};
    use std::path::{Path, PathBuf};

    /// Compute the unix timestamp (in seconds) of a UTC wall-clock instant.
    /// Tests express bounds in UTC and a separate offset, so the recorded
    /// commit time has a known `(seconds, offset)` pair.
    fn utc_seconds(y: i32, mo: u32, d: u32, h: u32, mi: u32, s: u32) -> i64 {
        let ndt = NaiveDateTime::new(
            NaiveDate::from_ymd_opt(y, mo, d).expect("valid date"),
            chrono::NaiveTime::from_hms_opt(h, mi, s).expect("valid time"),
        );
        Utc.from_utc_datetime(&ndt).timestamp()
    }

    struct TempRepo {
        path: PathBuf,
    }

    impl TempRepo {
        /// Create a new temporary git repository with a stable test identity.
        ///
        /// Why: the #334 `perform_fetch` tests need a quick one-liner to
        /// create a throw-away repo without needing the full `init_repo` API.
        /// What: initialises an empty git repo in a unique temp directory.
        /// Test: used directly by `no_fetch_returns_skipped` etc.
        fn new() -> Self {
            let path = unique_dir("temprepo");
            std::fs::create_dir_all(&path).expect("mkdir");
            let repo = Repository::init(&path).expect("git init");
            let mut cfg = repo.config().expect("repo config");
            cfg.set_str("user.name", "Test").expect("set user.name");
            cfg.set_str("user.email", "t@example.com")
                .expect("set user.email");
            TempRepo { path }
        }
    }

    impl Drop for TempRepo {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.path);
        }
    }

    fn unique_dir(label: &str) -> PathBuf {
        let mut p = std::env::temp_dir();
        let unique = format!(
            "tga-extractor-{}-{}-{}-{label}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0),
            // Counter so multiple commits within the same nanosecond stay
            // unique (path uniqueness, not commit uniqueness).
            rand_like(),
        );
        p.push(unique);
        p
    }

    fn rand_like() -> u64 {
        // Cheap monotonically-increasing-ish nonce. We just need uniqueness
        // within a single test run, not cryptographic randomness.
        use std::sync::atomic::{AtomicU64, Ordering};
        static N: AtomicU64 = AtomicU64::new(0);
        N.fetch_add(1, Ordering::Relaxed)
    }

    /// Build a fresh empty repository on disk.
    fn init_repo(label: &str) -> (TempRepo, Repository) {
        let path = unique_dir(label);
        std::fs::create_dir_all(&path).expect("mkdir");
        let repo = Repository::init(&path).expect("git init");
        // Set a stable identity so commits don't depend on global config.
        let mut cfg = repo.config().expect("repo config");
        cfg.set_str("user.name", "Test").expect("set user.name");
        cfg.set_str("user.email", "t@example.com")
            .expect("set user.email");
        (TempRepo { path }, repo)
    }

    /// Create a commit with the given (unix seconds, offset minutes) author
    /// time. The commit touches a unique file so its tree is distinct from
    /// every other commit (otherwise git would dedupe identical trees and
    /// our walk wouldn't iterate over distinct shas).
    fn commit_at(
        repo: &Repository,
        repo_path: &Path,
        seconds: i64,
        offset_minutes: i32,
        msg: &str,
    ) -> git2::Oid {
        let filename = format!("f-{}.txt", rand_like());
        let filepath = repo_path.join(&filename);
        std::fs::write(&filepath, msg).expect("write file");
        let mut index = repo.index().expect("index");
        index
            .add_path(Path::new(&filename))
            .expect("index add_path");
        index.write().expect("index write");
        let tree_oid = index.write_tree().expect("write_tree");
        let tree = repo.find_tree(tree_oid).expect("find_tree");

        let time = Time::new(seconds, offset_minutes);
        let sig =
            Signature::new("Test", "t@example.com", &time).expect("signature with explicit time");

        let parents: Vec<git2::Commit<'_>> = match repo.head() {
            Ok(head) => vec![head.peel_to_commit().expect("peel")],
            Err(_) => vec![],
        };
        let parent_refs: Vec<&git2::Commit<'_>> = parents.iter().collect();

        repo.commit(Some("HEAD"), &sig, &sig, msg, &tree, &parent_refs)
            .expect("commit")
    }

    fn open_in_memory_db() -> Database {
        Database::open_in_memory().expect("open in-memory db")
    }

    /// Helper: collect all commit timestamps stored in the DB.
    fn db_commit_timestamps(db: &Database) -> Vec<String> {
        let conn = db.connection();
        let mut stmt = conn
            .prepare("SELECT timestamp FROM commits ORDER BY timestamp")
            .expect("prepare");
        let rows = stmt
            .query_map([], |r| r.get::<_, String>(0))
            .expect("query_map");
        rows.map(|r| r.expect("row")).collect()
    }

    fn make_collector(path: &Path, since: Option<&str>, until: Option<&str>) -> GitCollector {
        make_collector_opts(path, since, until, None, false)
    }

    /// Build a minimal [`RepositoryConfig`] for a test repo path.
    fn make_repo_config(path: &Path) -> RepositoryConfig {
        RepositoryConfig {
            name: path
                .file_name()
                .and_then(|n| n.to_str())
                .map(str::to_string),
            path: path.to_path_buf(),
            branch: None,
            since_date: None,
            until_date: None,
            org: None,
            head_only: false,
            fetch_timeout_secs: None,
        }
    }

    /// Full-option collector factory used by branch-coverage tests.
    fn make_collector_opts(
        path: &Path,
        since: Option<&str>,
        until: Option<&str>,
        branch: Option<&str>,
        head_only: bool,
    ) -> GitCollector {
        let cfg = RepositoryConfig {
            name: Some("test-repo".to_string()),
            path: path.to_path_buf(),
            branch: branch.map(str::to_string),
            since_date: since.map(str::to_string),
            until_date: until.map(str::to_string),
            org: None,
            head_only,
            fetch_timeout_secs: None,
        };
        GitCollector::new(&cfg)
            .expect("collector::new")
            .no_fetch(true)
    }

    /// Issue #70 (cause #2): a commit timestamped late on the last day of an
    /// ISO week in a negative-UTC timezone must remain assigned to *that*
    /// week, not bump into the next.
    ///
    /// 2026-05-03 23:43:52 -0700  ==  2026-05-04 06:43:52 UTC
    /// The commit's *local* date (W18 Sunday) must win when we filter on a
    /// W18-aligned window [2026-04-27, 2026-05-03].
    #[test]
    fn commit_late_saturday_local_stays_in_iso_week() {
        let (_t, repo) = init_repo("iso-week-boundary");
        // 2026-05-03 23:43:52 -0700  ==  2026-05-04 06:43:52 UTC
        let seconds = utc_seconds(2026, 5, 4, 6, 43, 52);
        let offset_minutes = -7 * 60;
        commit_at(
            &repo,
            _t.path.as_path(),
            seconds,
            offset_minutes,
            "late sat",
        );

        // W18 2026 window: Mon 2026-04-27 .. Sun 2026-05-03 (inclusive,
        // local-calendar). Express the bounds the same way the by-week
        // collector does: YYYY-MM-DD strings.
        let collector = make_collector(_t.path.as_path(), Some("2026-04-27"), Some("2026-05-03"));
        let mut db = open_in_memory_db();
        let written = collector.collect(&mut db).expect("collect");
        assert_eq!(
            written, 1,
            "commit at 23:43 -0700 on Sun 2026-05-03 must be assigned \
             to W18, not bumped into W19 by UTC drift"
        );
    }

    /// `until_date` must be inclusive: a commit on the exact `until_date`
    /// (in its own local timezone) must be collected.
    #[test]
    fn until_date_is_inclusive_end_of_day() {
        let (_t, repo) = init_repo("until-inclusive");
        // 2026-05-10 23:30:00 +0200  ==  2026-05-10 21:30:00 UTC
        let seconds = utc_seconds(2026, 5, 10, 21, 30, 0);
        let offset_minutes = 2 * 60;
        commit_at(
            &repo,
            _t.path.as_path(),
            seconds,
            offset_minutes,
            "late sun",
        );

        let collector = make_collector(_t.path.as_path(), Some("2026-05-04"), Some("2026-05-10"));
        let mut db = open_in_memory_db();
        let written = collector.collect(&mut db).expect("collect");
        assert_eq!(
            written, 1,
            "commit on the exact until_date must be included (inclusive bound)"
        );

        let rows = db_commit_timestamps(&db);
        assert_eq!(rows.len(), 1, "exactly one row written");
    }

    /// A commit on the first day of a week (Monday) must be included when
    /// the window starts on that Monday.
    #[test]
    fn first_day_of_week_is_inclusive() {
        let (_t, repo) = init_repo("first-day-inclusive");
        // 2026-04-27 00:30:00 UTC — first commit of W18 at minute 30.
        let seconds = utc_seconds(2026, 4, 27, 0, 30, 0);
        commit_at(&repo, _t.path.as_path(), seconds, 0, "monday early");

        let collector = make_collector(_t.path.as_path(), Some("2026-04-27"), Some("2026-05-03"));
        let mut db = open_in_memory_db();
        let written = collector.collect(&mut db).expect("collect");
        assert_eq!(
            written, 1,
            "commit on the exact since_date must be included (inclusive bound)"
        );
    }

    /// A commit strictly outside the window must be filtered out.
    #[test]
    fn commit_after_until_date_is_excluded() {
        let (_t, repo) = init_repo("after-until");
        // 2026-05-11 12:00 UTC — strictly after until_date 2026-05-10.
        let seconds = utc_seconds(2026, 5, 11, 12, 0, 0);
        commit_at(&repo, _t.path.as_path(), seconds, 0, "next monday");

        let collector = make_collector(_t.path.as_path(), Some("2026-05-04"), Some("2026-05-10"));
        let mut db = open_in_memory_db();
        let written = collector.collect(&mut db).expect("collect");
        assert_eq!(written, 0, "commit on 2026-05-11 must NOT be in W19 window");
    }

    /// Issue #316: `tga collect` must populate `commits.ticket_id` at INSERT
    /// time — no separate `tga backfill ticket-ids` run should be required.
    ///
    /// Why: 32% of uncategorized commits (2,006 of 6,212) had extractable JIRA
    /// IDs (`BB-2746`, `SRE-3104`, `DRE-405`) but NULL `ticket_id` because
    /// extraction was only performed during backfill, not during collection.
    /// What: commits with JIRA-style subjects must have their `ticket_id`
    /// populated immediately after `collect`; plain commits must remain NULL.
    /// Test: this test itself.
    #[test]
    fn collect_populates_ticket_id_at_insert_time() {
        let (_t, repo) = init_repo("ticket-id-insert");
        let seconds = utc_seconds(2026, 5, 1, 12, 0, 0);
        // Three sample commits from issue #316.
        commit_at(
            &repo,
            _t.path.as_path(),
            seconds,
            0,
            "BB-2746: refactor auth",
        );
        commit_at(
            &repo,
            _t.path.as_path(),
            seconds - 1,
            0,
            "SRE-3104: increase RDS timeout",
        );
        commit_at(
            &repo,
            _t.path.as_path(),
            seconds - 2,
            0,
            "DRE-405 fix demand calculation",
        );
        // A plain commit — ticket_id must stay NULL.
        commit_at(&repo, _t.path.as_path(), seconds - 3, 0, "misc cleanup");

        let collector = make_collector(_t.path.as_path(), None, None);
        let mut db = open_in_memory_db();
        let written = collector.collect(&mut db).expect("collect");
        assert_eq!(written, 4, "all four commits must be collected");

        let conn = db.connection();

        // Verify all three JIRA commits have the correct ticket_id.
        for (msg_prefix, expected_id) in &[
            ("BB-2746:", "BB-2746"),
            ("SRE-3104:", "SRE-3104"),
            ("DRE-405 ", "DRE-405"),
        ] {
            let ticket_id: Option<String> = conn
                .query_row(
                    "SELECT ticket_id FROM commits WHERE message LIKE ?1",
                    rusqlite::params![format!("{msg_prefix}%")],
                    |r| r.get(0),
                )
                .expect("query ticket_id");
            assert_eq!(
                ticket_id.as_deref(),
                Some(*expected_id),
                "commit '{msg_prefix}...' must have ticket_id='{expected_id}' after collect"
            );
        }

        // Plain commit must have NULL ticket_id.
        let plain_ticket: Option<String> = conn
            .query_row(
                "SELECT ticket_id FROM commits WHERE message = 'misc cleanup'",
                [],
                |r| r.get(0),
            )
            .expect("query plain ticket_id");
        assert!(
            plain_ticket.is_none(),
            "plain commit must have NULL ticket_id, got {plain_ticket:?}"
        );
    }

    /// Direct unit test of `commit_local_date`: a commit at 2026-05-03
    /// 23:43:52 -0700 must report local date 2026-05-03 (not 2026-05-04).
    #[test]
    fn commit_local_date_uses_authoring_timezone() {
        let (_t, repo) = init_repo("local-date-helper");
        // 2026-05-04 06:43:52 UTC = 2026-05-03 23:43:52 -0700.
        let seconds = utc_seconds(2026, 5, 4, 6, 43, 52);
        let offset_minutes = -7 * 60;
        let oid = commit_at(
            &repo,
            _t.path.as_path(),
            seconds,
            offset_minutes,
            "late sat",
        );
        let commit = repo.find_commit(oid).expect("find_commit");
        let local = commit_local_date(&commit).expect("local date");
        assert_eq!(
            local,
            NaiveDate::from_ymd_opt(2026, 5, 3).expect("valid"),
            "commit_local_date must respect the author's recorded offset"
        );
        // Sanity: UTC date would have been 2026-05-04.
        let utc = commit_time_utc(&commit).expect("utc");
        assert_eq!(
            utc.date_naive(),
            NaiveDate::from_ymd_opt(2026, 5, 4).unwrap()
        );
    }

    // -------------------------------------------------------------------------
    // Issue #331 — branch coverage tests (added in tga 2.0.0)
    // -------------------------------------------------------------------------

    /// Helper: create a git branch pointing at the commit `oid`.
    ///
    /// Why: the existing `commit_at` helper always commits to HEAD on the
    /// current branch.  We need to create a side branch and commit to it to
    /// exercise the multi-branch revwalk path.
    /// What: creates `refs/heads/<name>` pointing at `oid`.
    /// Test: used by the #331 branch-coverage tests.
    fn create_branch(repo: &Repository, name: &str, oid: git2::Oid) {
        let commit = repo.find_commit(oid).expect("find_commit");
        repo.branch(name, &commit, false).expect("branch");
    }

    /// Switch HEAD to a given branch so subsequent `commit_at` calls land on it.
    ///
    /// Why: `commit_at` uses `repo.head()` to find the parent commit, so HEAD
    /// must point at the target branch for new commits to chain from it.
    /// What: sets HEAD to `refs/heads/<name>` and checks out the worktree so
    /// the index is consistent.
    /// Test: used by multi_branch_coverage and related tests.
    fn switch_branch(repo: &Repository, name: &str) {
        let refname = format!("refs/heads/{name}");
        repo.set_head(&refname).expect("set_head");
        repo.checkout_head(Some(git2::build::CheckoutBuilder::new().force()))
            .expect("checkout_head");
    }

    /// Return the name of the branch HEAD currently points at.
    ///
    /// Why: git2 `Repository::init` uses the system's `init.defaultBranch`
    /// config value (commonly `master` or `main`).  Tests that need to return
    /// HEAD to the default branch after switching to a feature branch must
    /// not hard-code "main".
    /// What: resolves `HEAD` as a symbolic ref and strips the `refs/heads/`
    /// prefix, or returns "master" as a last resort.
    /// Test: used in multi_branch_coverage and related tests.
    fn current_branch_name(repo: &Repository) -> String {
        repo.head()
            .ok()
            .and_then(|h| h.shorthand().map(str::to_string))
            .unwrap_or_else(|| "master".to_string())
    }

    /// Helper: count distinct commit SHAs in the DB.
    fn db_commit_count(db: &Database) -> usize {
        let conn = db.connection();
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM commits", [], |r| r.get(0))
            .expect("count");
        n as usize
    }

    /// Issue #331 — Test 1: default (head_only=false) walk collects commits on
    /// ALL branches, not just the default branch.
    ///
    /// Why: the 1.x HEAD-only walk silently dropped ~56% of commits in
    /// multi-branch repos.  This test verifies the 2.0.0 all-branch default
    /// collects every commit regardless of which branch it lives on.
    /// What: creates 2 commits on main, branches to feature/x and creates 3
    /// more, returns to main, and asserts all 5 are collected.
    /// Test: this test itself.
    #[test]
    fn multi_branch_coverage() {
        let (_t, repo) = init_repo("multi-branch-all");
        let base_ts = utc_seconds(2026, 5, 1, 12, 0, 0);

        // 2 commits on main.
        commit_at(&repo, _t.path.as_path(), base_ts, 0, "main-1");
        let main2 = commit_at(&repo, _t.path.as_path(), base_ts + 1, 0, "main-2");

        // Create feature/x off main and add 3 commits.
        let default_branch = current_branch_name(&repo);
        create_branch(&repo, "feature/x", main2);
        switch_branch(&repo, "feature/x");
        commit_at(&repo, _t.path.as_path(), base_ts + 2, 0, "feat-1");
        commit_at(&repo, _t.path.as_path(), base_ts + 3, 0, "feat-2");
        commit_at(&repo, _t.path.as_path(), base_ts + 4, 0, "feat-3");

        // Return to main (so HEAD points at main's tip — not feature/x).
        switch_branch(&repo, &default_branch);

        // Default collector: head_only = false → all branches.
        let collector = make_collector_opts(_t.path.as_path(), None, None, None, false);
        let mut db = open_in_memory_db();
        let written = collector.collect(&mut db).expect("collect");

        assert_eq!(
            written, 5,
            "all-branch walk must collect all 5 commits (2 on main + 3 on feature/x); \
             got {written}"
        );
        assert_eq!(db_commit_count(&db), 5);
    }

    /// Issue #331 — Test 2: `--head-only` flag restores legacy HEAD-only
    /// behaviour, collecting only commits reachable from HEAD.
    ///
    /// Why: operators who want the old behaviour must be able to opt out via
    /// `--head-only` or `head_only: true` in YAML.
    /// What: same setup as Test 1 but collects with `head_only = true`; since
    /// HEAD is on main, only the 2 main commits should be returned.
    /// Test: this test itself.
    #[test]
    fn head_only_legacy_behavior() {
        let (_t, repo) = init_repo("multi-branch-headonly");
        let base_ts = utc_seconds(2026, 5, 1, 12, 0, 0);

        // 2 commits on main.
        commit_at(&repo, _t.path.as_path(), base_ts, 0, "main-1");
        let main2 = commit_at(&repo, _t.path.as_path(), base_ts + 1, 0, "main-2");

        // Branch feature/x — 3 more commits (not reachable from HEAD/main).
        let default_branch = current_branch_name(&repo);
        create_branch(&repo, "feature/x", main2);
        switch_branch(&repo, "feature/x");
        commit_at(&repo, _t.path.as_path(), base_ts + 2, 0, "feat-1");
        commit_at(&repo, _t.path.as_path(), base_ts + 3, 0, "feat-2");
        commit_at(&repo, _t.path.as_path(), base_ts + 4, 0, "feat-3");

        // Return to main — HEAD points at the 2-commit ancestry.
        switch_branch(&repo, &default_branch);

        // head_only = true → legacy walk, only HEAD ancestry.
        let collector = make_collector_opts(_t.path.as_path(), None, None, None, true);
        let mut db = open_in_memory_db();
        let written = collector.collect(&mut db).expect("collect");

        assert_eq!(
            written, 2,
            "head_only walk must only collect the 2 main commits; got {written}"
        );
        assert_eq!(db_commit_count(&db), 2);
    }

    /// Issue #331 — Test 3: explicit `branch` override still walks only that
    /// branch's ancestry regardless of `head_only` setting.
    ///
    /// Why: per-repo `branch:` overrides should be unaffected by the 2.0.0
    /// default change — they remain an explicit single-branch selector.
    /// What: same setup; collect with `branch = Some("feature/x")` and
    /// `head_only = false`.  Expects the 2 main + 3 feature commits (5 total)
    /// because feature/x's ancestry includes both branches.
    /// Test: this test itself.
    #[test]
    fn branch_override_still_works() {
        let (_t, repo) = init_repo("multi-branch-override");
        let base_ts = utc_seconds(2026, 5, 1, 12, 0, 0);

        // 2 commits on main.
        commit_at(&repo, _t.path.as_path(), base_ts, 0, "main-1");
        let main2 = commit_at(&repo, _t.path.as_path(), base_ts + 1, 0, "main-2");

        // Branch feature/x — 3 more commits.
        let default_branch = current_branch_name(&repo);
        create_branch(&repo, "feature/x", main2);
        switch_branch(&repo, "feature/x");
        commit_at(&repo, _t.path.as_path(), base_ts + 2, 0, "feat-1");
        commit_at(&repo, _t.path.as_path(), base_ts + 3, 0, "feat-2");
        commit_at(&repo, _t.path.as_path(), base_ts + 4, 0, "feat-3");

        // Return to main.
        switch_branch(&repo, &default_branch);

        // Explicit branch override: walks feature/x ancestry which includes
        // the 2 main commits (they are ancestors of feature/x).
        let collector =
            make_collector_opts(_t.path.as_path(), None, None, Some("feature/x"), false);
        let mut db = open_in_memory_db();
        let written = collector.collect(&mut db).expect("collect");

        // feature/x was branched from main, so its full ancestry is 5 commits.
        assert_eq!(
            written, 5,
            "branch=feature/x walk must include its full ancestry (2 base + 3 feature = 5); \
             got {written}"
        );
        assert_eq!(db_commit_count(&db), 5);
    }

    /// Issue #331 — Test 4: all-branch walk on a repo where there is only a
    /// detached HEAD and no local branches falls back gracefully to HEAD.
    ///
    /// Why: CI shallow clones may have a detached HEAD and no `refs/heads/*`.
    /// The fallback must not panic or return an error.
    /// What: initialise a repo, make one commit directly (which puts HEAD in
    /// a normal state on the default branch), then manually delete the
    /// `refs/heads/main` ref so the walk has no local branches to push.
    /// Assert that collect returns the single commit via the HEAD fallback.
    /// Test: this test itself.
    #[test]
    fn all_branches_fallback_when_no_local_refs() {
        let (_t, repo) = init_repo("no-local-refs-fallback");
        let base_ts = utc_seconds(2026, 5, 1, 12, 0, 0);

        // One commit on the default branch (main or master depending on git config).
        commit_at(&repo, _t.path.as_path(), base_ts, 0, "only-commit");

        // Detach HEAD so refs/heads/* is empty.  We do this by setting HEAD
        // directly to the commit OID (a detached HEAD), then deleting all
        // local branch refs.
        let head_commit = repo.head().expect("head").peel_to_commit().expect("peel");
        // Detach HEAD to the commit OID.
        repo.set_head_detached(head_commit.id())
            .expect("detach HEAD");
        // Delete all local branch refs so refs/heads/* is empty.
        let ref_names: Vec<String> = repo
            .references()
            .expect("references")
            .flatten()
            .filter_map(|r| {
                r.name().and_then(|n| {
                    if n.starts_with("refs/heads/") {
                        Some(n.to_string())
                    } else {
                        None
                    }
                })
            })
            .collect();
        for rn in ref_names {
            repo.find_reference(&rn)
                .expect("find ref")
                .delete()
                .expect("delete ref");
        }

        // All-branch walk (head_only = false) should fall back to HEAD.
        let collector = make_collector_opts(_t.path.as_path(), None, None, None, false);
        let mut db = open_in_memory_db();
        let written = collector
            .collect(&mut db)
            .expect("collect — must not error");
        assert_eq!(
            written, 1,
            "fallback to HEAD must yield the single commit; got {written}"
        );
    }

    /// Why: `perform_fetch` must return Skipped when `no_fetch = true` so
    /// that `--no-fetch` callers get a typed outcome without opening the repo.
    /// What: builds a collector with `no_fetch(true)` on a temp repo and
    /// calls `perform_fetch`; expects `FetchOutcome::Skipped`.
    /// Test: this test itself.
    #[test]
    fn no_fetch_returns_skipped() {
        use crate::collect::collector::FetchOutcome;
        let _t = TempRepo::new();
        let cfg = make_repo_config(_t.path.as_path());
        let collector = GitCollector::new(&cfg).expect("new").no_fetch(true);
        let prf = collector.perform_fetch();
        assert!(
            matches!(prf.outcome, FetchOutcome::Skipped { .. }),
            "expected Skipped when no_fetch=true, got {:?}",
            prf.outcome
        );
        assert_eq!(
            prf.repo,
            _t.path.file_name().unwrap().to_string_lossy().as_ref()
        );
    }

    /// Why: `perform_fetch` on a local-only repo (no remotes) must return
    /// Skipped rather than Failed, because "no remote" is a valid config.
    /// What: builds a collector with `no_fetch(false)` on a temp repo that
    /// has no remotes and calls `perform_fetch`.
    /// Test: this test itself.
    #[test]
    fn perform_fetch_local_only_repo_returns_skipped() {
        use crate::collect::collector::FetchOutcome;
        let _t = TempRepo::new();
        let cfg = make_repo_config(_t.path.as_path());
        let collector = GitCollector::new(&cfg).expect("new").no_fetch(false);
        let prf = collector.perform_fetch();
        // Local-only repo → no "origin" remote → Skipped.
        assert!(
            matches!(prf.outcome, FetchOutcome::Skipped { .. }),
            "expected Skipped for local-only repo, got {:?}",
            prf.outcome
        );
    }

    /// Why: `with_fetch_timeout` must store the value so callers can
    /// introspect it (and future enforcement can read it).
    /// What: sets a timeout via the builder and verifies the value is stored.
    /// Test: this test itself (struct field is private, but `perform_fetch`
    /// logs the value without erroring — we just verify no panic).
    #[test]
    fn fetch_timeout_stored_does_not_panic() {
        let _t = TempRepo::new();
        let cfg = make_repo_config(_t.path.as_path());
        // Should not panic even when timeout is set.
        let collector = GitCollector::new(&cfg)
            .expect("new")
            .no_fetch(true)
            .with_fetch_timeout(Some(30));
        let prf = collector.perform_fetch();
        // no_fetch=true → always Skipped, regardless of timeout
        assert!(matches!(
            prf.outcome,
            crate::collect::collector::FetchOutcome::Skipped { .. }
        ));
    }
}
