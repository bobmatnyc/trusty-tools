//! Flag backfill operations: revert, ticket, ticketed, ai-detection.
//!
//! Why: these four operations share the same pattern — scan `commits.message`,
//! compute a new value, diff against the stored value, and batch-UPDATE changed
//! rows. Grouping them here keeps the effort and misc modules focused and
//! makes the shared `build_commits_filter_sql` helper easy to find.

use rusqlite::params;
use tga::collect::ai_attribution::detect_ai_tool;
use tga::collect::ticket::{extract_ticket_id, is_ticketed};
use tga::core::db::Database;

/// Build a SQL fragment and bind params for the common backfill filters.
///
/// Why: revert-flags and ticket-ids both need `WHERE` clauses for repos,
/// since, and until; extracting this avoids duplicating the SQL-building
/// logic in each function.
/// What: given a base SELECT (ending before any WHERE clause), appends
/// predicates for `repository IN (…)`, `timestamp >= ?`, `timestamp <= ?`
/// as needed, returning the assembled SQL string and bound values.
/// Test: exercised indirectly by backfill filter tests.
pub(super) fn build_commits_filter_sql(
    base_sql: &str,
    repos: &[String],
    since: Option<&str>,
    until: Option<&str>,
) -> (String, Vec<rusqlite::types::Value>) {
    use rusqlite::types::Value;
    let mut predicates: Vec<String> = Vec::new();
    let mut params: Vec<Value> = Vec::new();

    if !repos.is_empty() {
        let start = params.len() + 1;
        for r in repos {
            params.push(Value::Text(r.clone()));
        }
        let end = params.len();
        let placeholders: Vec<String> = (start..=end).map(|i| format!("?{i}")).collect();
        predicates.push(format!("repository IN ({})", placeholders.join(", ")));
    }
    if let Some(s) = since {
        params.push(Value::Text(s.to_string()));
        predicates.push(format!("timestamp >= ?{}", params.len()));
    }
    if let Some(u) = until {
        params.push(Value::Text(u.to_string()));
        predicates.push(format!("timestamp <= ?{}", params.len()));
    }

    let sql = if predicates.is_empty() {
        base_sql.to_string()
    } else {
        format!("{base_sql} WHERE {}", predicates.join(" AND "))
    };
    (sql, params)
}

/// Detect if a commit message looks like a revert.
///
/// Why: the `commits.is_revert` column written by `tga backfill is-revert`
/// must agree with the revert rate the report computes from the same commit
/// messages. Issue #377 unified both paths onto
/// [`tga::core::revert::is_revert`]; this thin wrapper preserves the
/// existing call sites while delegating to the single source of truth.
/// What: forwards to [`tga::core::revert::is_revert`], which catches
/// `Revert "..."`, `revert:`, `revert(scope):`, `^revert`, and `^fix.*revert`
/// (case-insensitive, first-line only).
/// Test: `tests::revert_detector_matches_expected_forms` in `tests.rs`, plus the
/// canonical coverage in `crate::core::revert::tests`.
pub(super) fn is_revert(message: &str) -> bool {
    tga::core::revert::is_revert(message)
}

