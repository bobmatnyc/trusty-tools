//! Read views over the existing `work_items` / `commit_work_items` schema.
//!
//! Why: #5197's TUI answers two questions from data already in the database —
//! which commits linked to which board items, and which did not. Both are pure
//! SQL: no model, no credentials, no network, so the view is fully useful with
//! nothing configured.
//! What: [`correlation_counts`] for the header roll-up, [`correlation_rows`]
//! for the scrollable list, and [`CorrelationFilter`] for the all / linked /
//! unlinked toggle. Read-only — the linking pass that WRITES these rows lives
//! in [`crate::collect::correlate`], which is where the ticket extractor it
//! depends on belongs in the module layering.
//! Test: `tests` at the bottom of this file.

use rusqlite::{params, Connection};

use crate::core::errors::{Result, TgaError};

/// Aggregate counts for the correlation results view.
///
/// Why: the header line must state coverage in one glance, and "what did not
/// link" is only meaningful next to "what could have".
/// What: commit totals split by whether a ticket key was extracted and whether
/// a board item is linked. `#[non_exhaustive]` so later columns are additive.
/// Test: `tests::counts_split_linked_and_unlinked`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub struct CorrelationCounts {
    /// Every row in `commits`.
    pub commits: u64,
    /// Commits with at least one `commit_work_items` link.
    pub linked: u64,
    /// Commits carrying a `ticket_id` but no link — the actionable gap.
    pub ticketed_unlinked: u64,
    /// Commits with neither a ticket reference nor a link.
    pub unticketed: u64,
    /// Rows in `work_items`.
    pub work_items: u64,
    /// Work items referenced by at least one commit.
    pub work_items_linked: u64,
}

impl CorrelationCounts {
    /// Construct a count set.
    ///
    /// Why: the struct is `#[non_exhaustive]` so later columns stay additive
    /// for the published crate, which means downstream code — including the
    /// `tga` binary's own tests — cannot use a struct literal. This is the
    /// supported way in.
    /// What: positional constructor in field order.
    /// Test: `tests::counts_split_linked_and_unlinked`.
    pub fn new(
        commits: u64,
        linked: u64,
        ticketed_unlinked: u64,
        unticketed: u64,
        work_items: u64,
        work_items_linked: u64,
    ) -> Self {
        Self {
            commits,
            linked,
            ticketed_unlinked,
            unticketed,
            work_items,
            work_items_linked,
        }
    }

    /// Commits with no board-item link, ticketed or not.
    ///
    /// Why/What/Test: `commits - linked`; see
    /// `tests::counts_split_linked_and_unlinked`.
    pub fn unlinked(&self) -> u64 {
        self.commits.saturating_sub(self.linked)
    }

    /// Share of commits that carry a link, in `0.0..=1.0`.
    ///
    /// Why/What/Test: `0.0` on an empty database rather than a NaN that would
    /// render as garbage; see `tests::counts_split_linked_and_unlinked`.
    pub fn linked_fraction(&self) -> f64 {
        if self.commits == 0 {
            0.0
        } else {
            self.linked as f64 / self.commits as f64
        }
    }
}

/// One row of the correlation results view.
///
/// Why: the view is per commit — the operator asks "did THIS commit reach the
/// board?", so the linked items hang off the commit rather than the reverse.
/// What: commit identity, the extracted ticket key if any, and every linked
/// board item as `(source, id, title)`.
/// Test: `tests::rows_report_links_and_gaps`.
#[derive(Debug, Clone, PartialEq, Eq)]
#[non_exhaustive]
pub struct CorrelationRow {
    /// Commit SHA.
    pub sha: String,
    /// Repository the commit was collected from.
    pub repository: String,
    /// First line of the commit message.
    pub subject: String,
    /// The ticket key stored on the commit, when collection extracted one.
    pub ticket_id: Option<String>,
    /// Linked board items as `(source, id, title)`, ordered by source then id.
    pub work_items: Vec<(String, String, String)>,
}

