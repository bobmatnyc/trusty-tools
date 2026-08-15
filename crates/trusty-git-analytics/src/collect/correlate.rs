//! The deterministic commit ↔ board-item linking pass.
//!
//! Why: board pulls populate `work_items` and the git walk populates
//! `commits.ticket_id`, but outside the ADO path nothing joined the two. #5197
//! needs that join to run on demand with live progress — and to run with no
//! model configured and no API key present, since zero inference is the
//! default. Everything here is SQL plus the existing commit-message extractor
//! [`crate::collect::ticket::extract_ticket_id`]: no network, no LLM.
//!
//! What: [`correlate_commits`] links every commit whose ticket key matches a
//! `work_items` row that is ALREADY in the database. A key with no matching
//! work item is counted as a gap, never invented — closing that gap is a board
//! pull's job, not this pass's. The schema is untouched (#5197 scope).
//!
//! Read views over the resulting rows live in
//! [`crate::core::db::correlation`].
//!
//! Test: `tests` at the bottom of this file.

use rusqlite::{params, Connection};

use crate::collect::ticket::extract_ticket_id;
use crate::core::db::work_items::link_commit_work_item;
use crate::core::errors::{Result, TgaError};
use crate::core::progress::{ProgressBus, ProgressEvent, Stage};

/// The [`Stage::Correlate`] target label every event in this pass carries.
///
/// Why: the pass is one logical unit, so its progress rows must agree on a
/// single target name or the aggregate splits them across rows.
/// What: `"commit → board item"`.
/// Test: `tests::emits_started_and_completed`.
const TARGET: &str = "commit → board item";

/// Emit a progress event every this many commits scanned.
///
/// Why: one event per commit would flood the bus's ring on a 180k-commit
/// corpus and evict everything useful; a coarse cadence keeps the display live
/// without the noise.
/// What: `250` commits.
/// Test: `tests::emits_started_and_completed`.
const PROGRESS_EVERY: usize = 250;

/// What one [`correlate_commits`] pass did.
///
/// Why: the pass is idempotent, so `linked == 0` is a meaningful success and
/// must be distinguishable from "there was nothing to look at".
/// What: per-commit disposition counts, summing to `scanned`.
/// `#[non_exhaustive]` so later fields stay additive for the published crate.
/// Test: `tests::links_matching_ticket_keys`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CorrelationOutcome {
    /// Commits examined.
    pub scanned: u64,
    /// New `commit_work_items` rows written by this pass.
    pub linked: u64,
    /// Commits that already carried a link before the pass ran.
    pub already_linked: u64,
    /// Commits with no extractable ticket reference.
    pub no_ticket: u64,
    /// Commits whose ticket key has no matching `work_items` row — the gap a
    /// board pull would close.
    pub no_work_item: u64,
}

impl CorrelationOutcome {
    /// One-line summary for the activity log and the CLI.
    ///
    /// Why: the four dispositions only mean something together.
    /// What: `"N linked, N already linked, N ticket without board item, N without ticket"`.
    /// Test: `tests::summary_names_every_disposition`.
    pub fn summary(&self) -> String {
        format!(
            "{} linked, {} already linked, {} ticket without board item, {} without ticket",
            self.linked, self.already_linked, self.no_work_item, self.no_ticket
        )
    }
}

/// A commit as the correlation pass sees it.
type Candidate = (String, Option<String>, String, bool);

/// Snapshot every commit with the facts the pass needs.
///
/// Why: reading the whole candidate set up front keeps the read statement from
/// being held open across the write transaction, which SQLite would otherwise
/// have to reconcile.
/// What: `(sha, ticket_id, message, already_linked)` for every commit.
/// Test: exercised by every test in this module.
fn load_candidates(conn: &Connection) -> Result<Vec<Candidate>> {
    let mut stmt = conn
        .prepare(
            "SELECT c.sha, c.ticket_id, c.message, \
                    EXISTS (SELECT 1 FROM commit_work_items w WHERE w.commit_sha = c.sha) \
             FROM commits c ORDER BY c.sha",
        )
        .map_err(TgaError::from)?;
    let mapped = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, i64>(3)? != 0,
            ))
        })
        .map_err(TgaError::from)?;
    let mut out = Vec::new();
    for row in mapped {
        out.push(row.map_err(TgaError::from)?);
    }
    Ok(out)
}