/// Scan every commit message for revert patterns and update `is_revert`.
///
/// Why: the `is_revert` boolean must mirror the verdict produced by the
/// classification cascade so DORA queries (CFR, MTTR) can join through it.
/// What: scans `commits` (filtered by repos/since/until when supplied),
/// detects revert prefixes, and updates changed rows. Supports dry-run.
/// Test: see `tests::backfill_revert_flags_updates_only_changed_rows`.
pub(super) fn backfill_revert_flags(
    db: &mut Database,
    dry_run: bool,
    repos_filter: &[String],
    since: Option<&str>,
    until: Option<&str>,
) -> anyhow::Result<()> {
    let mut to_update: Vec<(i64, bool)> = Vec::new();
    {
        let conn = db.connection();
        // Build filtered SQL for repos/since/until.
        let (sql, params) = build_commits_filter_sql(
            "SELECT id, message, is_revert FROM commits",
            repos_filter,
            since,
            until,
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for r in rows {
            let (id, message, current) = r?;
            let detected = is_revert(&message);
            let target = if detected { 1 } else { 0 };
            if target != current {
                to_update.push((id, detected));
            }
        }
    }

    if dry_run {
        println!(
            "Would update {} commits ({} would be marked as reverts). No changes written.",
            to_update.len(),
            to_update.iter().filter(|(_, v)| *v).count(),
        );
        return Ok(());
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut up = tx.prepare("UPDATE commits SET is_revert = ?1 WHERE id = ?2")?;
        for (id, flag) in &to_update {
            up.execute(params![if *flag { 1 } else { 0 }, id])?;
        }
    }
    tx.commit()?;
    println!(
        "Updated is_revert on {} commits ({} are reverts).",
        to_update.len(),
        to_update.iter().filter(|(_, v)| *v).count(),
    );
    Ok(())
}

/// Scan every commit message, extract the first ticket reference, and
/// update `ticket_id` + `ticketed`.
///
/// Why: ticket extraction patterns evolve; backfilling lets operators
/// update the DB after extending patterns without re-collecting.
/// What: scans `commits` (filtered by repos/since/until when supplied),
/// extracts ticket IDs, and updates changed rows.
/// Test: see `tests::backfill_ticket_ids_populates_ticket_id`.
pub(super) fn backfill_ticket_ids(
    db: &mut Database,
    dry_run: bool,
    repos_filter: &[String],
    since: Option<&str>,
    until: Option<&str>,
) -> anyhow::Result<()> {
    let mut to_update: Vec<(i64, Option<String>, i64)> = Vec::new();
    {
        let conn = db.connection();
        let (sql, params) = build_commits_filter_sql(
            "SELECT id, message, ticket_id, ticketed FROM commits",
            repos_filter,
            since,
            until,
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })?;
        for r in rows {
            let (id, message, current_id, current_ticketed) = r?;
            let extracted = extract_ticket_id(&message);
            let ticketed = if is_ticketed(&message) { 1 } else { 0 };
            if extracted != current_id || ticketed != current_ticketed {
                to_update.push((id, extracted, ticketed));
            }
        }
    }

    if dry_run {
        let with_id = to_update.iter().filter(|(_, id, _)| id.is_some()).count();
        println!(
            "Would update {} commits ({} would gain a ticket_id). No changes written.",
            to_update.len(),
            with_id,
        );
        return Ok(());
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut up =
            tx.prepare("UPDATE commits SET ticket_id = ?1, ticketed = ?2 WHERE id = ?3")?;
        for (id, ticket, ticketed) in &to_update {
            up.execute(params![ticket, ticketed, id])?;
        }
    }
    tx.commit()?;
    let with_id = to_update.iter().filter(|(_, id, _)| id.is_some()).count();
    println!(
        "Updated {} commits ({} now have a ticket_id).",
        to_update.len(),
        with_id,
    );
    Ok(())
}

