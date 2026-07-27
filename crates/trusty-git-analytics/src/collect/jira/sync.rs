//! Pure JQL-building and cursor-arithmetic helpers backing `tga jira sync`
//! (issue #3966).
//!
//! Kept free of HTTP/DB dependencies so pagination and incremental-cursor
//! logic can be unit tested without a mock server or database — the actual
//! orchestration (HTTP client + DB writes) lives in
//! `commands::jira::run_sync`.

use chrono::{DateTime, Utc};

/// The scope of one `tga jira sync` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncScope {
    /// JIRA project key to restrict the JQL to.
    pub project_key: String,
    /// Lower bound on `updated`, when known. `None` means "no lower bound"
    /// (full history — used for the first-ever sync of a project, or an
    /// explicit `--backfill` with no `--since`).
    pub since: Option<DateTime<Utc>>,
}

/// Format a UTC timestamp in JIRA's JQL date-time literal syntax
/// (`"yyyy-MM-dd HH:mm"`, minute resolution — JQL does not accept seconds).
///
/// Why minute resolution and not RFC3339: JQL's `date`/`datetime` literal
/// grammar is `"yyyy-MM-dd[ HH:mm]"`; passing a full RFC3339 string (with
/// seconds, fractional seconds, or a `T`/`Z` separator) is rejected by JIRA
/// with a 400. Truncating to the minute below the cursor is intentionally
/// conservative — it can very rarely re-fetch a ticket updated in the same
/// minute as the previous cursor, but upserts make that a no-op rather than
/// a correctness problem (never the reverse: never rounds *up* and skips a
/// ticket).
pub fn jql_date(dt: DateTime<Utc>) -> String {
    dt.format("%Y-%m-%d %H:%M").to_string()
}

/// Build the JQL query for one sync scope.
///
/// Always orders by `updated ASC` so the last issue returned in a run
/// determines the next cursor (issue's `updated` timestamp becomes the next
/// `since`).
pub fn build_jql(scope: &SyncScope) -> String {
    match scope.since {
        Some(since) => format!(
            "project = {} AND updated >= \"{}\" ORDER BY updated ASC",
            scope.project_key,
            jql_date(since)
        ),
        None => format!("project = {} ORDER BY updated ASC", scope.project_key),
    }
}

/// Resolve the effective sync scope from CLI/config inputs.
///
/// Precedence for the `since` bound:
/// 1. `--backfill` with no explicit `--since` → `None` (full history).
/// 2. Explicit `--since` (present regardless of `--backfill`) → that date.
/// 3. Stored cursor (`jira_sync_cursor.last_synced_at`) → that timestamp.
/// 4. Nothing stored and no flags → `None` (first-ever sync is a full
///    historical pull, satisfying the "backfill capability" acceptance
///    criterion without requiring `--backfill` to be passed explicitly).
pub fn resolve_scope(
    project_key: &str,
    explicit_since: Option<DateTime<Utc>>,
    backfill: bool,
    stored_cursor: Option<DateTime<Utc>>,
) -> SyncScope {
    let since = if let Some(since) = explicit_since {
        Some(since)
    } else if backfill {
        None
    } else {
        stored_cursor
    };
    SyncScope {
        project_key: project_key.to_string(),
        since,
    }
}

/// Compute the next incremental cursor from the `updated` timestamps of
/// tickets processed in a run.
///
/// Why: the cursor must advance to the maximum `updated` value actually
/// observed, not simply "now" — a lagging or throttled JIRA instance may
/// report `updated` timestamps behind wall-clock time, and using "now"
/// would create a gap where tickets updated between the true max and "now"
/// are silently skipped on the next incremental run.
///
/// Returns `None` when no tickets were processed (the caller should leave
/// the stored cursor untouched in that case, not regress it).
pub fn next_cursor(observed_updated: &[DateTime<Utc>]) -> Option<DateTime<Utc>> {
    observed_updated.iter().max().copied()
}

#[cfg(test)]
mod tests {
    use super::*;
    use chrono::TimeZone;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid rfc3339 fixture")
            .with_timezone(&Utc)
    }

    #[test]
    fn jql_date_formats_at_minute_resolution() {
        let d = Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap();
        assert_eq!(jql_date(d), "2026-03-04 05:06");
    }

    #[test]
    fn build_jql_without_since_omits_updated_clause() {
        let scope = SyncScope {
            project_key: "PROJ".into(),
            since: None,
        };
        assert_eq!(build_jql(&scope), "project = PROJ ORDER BY updated ASC");
    }

    #[test]
    fn build_jql_with_since_includes_updated_clause() {
        let scope = SyncScope {
            project_key: "PROJ".into(),
            since: Some(dt("2026-01-01T00:00:00Z")),
        };
        assert_eq!(
            build_jql(&scope),
            "project = PROJ AND updated >= \"2026-01-01 00:00\" ORDER BY updated ASC"
        );
    }

    #[test]
    fn resolve_scope_backfill_without_since_is_full_history() {
        let scope = resolve_scope("PROJ", None, true, Some(dt("2026-01-01T00:00:00Z")));
        assert_eq!(
            scope.since, None,
            "explicit --backfill must override the stored cursor"
        );
    }

    #[test]
    fn resolve_scope_explicit_since_wins_over_backfill_and_cursor() {
        let explicit = dt("2026-05-01T00:00:00Z");
        let scope = resolve_scope(
            "PROJ",
            Some(explicit),
            true,
            Some(dt("2026-01-01T00:00:00Z")),
        );
        assert_eq!(scope.since, Some(explicit));
    }

    #[test]
    fn resolve_scope_uses_stored_cursor_by_default() {
        let cursor = dt("2026-01-01T00:00:00Z");
        let scope = resolve_scope("PROJ", None, false, Some(cursor));
        assert_eq!(scope.since, Some(cursor));
    }

    #[test]
    fn resolve_scope_first_ever_sync_is_full_history() {
        // No explicit flags, no stored cursor — this is the "first run ever"
        // case and must default to a full historical pull.
        let scope = resolve_scope("PROJ", None, false, None);
        assert_eq!(scope.since, None);
    }

    #[test]
    fn next_cursor_returns_the_maximum_observed_timestamp() {
        let observed = vec![
            dt("2026-01-01T00:00:00Z"),
            dt("2026-03-01T00:00:00Z"),
            dt("2026-02-01T00:00:00Z"),
        ];
        assert_eq!(next_cursor(&observed), Some(dt("2026-03-01T00:00:00Z")));
    }

    #[test]
    fn next_cursor_returns_none_for_empty_batch() {
        assert_eq!(next_cursor(&[]), None);
    }
}