impl CorrelationRow {
    /// Whether this commit reached the board.
    ///
    /// Why/What/Test: see `tests::rows_report_links_and_gaps`.
    pub fn is_linked(&self) -> bool {
        !self.work_items.is_empty()
    }
}

/// Which slice of commits a results view should show.
///
/// Why: "what did not link" is the question worth asking most often, so it
/// gets a first-class filter rather than a client-side scan.
/// What: `All`, `Linked`, or `Unlinked`. `#[non_exhaustive]` for later filters.
/// Test: `tests::rows_respect_filter`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
#[non_exhaustive]
pub enum CorrelationFilter {
    /// Every commit.
    #[default]
    All,
    /// Only commits with at least one board-item link.
    Linked,
    /// Only commits with no board-item link.
    Unlinked,
}

impl CorrelationFilter {
    /// Cycle to the next filter.
    ///
    /// Why: one key toggles the view; `All → Linked → Unlinked → All`.
    /// What: see above.
    /// Test: `tests::filter_cycles`.
    pub fn next(self) -> Self {
        match self {
            Self::All => Self::Linked,
            Self::Linked => Self::Unlinked,
            Self::Unlinked => Self::All,
        }
    }

    /// Short display label.
    ///
    /// Why/What/Test: see `tests::filter_cycles`.
    pub fn label(self) -> &'static str {
        match self {
            Self::All => "all",
            Self::Linked => "linked",
            Self::Unlinked => "unlinked",
        }
    }
}

/// Count commits and work items by link status.
///
/// Why: the results view's header, and the only cheap way to state coverage
/// over a corpus too large to enumerate.
/// What: five aggregate queries against `commits`, `work_items`, and
/// `commit_work_items`. Read-only.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] when any query fails.
pub fn correlation_counts(conn: &Connection) -> Result<CorrelationCounts> {
    let scalar = |sql: &str| -> Result<u64> {
        conn.query_row(sql, [], |r| r.get::<_, i64>(0))
            .map(|v| v.max(0) as u64)
            .map_err(TgaError::from)
    };
    let commits = scalar("SELECT COUNT(*) FROM commits")?;
    let linked = scalar(
        "SELECT COUNT(*) FROM commits c \
         WHERE EXISTS (SELECT 1 FROM commit_work_items w WHERE w.commit_sha = c.sha)",
    )?;
    let ticketed_unlinked = scalar(
        "SELECT COUNT(*) FROM commits c \
         WHERE c.ticket_id IS NOT NULL AND TRIM(c.ticket_id) <> '' \
           AND NOT EXISTS (SELECT 1 FROM commit_work_items w WHERE w.commit_sha = c.sha)",
    )?;
    let unticketed = scalar(
        "SELECT COUNT(*) FROM commits c \
         WHERE (c.ticket_id IS NULL OR TRIM(c.ticket_id) = '') \
           AND NOT EXISTS (SELECT 1 FROM commit_work_items w WHERE w.commit_sha = c.sha)",
    )?;
    let work_items = scalar("SELECT COUNT(*) FROM work_items")?;
    let work_items_linked = scalar(
        "SELECT COUNT(*) FROM work_items i \
         WHERE EXISTS (SELECT 1 FROM commit_work_items w \
                       WHERE w.work_item_id = i.id AND w.work_item_source = i.source)",
    )?;
    Ok(CorrelationCounts::new(
        commits,
        linked,
        ticketed_unlinked,
        unticketed,
        work_items,
        work_items_linked,
    ))
}

