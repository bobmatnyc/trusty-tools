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

use std::collections::HashMap;

use rusqlite::{params, Connection};

use crate::collect::ticket::{branch_ticket_key, extract_ticket_id};
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
    /// Commits whose key came from their pull request's branch name (#5734).
    ///
    /// Why: harvesting a source that yields nothing must be visible as a zero,
    /// not as silence. On this repository this reads 0 — branch names here are
    /// lowercase (`fix/5734-slug`), so there is no key to find. An operator who
    /// expected otherwise can see that from the summary line rather than
    /// inferring it from an unchanged link count.
    pub from_branch: u64,
    /// Commits whose key came from their pull request's body (#5734).
    pub from_pr_body: u64,
}

impl CorrelationOutcome {
    /// One-line summary for the activity log and the CLI.
    ///
    /// Why: the dispositions only mean something together. #5734 appends the
    /// two harvest counts unconditionally — printing them only when non-zero
    /// would make "this source found nothing" and "this source was never
    /// consulted" the same output.
    /// What: the four dispositions, then `"N via branch, N via PR body"`.
    /// Test: `tests::summary_names_every_disposition`,
    /// `tests::summary_reports_a_zero_harvest_rather_than_omitting_it`.
    pub fn summary(&self) -> String {
        format!(
            "{} linked, {} already linked, {} ticket without board item, \
             {} without ticket ({} via branch, {} via PR body)",
            self.linked,
            self.already_linked,
            self.no_work_item,
            self.no_ticket,
            self.from_branch,
            self.from_pr_body
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

/// Where a commit's ticket key came from.
///
/// Why: #5734 added two more places a key can come from, and they disagree —
/// 323 of this repository's merged commits have a subject key and a PR-body key
/// that differ. Which one wins must be a stated rule, not a side effect of the
/// order someone happened to write the `or_else` chain in.
///
/// What: the variants are declared in precedence order and [`SOURCE_ORDER`]
/// iterates them; `derive(PartialOrd)` follows declaration order, so the type
/// and the constant cannot drift apart. The ordering rests on how specifically
/// the author was naming THIS commit's work:
///
/// 1. [`Self::CommitText`] — the commit's own subject, written about this one
///    commit. #5199 established subject position as authoritative and nothing
///    here weakens it: a new source never displaces a key the commit declares.
/// 2. [`Self::BranchName`] — authored once as a work identifier and shared by
///    every commit on the branch. Coarser than a subject, but it is a
///    deliberate identifier rather than prose.
/// 3. [`Self::PrBody`] — prose written last, routinely citing several issues.
///    Weakest, and the only source measured to need an action keyword before
///    it can be believed at all.
///
/// Test: `tests::precedence_prefers_commit_text_then_branch_then_body`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum TicketSource {
    /// The commit's own `ticket_id` column or subject line.
    CommitText,
    /// The head-ref of the pull request that carried the commit.
    BranchName,
    /// The `Closes #N` reference in that pull request's body.
    PrBody,
}

/// The sources a commit's ticket key is resolved from, most trusted first.
///
/// Why: see [`TicketSource`] — the precedence is data, so it can be read in one
/// place and asserted directly by a test.
/// What: the three variants in declaration order.
/// Test: `tests::precedence_order_matches_the_declared_variant_order`.
const SOURCE_ORDER: [TicketSource; 3] = [
    TicketSource::CommitText,
    TicketSource::BranchName,
    TicketSource::PrBody,
];

/// What one pull request contributes to the commits it produced.
///
/// Why: #5734 — a commit carries no branch, so the branch and body keys arrive
/// through `pull_requests.commit_shas`. Both are `Option` because a provider
/// that supplies neither must be distinguishable from one that supplied an
/// empty value.
/// What: the harvested key from each source, already extracted.
/// Test: `tests::branch_name_supplies_a_key_the_subject_lacks`.
#[derive(Debug, Clone, Default)]
struct PrKeys {
    /// Key read from the PR's head ref by [`branch_ticket_key`].
    branch: Option<String>,
    /// Key extracted from the PR body at fetch time.
    body: Option<String>,
}

/// Map every commit SHA a pull request produced to that PR's harvested keys.
///
/// Why: #5734's join. `pull_requests.commit_shas` is a JSON array of SHAs, so
/// the mapping is built in Rust rather than leaning on SQLite's JSON1
/// extension, matching how [`load_candidates`] already reads everything up
/// front.
///
/// What: reads every PR row carrying a head ref or a body key, runs
/// [`branch_ticket_key`] over the head ref, and indexes both by SHA. A PR whose
/// `commit_shas` will not parse is skipped rather than failing the pass — the
/// column is provider-written and one malformed row must not cost the whole
/// correlation. A later PR wins a SHA collision, which cannot occur in practice
/// because a squash merge SHA belongs to exactly one PR.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] when the query itself fails — that is a whole
/// -source failure, not one skippable row, so it is NOT swallowed.
fn load_pr_keys(conn: &Connection) -> Result<HashMap<String, PrKeys>> {
    let mut stmt = conn
        .prepare(
            "SELECT commit_shas, head_ref, body_ticket_id FROM pull_requests \
             WHERE (head_ref IS NOT NULL AND head_ref != '') OR body_ticket_id IS NOT NULL",
        )
        .map_err(TgaError::from)?;
    let mapped = stmt
        .query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, Option<String>>(1)?,
                r.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(TgaError::from)?;

    let mut out: HashMap<String, PrKeys> = HashMap::new();
    for row in mapped {
        let (shas_json, head_ref, body_key) = row.map_err(TgaError::from)?;
        let branch = head_ref.as_deref().and_then(branch_ticket_key);
        if branch.is_none() && body_key.is_none() {
            continue;
        }
        // #5734: a provider-written JSON column; a malformed one skips this PR.
        let Ok(shas) = serde_json::from_str::<Vec<String>>(&shas_json) else {
            continue;
        };
        for sha in shas {
            out.insert(
                sha,
                PrKeys {
                    branch: branch.clone(),
                    body: body_key.clone(),
                },
            );
        }
    }
    Ok(out)
}

/// The ticket key for a commit, and which source produced it.
///
/// Why: `commits.ticket_id` is populated at collection time, but rows written
/// before that column existed (or by a collector run that predates the
/// extractor) still carry the key in the message. Falling back keeps the pass
/// useful on an old database without a re-collect. #5734 adds the two
/// pull-request sources behind it.
///
/// What: walks [`SOURCE_ORDER`] and returns the first source that yields a key,
/// so the precedence is the constant's, not this function's control flow.
/// [`TicketSource::CommitText`] is the trimmed non-empty `ticket_id` else
/// [`extract_ticket_id`] over the message — unchanged from before #5734, which
/// is what keeps #5199's subject-position rule authoritative.
///
/// Test: `tests::ticket_key_prefers_column_then_message`,
/// `tests::precedence_prefers_commit_text_then_branch_then_body`.
fn ticket_key(
    ticket_id: Option<&str>,
    message: &str,
    pr: Option<&PrKeys>,
) -> Option<(String, TicketSource)> {
    SOURCE_ORDER.iter().find_map(|&source| {
        let key = match source {
            TicketSource::CommitText => ticket_id
                .map(str::trim)
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| extract_ticket_id(message)),
            TicketSource::BranchName => pr.and_then(|p| p.branch.clone()),
            TicketSource::PrBody => pr.and_then(|p| p.body.clone()),
        };
        key.map(|k| (k, source))
    })
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
    // #5734: branch and PR-body keys reach a commit through `commit_shas`.
    let pr_keys = load_pr_keys(conn)?;
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
                match ticket_key(ticket_id.as_deref(), message, pr_keys.get(sha)) {
                    None => outcome.no_ticket += 1,
                    Some((key, from)) => {
                        // #5734: count the harvest per source so a source that
                        // finds nothing reports a zero instead of staying silent.
                        match from {
                            TicketSource::CommitText => {}
                            TicketSource::BranchName => outcome.from_branch += 1,
                            TicketSource::PrBody => outcome.from_pr_body += 1,
                        }
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

    /// Seed a pull request that claims `sha`, with a head ref and/or a
    /// body-extracted key (#5734).
    fn insert_pr(
        conn: &Connection,
        pr_number: i64,
        sha: &str,
        head_ref: Option<&str>,
        body_key: Option<&str>,
    ) {
        let shas = serde_json::to_string(&vec![sha]).expect("encode shas");
        conn.execute(
            "INSERT INTO pull_requests \
             (provider, repository, pr_number, title, author, state, created_at, \
              commit_shas, head_ref, body_ticket_id) \
             VALUES ('github', 'acme/widgets', ?1, 'T', 'ada', 'merged', \
                     '2026-01-01T00:00:00Z', ?2, ?3, ?4)",
            params![pr_number, shas, head_ref.unwrap_or(""), body_key],
        )
        .expect("insert pr");
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
            from_branch: 2,
            from_pr_body: 3,
        };
        let s = out.summary();
        assert!(s.contains("1 linked"));
        assert!(s.contains("1 already linked"));
        assert!(s.contains("1 ticket without board item"));
        assert!(s.contains("1 without ticket"));
        assert!(s.contains("2 via branch"));
        assert!(s.contains("3 via PR body"));
    }

    #[test]
    fn ticket_key_prefers_column_then_message() {
        assert_eq!(
            ticket_key(Some("PROJ-1"), "PROJ-2 unrelated", None),
            Some(("PROJ-1".into(), TicketSource::CommitText))
        );
        // #5199: the message fallback reads a subject-leading key. The
        // fixtures say `PROJ-2 seen` rather than `sees PROJ-2` for that
        // reason; which of the two sources wins is what this test covers.
        assert_eq!(
            ticket_key(Some("  "), "PROJ-2 seen", None),
            Some(("PROJ-2".into(), TicketSource::CommitText))
        );
        assert_eq!(
            ticket_key(None, "PROJ-2 seen", None),
            Some(("PROJ-2".into(), TicketSource::CommitText))
        );
        assert_eq!(ticket_key(None, "chore: nothing", None), None);
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

    // ── #5734: branch names and PR bodies as correlation inputs ───────────

    /// Why: #5734's first closure condition — a branch name must reach
    /// `commit_work_items` for a commit whose own text declares nothing.
    /// What: a commit with no key, carried by a PR whose head ref names one,
    /// links; the harvest is counted under `from_branch`.
    /// Test: this test itself.
    #[test]
    fn branch_name_supplies_a_key_the_subject_lacks() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "chore: tidy up", None);
        insert_pr(db.connection(), 1, "aaa", Some("feature/PROJ-1-tidy"), None);
        upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("upsert");

        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(out.linked, 1, "the branch name carried the key");
        assert_eq!(out.from_branch, 1);
        assert_eq!(out.from_pr_body, 0);
        assert_eq!(out.no_ticket, 0);
    }

    /// Why: #5734 — measured on this repository, a PR body's `Closes #N`
    /// recovers a key for 51 merged commits whose subject declares none. This
    /// is the source that actually earns its keep here.
    /// What: a commit with no key, carried by a PR whose body named an issue,
    /// links; the harvest is counted under `from_pr_body`.
    /// Test: this test itself.
    #[test]
    fn pr_body_supplies_a_key_the_subject_lacks() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "chore: tidy up", None);
        insert_pr(db.connection(), 1, "aaa", None, Some("#5089"));
        upsert_work_item(db.connection(), &work_item("#5089", "github")).expect("upsert");

        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(out.linked, 1);
        assert_eq!(out.from_pr_body, 1);
        assert_eq!(out.from_branch, 0);
    }

    /// Why: #5734 — 323 of this repository's merged commits have a subject key
    /// and a PR-body key that DIFFER, so which source wins is not academic.
    /// #5199 made subject position authoritative and a new source must not
    /// displace it.
    /// What: with all three sources disagreeing, the commit's own text wins;
    /// remove it and the branch wins; remove that and the body wins.
    /// Test: this test itself.
    #[test]
    fn precedence_prefers_commit_text_then_branch_then_body() {
        let pr = PrKeys {
            branch: Some("BRANCH-2".into()),
            body: Some("#3".into()),
        };
        assert_eq!(
            ticket_key(Some("SUBJ-1"), "SUBJ-1 x", Some(&pr)),
            Some(("SUBJ-1".into(), TicketSource::CommitText)),
            "the commit's own text outranks both pull-request sources"
        );
        assert_eq!(
            ticket_key(None, "chore: nothing", Some(&pr)),
            Some(("BRANCH-2".into(), TicketSource::BranchName)),
            "a deliberate branch identifier outranks PR prose"
        );
        let body_only = PrKeys {
            branch: None,
            body: Some("#3".into()),
        };
        assert_eq!(
            ticket_key(None, "chore: nothing", Some(&body_only)),
            Some(("#3".into(), TicketSource::PrBody))
        );
        assert_eq!(ticket_key(None, "chore: nothing", None), None);
    }

    /// Why: the precedence must be readable in one place; a constant that
    /// drifted from the enum's declaration order would silently reorder it.
    /// What: `SOURCE_ORDER` is sorted, so it agrees with `derive(Ord)`.
    /// Test: this test itself.
    #[test]
    fn precedence_order_matches_the_declared_variant_order() {
        let mut sorted = SOURCE_ORDER;
        sorted.sort();
        assert_eq!(SOURCE_ORDER, sorted);
        assert!(TicketSource::CommitText < TicketSource::BranchName);
        assert!(TicketSource::BranchName < TicketSource::PrBody);
    }

    /// Why: #5734's central risk — a branch named `fix/ADR-0029-followup` is
    /// exactly what #5199 spent its effort excluding, and harvesting it here
    /// would undo that work through a side door. `correlate_commits` is where
    /// a bad key becomes an operator-facing `no_work_item` number.
    /// What: neither an ADR-citing branch nor a body full of `DOC-`/`ADR-`
    /// prose produces a key, so neither becomes a phantom coverage gap.
    /// Test: this test itself.
    #[test]
    fn branch_and_body_noise_never_becomes_a_phantom_coverage_gap() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "chore: tidy up", None);
        insert_pr(
            db.connection(),
            1,
            "aaa",
            Some("fix/ADR-0029-followup"),
            None,
        );
        insert_commit(db.connection(), "bbb", "chore: tidy more", None);
        // A body whose only JIRA-shaped tokens are documentation citations
        // extracts no key at fetch time, so `body_ticket_id` is NULL.
        insert_pr(db.connection(), 2, "bbb", Some("fix/5734-slug"), None);

        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(out.no_ticket, 2, "neither commit gains a key");
        assert_eq!(out.from_branch, 0);
        assert_eq!(out.no_work_item, 0, "no phantom gap is reported");
    }

    /// Why: #5734 — a source that harvests nothing must say so. Silence is
    /// indistinguishable from a source that was never consulted, which is the
    /// fail-open this pass has to avoid.
    /// What: with no PR rows at all, the summary still names both counts as 0.
    /// Test: this test itself.
    #[test]
    fn summary_reports_a_zero_harvest_rather_than_omitting_it() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "chore: nothing", None);
        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(out.from_branch, 0);
        assert_eq!(out.from_pr_body, 0);
        let s = out.summary();
        assert!(s.contains("0 via branch"), "summary was: {s}");
        assert!(s.contains("0 via PR body"), "summary was: {s}");
    }

    /// Why: `pull_requests.commit_shas` is provider-written, and #5734 now
    /// parses it during correlation. One malformed row must not cost the whole
    /// pass — but it must not fail the query silently either.
    /// What: a PR with unparseable `commit_shas` is skipped and the other
    /// commit still correlates.
    /// Test: this test itself.
    #[test]
    fn a_malformed_commit_shas_column_skips_one_pr_not_the_pass() {
        let mut db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "chore: tidy", None);
        insert_commit(db.connection(), "bbb", "chore: tidy", None);
        db.connection()
            .execute(
                "INSERT INTO pull_requests \
                 (provider, repository, pr_number, title, author, state, created_at, \
                  commit_shas, head_ref) \
                 VALUES ('github','acme/w',1,'T','ada','merged','2026-01-01T00:00:00Z', \
                         'not json', 'feature/PROJ-1-x')",
                [],
            )
            .expect("insert malformed pr");
        insert_pr(db.connection(), 2, "bbb", Some("feature/PROJ-1-y"), None);
        upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("upsert");

        let out = correlate_commits(db.connection_mut(), &ProgressBus::disabled()).expect("run");
        assert_eq!(out.linked, 1, "the well-formed PR still correlates");
        assert_eq!(out.from_branch, 1);
        assert_eq!(out.no_ticket, 1, "the malformed PR's commit gains nothing");
    }
}