/// The ticket key for a commit: the stored column, else the message.
///
/// Why: `commits.ticket_id` is populated at collection time, but rows written
/// before that column existed (or by a collector run that predates the
/// extractor) still carry the key in the message. Falling back keeps the pass
/// useful on an old database without a re-collect.
/// What: the trimmed non-empty `ticket_id`, else
/// [`extract_ticket_id`] over `message`, else `None`.
/// Test: `tests::links_matching_ticket_keys` (the `ddd` case).
fn ticket_key(ticket_id: Option<&str>, message: &str) -> Option<String> {
    ticket_id
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(str::to_string)
        .or_else(|| extract_ticket_id(message))
}

/// Link commits to board items already present in `work_items`.
///
/// Why: see the module doc — this is the deterministic half of #5197's
/// correlation story, and it must work with zero credentials.
///
/// What: walks every commit, skipping ones that already carry a link, resolves
/// each remaining commit's ticket key via [`ticket_key`], and writes a
/// `commit_work_items` row for every `work_items` entry with that id, across
/// all sources. All writes run in one transaction and go through
/// [`link_commit_work_item`]'s `INSERT OR IGNORE`, so re-running the pass
/// changes nothing. Progress is published to `bus` at start, every
/// [`PROGRESS_EVERY`] commits, and at the end; with
/// [`ProgressBus::disabled`] every one of those emits is a no-op.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] when a query, insert, or the commit fails.
pub fn correlate_commits(conn: &mut Connection, bus: &ProgressBus) -> Result<CorrelationOutcome> {
    let candidates = load_candidates(conn)?;
    let total = candidates.len() as u64;
    bus.emit(ProgressEvent::started(
        Stage::Correlate,
        TARGET,
        Some(total),
    ));

    let mut outcome = CorrelationOutcome::default();
    let tx = conn.transaction().map_err(TgaError::from)?;
    {
        let mut lookup = tx
            .prepare("SELECT source FROM work_items WHERE id = ?1 ORDER BY source")
            .map_err(TgaError::from)?;

        for (i, (sha, ticket_id, message, already)) in candidates.iter().enumerate() {
            outcome.scanned += 1;
            if *already {
                outcome.already_linked += 1;
            } else {
                match ticket_key(ticket_id.as_deref(), message) {
                    None => outcome.no_ticket += 1,
                    Some(key) => {
                        let sources: Vec<String> = lookup
                            .query_map(params![key], |r| r.get::<_, String>(0))
                            .map_err(TgaError::from)?
                            .collect::<std::result::Result<_, _>>()
                            .map_err(TgaError::from)?;
                        if sources.is_empty() {
                            outcome.no_work_item += 1;
                        }
                        for source in &sources {
                            link_commit_work_item(&tx, sha, &key, source)?;
                            outcome.linked += 1;
                        }
                    }
                }
            }

            if (i + 1).is_multiple_of(PROGRESS_EVERY) {
                bus.emit(ProgressEvent::advanced(
                    Stage::Correlate,
                    TARGET,
                    outcome.scanned,
                    Some(total),
                ));
            }
        }
    }
    tx.commit().map_err(TgaError::from)?;

    bus.emit(
        ProgressEvent::completed(Stage::Correlate, TARGET, outcome.scanned)
            .with_detail(outcome.summary()),
    );
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::correlation::{correlation_counts, correlation_rows, CorrelationFilter};
    use crate::core::db::work_items::{upsert_work_item, WorkItemRow};
    use crate::core::db::Database;

    fn work_item(id: &str, source: &str) -> WorkItemRow {
        WorkItemRow {
            id: id.into(),
            source: source.into(),
            title: format!("Item {id}"),
            status: "Open".into(),
            item_type: "Task".into(),
            tags: None,
            project: None,
            url: None,
            raw_json: None,
        }
    }

    fn insert_commit(conn: &Connection, sha: &str, message: &str, ticket: Option<&str>) {
        conn.execute(
            "INSERT INTO commits (sha, author_name, author_email, timestamp, message, \
                                  repository, ticket_id) \
             VALUES (?1, 'A', 'a@x', '2026-01-01T00:00:00Z', ?2, 'repo', ?3)",
            params![sha, message, ticket],
        )
        .expect("insert commit");
    }

    #[test]
    fn links_matching_ticket_keys() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "PROJ-1 x", Some("PROJ-1"));
        insert_commit(
            db.connection(),
            "bbb",
            "PROJ-9 no such item",
            Some("PROJ-9"),
        );
        insert_commit(db.connection(), "ccc", "chore: nothing here", None);
        // ticket_id column empty, but the subject still declares the key.
        // #5199: the key must lead the subject — a mid-prose `fixes PROJ-1`
        // no longer counts, so this fixture states it the way a real commit
        // would. The behaviour under test (column empty → read the message)
        // is unchanged.
        insert_commit(db.connection(), "ddd", "PROJ-1 fixed again", None);
        upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("upsert");

        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(out.scanned, 4);
        assert_eq!(out.linked, 2, "aaa and ddd both resolve PROJ-1");
        assert_eq!(out.no_work_item, 1, "PROJ-9 has no board item");
        assert_eq!(out.no_ticket, 1);
        assert_eq!(out.already_linked, 0);
        assert_eq!(
            correlation_counts(db.connection()).expect("counts").linked,
            2
        );
    }

    /// The whole point of #5197's zero-inference default: a database with no
    /// LLM config, no API key, and no network still produces the full
    /// deterministic correlation result.
    #[test]
    fn produces_full_result_with_no_credentials_configured() {
        // Nothing in this test reads config, env, or the network.
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "ENG-7 ship it", Some("ENG-7"));
        insert_commit(db.connection(), "bbb", "AB#42 azdo work", Some("AB#42"));
        insert_commit(db.connection(), "ccc", "chore: no ticket", None);
        upsert_work_item(db.connection(), &work_item("ENG-7", "linear")).expect("linear");
        upsert_work_item(db.connection(), &work_item("AB#42", "azdo")).expect("azdo");

        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(out.linked, 2);
        assert_eq!(out.no_ticket, 1);

        let counts = correlation_counts(db.connection()).expect("counts");
        assert_eq!(counts.commits, 3);
        assert_eq!(counts.linked, 2);
        assert_eq!(counts.unticketed, 1);
        assert_eq!(counts.work_items_linked, 2);

        let rows =
            correlation_rows(db.connection(), CorrelationFilter::Unlinked, 10).expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].sha, "ccc", "the gap is named, not guessed at");
    }

    #[test]
    fn is_idempotent() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "PROJ-1 x", Some("PROJ-1"));
        upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("upsert");

        let first = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("1st");
        assert_eq!(first.linked, 1);
        let second = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("2nd");
        assert_eq!(second.linked, 0);
        assert_eq!(second.already_linked, 1);
        assert_eq!(
            correlation_counts(db.connection()).expect("counts").linked,
            1
        );
    }

    #[test]
    fn links_the_same_key_across_sources() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "PROJ-1 x", Some("PROJ-1"));
        upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("jira");
        upsert_work_item(db.connection(), &work_item("PROJ-1", "linear")).expect("linear");

        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(out.linked, 2);
    }

    #[test]
    fn never_invents_a_work_item() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "PROJ-1 x", Some("PROJ-1"));
        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(out.linked, 0);
        assert_eq!(out.no_work_item, 1);
        assert_eq!(
            correlation_counts(db.connection())
                .expect("counts")
                .work_items,
            0
        );
    }

    #[test]
    fn emits_started_and_completed() {
        let mut db = Database::open_in_memory().expect("open");
        for i in 0..3 {
            insert_commit(db.connection(), &format!("sha{i}"), "chore: x", None);
        }
        let bus = ProgressBus::bounded(64);
        correlate_commits(db.connection_mut(), &bus).expect("run");
        let events = bus.drain();
        assert_eq!(events.len(), 2, "one started, one completed (3 < 250)");
        assert_eq!(events[0].stage, Stage::Correlate);
        assert_eq!(events[0].target, TARGET);
        assert_eq!(events[0].total, Some(3));
        assert!(!events[0].is_terminal());
        assert!(events[1].is_terminal());
        assert_eq!(events[1].done, 3);
    }

    /// A disabled bus must not change what the pass does — the guarantee that
    /// the existing CLI paths are unaffected by the bus wiring.
    #[test]
    fn disabled_bus_produces_identical_outcome() {
        let build = || {
            let db = Database::open_in_memory().expect("open");
            insert_commit(db.connection(), "aaa", "PROJ-1 x", Some("PROJ-1"));
            insert_commit(db.connection(), "bbb", "chore: y", None);
            upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("upsert");
            db
        };
        let mut off = build();
        let mut on = build();
        let a = correlate_commits(off.connection_mut(), &ProgressBus::disabled()).expect("off");
        let b = correlate_commits(on.connection_mut(), &ProgressBus::bounded(8)).expect("on");
        assert_eq!(a, b);
    }

    #[test]
    fn summary_names_every_disposition() {
        let out = CorrelationOutcome {
            scanned: 4,
            linked: 1,
            already_linked: 1,
            no_ticket: 1,
            no_work_item: 1,
        };
        let s = out.summary();
        assert!(s.contains("1 linked"));
        assert!(s.contains("1 already linked"));
        assert!(s.contains("1 ticket without board item"));
        assert!(s.contains("1 without ticket"));
    }

    #[test]
    fn ticket_key_prefers_column_then_message() {
        assert_eq!(
            ticket_key(Some("PROJ-1"), "PROJ-2 unrelated"),
            Some("PROJ-1".into())
        );
        // #5199: the message fallback reads a subject-leading key. The
        // fixtures say `PROJ-2 seen` rather than `sees PROJ-2` for that
        // reason; which of the two sources wins is what this test covers.
        assert_eq!(ticket_key(Some("  "), "PROJ-2 seen"), Some("PROJ-2".into()));
        assert_eq!(ticket_key(None, "PROJ-2 seen"), Some("PROJ-2".into()));
        assert_eq!(ticket_key(None, "chore: nothing"), None);
    }

    /// Why: #5199 — this pass is where the corrupted key became an operator-
    /// facing number. A commit citing an ADR in its body was stored with
    /// `ticket_id = 'ADR-0034'` and counted in `no_work_item`, reporting a
    /// coverage gap against a ticket that exists on no board.
    /// What: the ADR-citing commit resolves to the GitHub issue its body
    /// names, links to the real work item, and never lands in `no_work_item`.
    /// Test: this test itself.
    #[test]
    fn adr_citation_does_not_become_a_phantom_coverage_gap() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(
            db.connection(),
            "352fe5d6",
            "fix(relay): verify the HMAC once\n\nPer ADR-0034 the relay spools durably.\nCloses #5089\n",
            None,
        );
        upsert_work_item(db.connection(), &work_item("#5089", "github")).expect("upsert");

        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(
            out.linked, 1,
            "resolves the issue the commit actually cites"
        );
        assert_eq!(out.no_work_item, 0, "ADR-0034 is not a phantom gap");
        assert_eq!(out.no_ticket, 0);
    }
}