/// Recompute `commits.ticketed` using the corrected `is_ticketed` logic.
///
/// Why: before issue #445 the `gh_bare` pattern (`#N` preceded by whitespace)
/// was included in [`is_ticketed`], inflating the ticketed rate to ~100%.
/// After the fix, bare `#N` no longer marks a commit as ticketed. This
/// backfill lets operators correct existing rows without re-collecting.
/// What: loads every commit (filtered by repos/since/until), recomputes
/// `ticketed` from `commits.message` using the fixed `is_ticketed`, and
/// updates rows whose stored value differs. No LLM required — pure regex.
/// Test: `tests::backfill_ticketed_corrects_bare_hash_rows`.
///
/// # Errors
///
/// Propagates database errors from the underlying queries.
pub(super) fn backfill_ticketed(
    db: &mut Database,
    dry_run: bool,
    repos_filter: &[String],
    since: Option<&str>,
    until: Option<&str>,
) -> anyhow::Result<()> {
    let mut to_update: Vec<(i64, i64)> = Vec::new();
    {
        let conn = db.connection();
        let (sql, params) = build_commits_filter_sql(
            "SELECT id, message, ticketed FROM commits",
            repos_filter,
            since,
            until,
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows = stmt.query_map(rusqlite::params_from_iter(params.iter()), |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;
        for r in rows {
            let (id, message, current) = r?;
            let new_val = if is_ticketed(&message) { 1 } else { 0 };
            if new_val != current {
                to_update.push((id, new_val));
            }
        }
    }

    let now_ticketed = to_update.iter().filter(|(_, v)| *v == 1).count();
    let now_unticketed = to_update.iter().filter(|(_, v)| *v == 0).count();

    if dry_run {
        println!(
            "Dry run — would update {} commits \
             ({} newly ticketed, {} newly unticketed). No changes written.",
            to_update.len(),
            now_ticketed,
            now_unticketed,
        );
        return Ok(());
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut up = tx.prepare("UPDATE commits SET ticketed = ?1 WHERE id = ?2")?;
        for (id, val) in &to_update {
            up.execute(params![val, id])?;
        }
    }
    tx.commit()?;
    println!(
        "Updated ticketed on {} commits \
         ({} newly ticketed, {} newly unticketed).",
        to_update.len(),
        now_ticketed,
        now_unticketed,
    );
    Ok(())
}

/// Mark every commit whose existing classification was produced by the LLM
/// tier with a confidence below 0.7 as needing re-classification.
///
/// Why: clearing `classification_id` on low-confidence LLM verdicts enables the
/// next `tga classify` run to reprocess them with updated models or rules.
/// What: sets `classification_id = NULL, confidence = NULL` on commits that
/// match `method='llm' AND confidence < 0.7`. Supports dry-run.
/// Test: implicit via the classification pipeline integration tests.
pub(super) fn backfill_ai_detection(db: &mut Database, dry_run: bool) -> anyhow::Result<()> {
    let conn = db.connection();
    // Count first.
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM commits c \
             JOIN classifications cl ON c.classification_id = cl.id \
             WHERE cl.method = 'llm' AND COALESCE(c.confidence, cl.confidence) < 0.7",
            [],
            |row| row.get(0),
        )
        .unwrap_or(0);

    if dry_run {
        println!(
            "Would re-classify {count} commits (method='llm', confidence<0.7). No changes written."
        );
        return Ok(());
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    let n = tx.execute(
        "UPDATE commits SET classification_id = NULL, confidence = NULL \
         WHERE classification_id IN ( \
             SELECT id FROM classifications WHERE method = 'llm' \
         ) AND COALESCE(confidence, 0.0) < 0.7",
        [],
    )?;
    tx.commit()?;
    println!(
        "Cleared classification on {n} commits — next `tga classify` run will reprocess them."
    );
    Ok(())
}

/// Scan existing `commits.message` for AI co-authorship trailers.
///
/// Why: `is_ai_assisted` and `ai_tool` columns were added in migration v17;
/// existing rows have `is_ai_assisted = 0` and `ai_tool = NULL` regardless of
/// their actual history. This backfill retroactively detects Claude,
/// GitHub Copilot, and Cursor via `Co-Authored-By:` trailers.
/// What: loads every commit (filtered by repos/since/until), runs
/// [`detect_ai_tool`] on the message, and updates rows where `ai_tool`
/// differs from the stored value. No LLM required — pure string matching.
/// Test: `tests::backfill_ai_detection_commits_detects_claude`.
///
/// # Errors
///
/// Propagates database errors from the underlying queries.
pub(super) fn backfill_ai_detection_commits(
    db: &mut Database,
    dry_run: bool,
    repos_filter: &[String],
    since: Option<&str>,
    until: Option<&str>,
) -> anyhow::Result<()> {
    let mut to_update: Vec<(i64, i64, Option<&'static str>)> = Vec::new();
    {
        let conn = db.connection();
        let (sql, params) = build_commits_filter_sql(
            "SELECT id, message, ai_tool FROM commits",
            repos_filter,
            since,
            until,
        );
        let mut stmt = conn.prepare(&sql)?;
        let rows: Vec<(i64, String, Option<String>)> = stmt
            .query_map(rusqlite::params_from_iter(params.iter()), |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                ))
            })?
            .collect::<Result<_, _>>()?;

        for (id, message, current_tool) in rows {
            let detected = detect_ai_tool(&message);
            let current_str = current_tool.as_deref();
            if detected != current_str {
                let is_ai = if detected.is_some() { 1_i64 } else { 0_i64 };
                to_update.push((id, is_ai, detected));
            }
        }
    }

    let with_tool = to_update.iter().filter(|(_, _, t)| t.is_some()).count();

    if dry_run {
        println!(
            "Dry run — would update {} commits ({} with AI tool detected). No changes written.",
            to_update.len(),
            with_tool,
        );
        return Ok(());
    }

    let conn = db.connection_mut();
    let tx = conn.transaction()?;
    {
        let mut up =
            tx.prepare("UPDATE commits SET is_ai_assisted = ?1, ai_tool = ?2 WHERE id = ?3")?;
        for (id, is_ai, tool) in &to_update {
            up.execute(params![is_ai, tool, id])?;
        }
    }
    tx.commit()?;
    println!(
        "Updated {} commits ({} AI-assisted, {} cleared).",
        to_update.len(),
        with_tool,
        to_update.len() - with_tool,
    );
    Ok(())
}