/// Load up to `limit` correlation rows, newest commit first.
///
/// Why: the results view is a scrollable list, and a full corpus does not fit
/// in a terminal or in memory.
/// What: one query for the commits matching `filter`, then one grouped query
/// for their links. Read-only.
///
/// # Errors
///
/// Returns [`TgaError::DbError`] when a query or row decode fails.
pub fn correlation_rows(
    conn: &Connection,
    filter: CorrelationFilter,
    limit: usize,
) -> Result<Vec<CorrelationRow>> {
    let predicate = match filter {
        CorrelationFilter::All => "1 = 1",
        CorrelationFilter::Linked => {
            "EXISTS (SELECT 1 FROM commit_work_items w WHERE w.commit_sha = c.sha)"
        }
        CorrelationFilter::Unlinked => {
            "NOT EXISTS (SELECT 1 FROM commit_work_items w WHERE w.commit_sha = c.sha)"
        }
    };
    let sql = format!(
        "SELECT c.sha, c.repository, c.message, c.ticket_id \
         FROM commits c WHERE {predicate} \
         ORDER BY c.timestamp DESC, c.sha LIMIT ?1"
    );
    let mut stmt = conn.prepare(&sql).map_err(TgaError::from)?;
    let mut rows: Vec<CorrelationRow> = Vec::new();
    let mapped = stmt
        .query_map(params![limit as i64], |row| {
            let message: String = row.get(2)?;
            Ok(CorrelationRow {
                sha: row.get(0)?,
                repository: row.get(1)?,
                subject: message.lines().next().unwrap_or_default().to_string(),
                ticket_id: row.get(3)?,
                work_items: Vec::new(),
            })
        })
        .map_err(TgaError::from)?;
    for r in mapped {
        rows.push(r.map_err(TgaError::from)?);
    }

    let mut items = conn
        .prepare(
            "SELECT i.source, i.id, i.title FROM work_items i \
             JOIN commit_work_items w \
               ON w.work_item_id = i.id AND w.work_item_source = i.source \
             WHERE w.commit_sha = ?1 ORDER BY i.source, i.id",
        )
        .map_err(TgaError::from)?;
    for row in &mut rows {
        let mapped = items
            .query_map(params![row.sha], |r| {
                Ok((r.get::<_, String>(0)?, r.get(1)?, r.get(2)?))
            })
            .map_err(TgaError::from)?;
        for it in mapped {
            row.work_items.push(it.map_err(TgaError::from)?);
        }
    }
    Ok(rows)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::core::db::work_items::{link_commit_work_item, upsert_work_item, WorkItemRow};
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
    fn counts_split_linked_and_unlinked() {
        let db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "PROJ-1 do a thing", Some("PROJ-1"));
        insert_commit(db.connection(), "bbb", "chore: tidy", None);
        upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("upsert");

        let before = correlation_counts(db.connection()).expect("counts");
        assert_eq!(before.commits, 2);
        assert_eq!(before.linked, 0);
        assert_eq!(before.ticketed_unlinked, 1);
        assert_eq!(before.unticketed, 1);
        assert_eq!(before.unlinked(), 2);
        assert_eq!(before.work_items, 1);
        assert_eq!(before.work_items_linked, 0);
        assert_eq!(before.linked_fraction(), 0.0);

        link_commit_work_item(db.connection(), "aaa", "PROJ-1", "jira").expect("link");

        let after = correlation_counts(db.connection()).expect("counts");
        assert_eq!(after.linked, 1);
        assert_eq!(after.ticketed_unlinked, 0);
        assert_eq!(after.unlinked(), 1);
        assert_eq!(after.work_items_linked, 1);
        assert!((after.linked_fraction() - 0.5).abs() < f64::EPSILON);
    }

    #[test]
    fn empty_database_reports_zeroes_not_nan() {
        let db = Database::open_in_memory().expect("open");
        let c = correlation_counts(db.connection()).expect("counts");
        assert_eq!(c, CorrelationCounts::default());
        assert_eq!(c.linked_fraction(), 0.0);
    }

    #[test]
    fn rows_report_links_and_gaps() {
        let db = Database::open_in_memory().expect("open");
        insert_commit(
            db.connection(),
            "aaa",
            "PROJ-1 subject\n\nbody text",
            Some("PROJ-1"),
        );
        insert_commit(db.connection(), "bbb", "chore: tidy", None);
        upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("upsert");
        link_commit_work_item(db.connection(), "aaa", "PROJ-1", "jira").expect("link");

        let rows = correlation_rows(db.connection(), CorrelationFilter::All, 10).expect("rows");
        assert_eq!(rows.len(), 2);
        let linked: Vec<&CorrelationRow> = rows.iter().filter(|r| r.is_linked()).collect();
        assert_eq!(linked.len(), 1);
        assert_eq!(linked[0].sha, "aaa");
        assert_eq!(
            linked[0].subject, "PROJ-1 subject",
            "subject is line 1 only"
        );
        assert_eq!(linked[0].repository, "repo");
        assert_eq!(linked[0].ticket_id.as_deref(), Some("PROJ-1"));
        assert_eq!(linked[0].work_items[0].0, "jira");
        assert_eq!(linked[0].work_items[0].2, "Item PROJ-1");
        let unlinked: Vec<&CorrelationRow> = rows.iter().filter(|r| !r.is_linked()).collect();
        assert_eq!(unlinked[0].sha, "bbb");
        assert!(unlinked[0].ticket_id.is_none());
    }

    #[test]
    fn rows_list_every_source_for_one_commit() {
        let db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "PROJ-1 x", Some("PROJ-1"));
        upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("jira");
        upsert_work_item(db.connection(), &work_item("PROJ-1", "linear")).expect("linear");
        link_commit_work_item(db.connection(), "aaa", "PROJ-1", "jira").expect("link jira");
        link_commit_work_item(db.connection(), "aaa", "PROJ-1", "linear").expect("link linear");

        let rows = correlation_rows(db.connection(), CorrelationFilter::Linked, 10).expect("rows");
        assert_eq!(rows.len(), 1);
        assert_eq!(
            rows[0]
                .work_items
                .iter()
                .map(|(s, _, _)| s.as_str())
                .collect::<Vec<_>>(),
            vec!["jira", "linear"]
        );
    }

    #[test]
    fn rows_respect_filter() {
        let db = Database::open_in_memory().expect("open");
        insert_commit(db.connection(), "aaa", "PROJ-1 x", Some("PROJ-1"));
        insert_commit(db.connection(), "bbb", "chore: tidy", None);
        upsert_work_item(db.connection(), &work_item("PROJ-1", "jira")).expect("upsert");
        link_commit_work_item(db.connection(), "aaa", "PROJ-1", "jira").expect("link");

        let all = correlation_rows(db.connection(), CorrelationFilter::All, 10).expect("all");
        let linked =
            correlation_rows(db.connection(), CorrelationFilter::Linked, 10).expect("linked");
        let unlinked =
            correlation_rows(db.connection(), CorrelationFilter::Unlinked, 10).expect("unlinked");
        assert_eq!(all.len(), 2);
        assert_eq!(linked.len(), 1);
        assert_eq!(unlinked.len(), 1);
        assert_eq!(unlinked[0].sha, "bbb");
    }

    #[test]
    fn rows_respect_limit() {
        let db = Database::open_in_memory().expect("open");
        for i in 0..5 {
            insert_commit(db.connection(), &format!("sha{i}"), "chore: x", None);
        }
        let rows = correlation_rows(db.connection(), CorrelationFilter::All, 2).expect("rows");
        assert_eq!(rows.len(), 2);
    }

    #[test]
    fn filter_cycles() {
        assert_eq!(CorrelationFilter::default(), CorrelationFilter::All);
        assert_eq!(CorrelationFilter::All.next(), CorrelationFilter::Linked);
        assert_eq!(
            CorrelationFilter::Linked.next(),
            CorrelationFilter::Unlinked
        );
        assert_eq!(CorrelationFilter::Unlinked.next(), CorrelationFilter::All);
        assert_eq!(CorrelationFilter::All.label(), "all");
        assert_eq!(CorrelationFilter::Linked.label(), "linked");
        assert_eq!(CorrelationFilter::Unlinked.label(), "unlinked");
    }
}
