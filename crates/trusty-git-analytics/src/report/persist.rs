//! Persistence helpers: UPSERT [`ReportData`] weekly slices into SQLite fact tables.
//!
//! Why: `aggregator.rs` was approaching its 500-line budget when the two persist
//! functions for `fact_weekly_quality` (issue #445) and `fact_weekly_engineer`
//! (issue #1113) were added. Extracting them here keeps each file focused and
//! within the project line-cap.
//! What: two public functions — [`persist_weekly_quality`] and
//! [`persist_weekly_engineer`] — each UPSERT one row per
//! [`crate::report::models::WeeklyActivity`] into the corresponding fact table.
//! Test: `report::tests::persist_weekly_quality_upserts_rows` and
//! `report::tests::persist_weekly_engineer_upserts_rows`.

use std::collections::HashMap;

use tracing::warn;

use crate::core::db::Database;
use crate::core::quality::QUALITY_FORMULA_VERSION;
use crate::report::errors::Result;
use crate::report::models::ReportData;

/// Parse an ISO week label `"YYYY-Www"` into `(iso_year, iso_week)`.
///
/// Why: fact tables store year/week as separate INTEGER columns so warehouse
/// tools can filter without string parsing.
/// What: splits on `-W`, parses both halves as i64.
/// Test: exercised indirectly by both persist functions below.
pub(super) fn parse_week_label_to_parts(label: &str) -> Option<(i64, i64)> {
    let (y, w) = label.split_once("-W")?;
    let year: i64 = y.parse().ok()?;
    let week: i64 = w.parse().ok()?;
    Some((year, week))
}

/// Build a display-name → canonical-email lookup from [`ReportData::authors`].
///
/// Why: [`crate::report::models::WeeklyActivity::author`] carries the display
/// name (resolved at materialisation); the fact tables need the canonical email
/// as the grain key so they join correctly with `fact_weekly_quality`.
/// What: returns a `HashMap<display_name, canonical_email>`.
/// Test: exercised by both persist functions.
fn name_to_email_map(data: &ReportData) -> HashMap<String, String> {
    data.authors
        .iter()
        .map(|a| (a.name.clone(), a.email.clone()))
        .collect()
}

/// Unix-epoch seconds for "now", used as `computed_at` in fact rows.
fn computed_at_secs() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_secs() as i64)
        .unwrap_or(0)
}

/// Persist per-engineer-per-week quality scores to `fact_weekly_quality`.
///
/// Why: downstream warehouses read `tga.db` directly; storing quality rows
/// avoids requiring consumers to re-implement the scoring formula in SQL.
/// This is called immediately after aggregation so stored values always
/// reflect the corrected ticketed logic from migration v17.
/// What: UPSERTs one row per [`crate::report::models::WeeklyActivity`] into
/// `fact_weekly_quality`, batching in chunks of 500. Rows whose ISO week label
/// cannot be parsed are skipped with a warning. Rows whose author display name
/// cannot be resolved to a canonical email are also skipped with a `warn!` — the
/// same policy as [`persist_weekly_engineer`]. This ensures both fact tables
/// share an identical grain key (author_email, iso_year, iso_week, repository)
/// so downstream joins between them are always consistent. Never write a display
/// name into the email-keyed column: doing so would produce rows that can never
/// join with other tables and would silently corrupt aggregate queries.
/// To fix unmapped identities run `tga aliases list` and add the missing mapping.
/// Test: `report::tests::persist_weekly_quality_upserts_rows`.
///
/// # Errors
///
/// Returns [`ReportError::Core`] if any SQLite operation fails.
pub fn persist_weekly_quality(db: &Database, data: &ReportData) -> Result<usize> {
    if data.weekly_activity.is_empty() {
        return Ok(0);
    }
    let computed_at = computed_at_secs();
    let name_to_email = name_to_email_map(data);

    let rows: Vec<_> =
        data.weekly_activity
            .iter()
            .filter_map(|wa| {
                let (iso_year, iso_week) = match parse_week_label_to_parts(&wa.week) {
                    Some(p) => p,
                    None => {
                        warn!(
                            week = %wa.week,
                            "persist_weekly_quality: cannot parse week label; skipping row"
                        );
                        return None;
                    }
                };
                let quality_tshirt: i64 = wa.quality_tshirt.parse().unwrap_or(
                    crate::core::quality::size_for_quality_score(wa.quality_score) as i64,
                );
                Some((
                    wa.author.clone(),
                    iso_year,
                    iso_week,
                    wa.repository.clone(),
                    wa.quality_score,
                    quality_tshirt,
                    wa.revert_count as i64,
                    wa.bugfix_count as i64,
                    wa.ticketed_count as i64,
                    wa.commit_count as i64,
                    computed_at,
                ))
            })
            .collect();

    let mut written = 0usize;
    for chunk in rows.chunks(500) {
        let conn = db.connection();
        let tx = conn
            .unchecked_transaction()
            .map_err(crate::core::TgaError::from)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO fact_weekly_quality \
                     (author_email, iso_year, iso_week, repository, quality_score, \
                      quality_tshirt, revert_count, bugfix_count, ticketed_count, \
                      commit_count, formula_version, computed_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                )
                .map_err(crate::core::TgaError::from)?;
            for (author_display, iso_year, iso_week, repo, qs, qt, rc, bc, tc, cc, ca) in chunk {
                // Resolve display name → canonical email.
                // POLICY: skip rows that cannot be resolved rather than
                // falling back to the display name. Both fact tables
                // (fact_weekly_quality and fact_weekly_engineer) share the
                // grain key (author_email, iso_year, iso_week, repository).
                // Writing a display name into the email-keyed column would
                // produce rows that never join with the other table and
                // silently corrupt downstream aggregates. Run `tga aliases
                // list` to review and add unmapped identities.
                let author_email = match name_to_email.get(author_display) {
                    Some(e) => e.clone(),
                    None => {
                        warn!(
                            author = %author_display,
                            iso_year = iso_year,
                            iso_week = iso_week,
                            "persist_weekly_quality: no email mapping for author; \
                             skipping row to avoid corrupting the grain key. \
                             Run `tga aliases list` to review unmapped identities."
                        );
                        continue;
                    }
                };
                stmt.execute(rusqlite::params![
                    author_email,
                    iso_year,
                    iso_week,
                    repo,
                    qs,
                    qt,
                    rc,
                    bc,
                    tc,
                    cc,
                    QUALITY_FORMULA_VERSION,
                    ca,
                ])
                .map_err(crate::core::TgaError::from)?;
                written += 1;
            }
        }
        tx.commit().map_err(crate::core::TgaError::from)?;
    }
    Ok(written)
}

