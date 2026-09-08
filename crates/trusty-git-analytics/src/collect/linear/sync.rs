//! Pure scope-resolution and filter-building helpers backing `tga linear
//! sync` (issue #7139).
//!
//! Why: kept free of HTTP/DB dependencies, mirroring
//! `collect::jira::sync`, so cursor arithmetic and GraphQL filter shape are
//! unit-testable without a mock server. The actual orchestration (HTTP +
//! DB writes) lives in `commands::linear::run_sync`.
//! What: [`SyncScope`], [`resolve_scope`], [`next_cursor`],
//! [`validate_team_key`], and [`build_issues_filter`] — the Linear
//! GraphQL-filter counterpart to JIRA's JQL-string [`build_jql`].
//!
//! [`build_jql`]: crate::collect::jira::sync::build_jql

use chrono::{DateTime, Utc};

/// The scope of one `tga linear sync` invocation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncScope {
    /// Linear team key to restrict the sync to, e.g. `"ENG"`.
    pub team_key: String,
    /// Lower bound on `updatedAt`, when known. `None` means "no lower bound"
    /// — used for the first-ever sync of a team, or an explicit `--backfill`
    /// with no `--since`.
    pub since: Option<DateTime<Utc>>,
}

/// Resolve the effective sync scope from CLI/config inputs.
///
/// Precedence for the `since` bound, identical to
/// [`crate::collect::jira::sync::resolve_scope`]:
/// 1. `--backfill` with no explicit `--since` → `None` (full history).
/// 2. Explicit `--since` (present regardless of `--backfill`) → that date.
/// 3. Stored cursor (`linear_sync_cursor.last_synced_at`) → that timestamp.
/// 4. Nothing stored and no flags → `None` (first-ever sync is a full pull).
///
/// Test: `resolve_scope_prefers_explicit_since`,
/// `resolve_scope_backfill_with_no_since_is_full_history`,
/// `resolve_scope_falls_back_to_the_stored_cursor`,
/// `resolve_scope_first_ever_sync_is_full_history`.
pub fn resolve_scope(
    team_key: &str,
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
        team_key: team_key.to_string(),
        since,
    }
}

/// Compute the next incremental cursor from the `updatedAt` timestamps of
/// issues processed in a run.
///
/// Returns `None` when no issues were processed (the caller should leave the
/// stored cursor untouched, not regress it). Unlike JIRA's `plan_cursor`, a
/// bulk sync run either succeeds in full or propagates its error before any
/// cursor decision is made — there is no per-issue partial-failure state to
/// clamp against, so this is the whole cursor policy.
///
/// Test: `next_cursor_returns_the_maximum_observed`,
/// `next_cursor_is_none_on_an_empty_run`.
pub fn next_cursor(observed_updated: &[DateTime<Utc>]) -> Option<DateTime<Utc>> {
    observed_updated.iter().max().copied()
}

/// Reject a team key that would not survive interpolation into a GraphQL
/// filter value.
///
/// Mirrors [`crate::collect::jira::sync::validate_project_key`]'s rationale:
/// this is a robustness guard, not a security boundary (the value comes from
/// `--team` or `config.yaml`, both operator-controlled), but an unvalidated
/// key sent as a GraphQL string literal could match more issues than
/// intended.
///
/// # Errors
///
/// Returns a human-readable message naming the offending value.
///
/// Test: `validate_team_key_accepts_conventional_keys`,
/// `validate_team_key_rejects_empty_and_malformed_keys`.
pub fn validate_team_key(key: &str) -> Result<(), String> {
    let mut chars = key.chars();
    let ok = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid Linear team key '{key}': expected a leading ASCII letter followed by \
             letters, digits or underscores (e.g. ENG)"
        ))
    }
}

/// Build the `filter` GraphQL variable for a team-scoped `issues` query.
///
/// Filters on `team.key` always; adds an `updatedAt.gte` clause only when
/// `since` is `Some` — omitting the key entirely (rather than sending
/// `null`) is deliberate, matching [`crate::collect::jira::sync::build_jql`]'s
/// choice to emit no bound at all for a full-history scope rather than a
/// bound that might be misread as "since the epoch".
///
/// Test: `build_issues_filter_scopes_to_the_team`,
/// `build_issues_filter_adds_the_updated_at_bound_when_since_is_some`,
/// `build_issues_filter_omits_the_bound_when_since_is_none`.
pub fn build_issues_filter(team_key: &str, since: Option<DateTime<Utc>>) -> serde_json::Value {
    match since {
        Some(since) => serde_json::json!({
            "team": { "key": { "eq": team_key } },
            "updatedAt": { "gte": since.to_rfc3339() },
        }),
        None => serde_json::json!({
            "team": { "key": { "eq": team_key } },
        }),
    }
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
    fn resolve_scope_prefers_explicit_since() {
        let scope = resolve_scope(
            "ENG",
            Some(dt("2026-02-01T00:00:00Z")),
            true,
            Some(dt("2026-01-01T00:00:00Z")),
        );
        assert_eq!(scope.since, Some(dt("2026-02-01T00:00:00Z")));
    }

    #[test]
    fn resolve_scope_backfill_with_no_since_is_full_history() {
        let scope = resolve_scope("ENG", None, true, Some(dt("2026-01-01T00:00:00Z")));
        assert_eq!(scope.since, None);
    }

    #[test]
    fn resolve_scope_falls_back_to_the_stored_cursor() {
        let scope = resolve_scope("ENG", None, false, Some(dt("2026-01-01T00:00:00Z")));
        assert_eq!(scope.since, Some(dt("2026-01-01T00:00:00Z")));
    }

    #[test]
    fn resolve_scope_first_ever_sync_is_full_history() {
        let scope = resolve_scope("ENG", None, false, None);
        assert_eq!(scope.since, None);
    }

    #[test]
    fn next_cursor_returns_the_maximum_observed() {
        let times = vec![
            dt("2026-01-01T00:00:00Z"),
            dt("2026-03-01T00:00:00Z"),
            dt("2026-02-01T00:00:00Z"),
        ];
        assert_eq!(next_cursor(&times), Some(dt("2026-03-01T00:00:00Z")));
    }

    #[test]
    fn next_cursor_is_none_on_an_empty_run() {
        assert_eq!(next_cursor(&[]), None);
    }

    #[test]
    fn validate_team_key_accepts_conventional_keys() {
        for key in ["ENG", "FE", "Ops1", "team_a"] {
            assert!(validate_team_key(key).is_ok(), "key: {key}");
        }
    }

    #[test]
    fn validate_team_key_rejects_empty_and_malformed_keys() {
        for key in ["", "1ENG", "ENG OR 1=1", "ENG-1", "ENG\""] {
            assert!(validate_team_key(key).is_err(), "key: {key}");
        }
    }

    #[test]
    fn build_issues_filter_scopes_to_the_team() {
        let filter = build_issues_filter("ENG", None);
        assert_eq!(filter["team"]["key"]["eq"], "ENG");
    }

    #[test]
    fn build_issues_filter_adds_the_updated_at_bound_when_since_is_some() {
        let since = Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap();
        let filter = build_issues_filter("ENG", Some(since));
        assert_eq!(filter["updatedAt"]["gte"], since.to_rfc3339());
    }

    #[test]
    fn build_issues_filter_omits_the_bound_when_since_is_none() {
        let filter = build_issues_filter("ENG", None);
        assert!(filter.get("updatedAt").is_none());
    }
}
