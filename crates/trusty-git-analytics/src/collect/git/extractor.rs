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

use crate::collect::errors::{CollectError, Result};
use crate::collect::git::diff::{compute_commit_diff, CommitDiff};
use crate::collect::git::fetch::fetch_remote;
use crate::collect::ticket::is_ticketed;
use crate::core::config::{expand_path, RepositoryConfig};
use crate::core::db::Database;

/// Extracts commits from a single configured repository.
#[derive(Debug)]
pub struct GitCollector {
    /// Resolved on-disk path of the repository.
    path: PathBuf,
    /// Display name used in the `repository` column.
    name: String,
    /// Branch override (None = HEAD).
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

        // Optional pre-walk remote fetch. Soft-fails on auth/transport so a
        // misconfigured remote doesn't break collection on local history.
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
            debug!(repo = %self.name, "skipping pre-walk fetch (--no-fetch)");
        }

        let mut revwalk = repo.revwalk()?;
        revwalk.set_sorting(Sort::TIME)?;
        match &self.branch {
            Some(name) => {
                let refname = format!("refs/heads/{name}");
                if revwalk.push_ref(&refname).is_err() {
                    // Try as a generic revision (could be a tag or remote ref).
                    revwalk.push_ref(name)?;
                }
            }
            None => revwalk.push_head()?,
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

            let inserted = tx.execute(
                "INSERT OR IGNORE INTO commits \
                 (sha, author_name, author_email, timestamp, message, repository, \
                  files_changed, insertions, deletions, is_merge, ticketed) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
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
        let cfg = RepositoryConfig {
            name: Some("test-repo".to_string()),
            path: path.to_path_buf(),
            branch: None,
            since_date: since.map(str::to_string),
            until_date: until.map(str::to_string),
            org: None,
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
}