/// Persist per-engineer-per-week agentic counts to `fact_weekly_engineer`.
///
/// Why: downstream warehouses (cto-reports) need agentic % per engineer per
/// ISO week without re-running the aggregator (issue #1113). Mirrors the
/// `fact_weekly_quality` pattern from issue #445 batch B.
/// What: UPSERTs one row per [`crate::report::models::WeeklyActivity`] into
/// `fact_weekly_engineer`. `net_commits` = `commit_count - revert_count`;
/// merge commits are **included** in the denominator (only reverts are
/// subtracted), per the #1113 spec. `agentic_pct` = `agentic_count / net *
/// 100` (full-agentic only — excludes `ide_assisted_count`). Rows with
/// unresolvable author emails are skipped with a `warn!` to preserve
/// grain-key integrity.
/// Test: `report::tests::persist_weekly_engineer_upserts_rows`.
///
/// # Errors
///
/// Returns [`ReportError::Core`] if any SQLite operation fails.
pub fn persist_weekly_engineer(db: &Database, data: &ReportData) -> Result<usize> {
    if data.weekly_activity.is_empty() {
        return Ok(0);
    }
    let computed_at = computed_at_secs();
    let name_to_email = name_to_email_map(data);

    let rows: Vec<_> = data
        .weekly_activity
        .iter()
        .filter_map(|wa| {
            let (iso_year, iso_week) = parse_week_label_to_parts(&wa.week)?;
            // net_commits = commit_count - revert_count. Merge commits are
            // intentionally INCLUDED in this denominator (per #1113 spec) —
            // only reverts are subtracted. Future specs that wish to also
            // exclude merge commits must update both this formula and the
            // column description in the DB schema docs.
            let net = wa.commit_count.saturating_sub(wa.revert_count) as i64;
            // agentic_pct = agentic_count / net * 100 — intentionally
            // EXCLUDES ide_assisted_count (full-agentic only, per #1113 spec).
            // ide_assisted is tracked separately and must NOT inflate the numerator.
            let agentic_pct = if net > 0 {
                (wa.agentic_count as f64) / (net as f64) * 100.0
            } else {
                0.0
            };
            Some((
                wa.author.clone(),
                iso_year,
                iso_week,
                wa.repository.clone(),
                net,
                wa.agentic_count as i64,
                wa.ide_assisted_count as i64,
                agentic_pct,
                computed_at,
            ))
        })
        .collect();

    let mut written = 0usize;
    for chunk in rows.chunks(500) {
        let conn = db.connection();
        let tx = conn
            .unchecked_transaction()
            .map_err(crate::core::TgaError::from)?;
        {
            let mut stmt = tx
                .prepare(
                    "INSERT OR REPLACE INTO fact_weekly_engineer \
                     (author_email, iso_year, iso_week, repository, \
                      net_commits, agentic_count, ide_assisted_count, agentic_pct, \
                      formula_version, computed_at) \
                     VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                )
                .map_err(crate::core::TgaError::from)?;
            for (author_display, iso_year, iso_week, repo, net, ac, ic, pct, ca) in chunk {
                // Resolve display name → canonical email for the grain key.
                // Skip rows that cannot be resolved: persisting a display
                // name as author_email corrupts the grain key and breaks
                // joins with fact_weekly_quality (issue #1113).
                let author_email = match name_to_email.get(author_display) {
                    Some(e) => e.clone(),
                    None => {
                        warn!(
                            author = %author_display,
                            iso_year = iso_year,
                            iso_week = iso_week,
                            "persist_weekly_engineer: no email mapping for author; \
                             skipping row to avoid corrupting the grain key. \
                             Run `tga aliases list` to review unmapped identities."
                        );
                        continue;
                    }
                };
                stmt.execute(rusqlite::params![
                    author_email,
                    iso_year,
                    iso_week,
                    repo,
                    net,
                    ac,
                    ic,
                    pct,
                    "v1",
                    ca,
                ])
                .map_err(crate::core::TgaError::from)?;
                written += 1;
            }
        }
        tx.commit().map_err(crate::core::TgaError::from)?;
    }
    Ok(written)
}
