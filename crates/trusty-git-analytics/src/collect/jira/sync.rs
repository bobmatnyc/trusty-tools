//! Pure JQL-building and cursor-arithmetic helpers backing `tga jira sync`
//! (issue #3966).
//!
//! Kept free of HTTP/DB dependencies so pagination and incremental-cursor
//! logic can be unit tested without a mock server or database — the actual
//! orchestration (HTTP client + DB writes) lives in
//! `commands::jira::run_sync`.

use chrono::{DateTime, Utc};
use chrono_tz::Tz;

// Imported under an alias, not as `Result`: `validate_project_key` below
// returns a plain `std::result::Result<(), String>`, and shadowing the
// prelude alias in this module would silently retype it.
use crate::collect::errors::Result as CollectResult;
use crate::collect::jira::jql_time::jql_date;

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

/// Build the JQL query for one sync scope, rendering the `updated >=` bound
/// in `tz`.
///
/// Always orders by `updated ASC` so the last issue returned in a run
/// determines the next cursor (issue's `updated` timestamp becomes the next
/// `since`).
///
/// Why minute resolution and not RFC3339: JQL's `date`/`datetime` literal
/// grammar is `"yyyy-MM-dd[ HH:mm]"`; a full RFC3339 string (with seconds,
/// fractional seconds, or a `T`/`Z` separator) is rejected with a 400.
///
/// Why `tz` is a parameter and not `Utc`: the grammar has no timezone syntax,
/// and JIRA resolves the literal in the *querying account's* profile
/// timezone. Rendering it as UTC wall-clock on a non-UTC account moves the
/// window by the account's offset — hours, against a safety margin measured
/// in seconds. [`jql_date`] carries the full argument and the guarantee.
///
/// # Errors
///
/// Propagates [`jql_date`]'s config error when the bound cannot be rendered
/// safely in `tz`. A scope with no `since` is infallible — it emits no date
/// literal at all.
pub fn build_jql(scope: &SyncScope, tz: Tz) -> CollectResult<String> {
    Ok(match scope.since {
        Some(since) => format!(
            "project = {} AND updated >= \"{}\" ORDER BY updated ASC",
            scope.project_key,
            jql_date(since, tz)?
        ),
        None => format!("project = {} ORDER BY updated ASC", scope.project_key),
    })
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
///
/// Callers that can observe *partial* ticket failures must use
/// [`plan_cursor`] instead — this function knows nothing about failures and
/// will happily return a maximum that steps over a ticket whose ingestion
/// did not complete.
pub fn next_cursor(observed_updated: &[DateTime<Utc>]) -> Option<DateTime<Utc>> {
    observed_updated.iter().max().copied()
}

/// What a sync run should do with the stored incremental cursor.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CursorPlan {
    /// Store this timestamp as the new `updated >=` cursor.
    Advance(DateTime<Utc>),
    /// Leave the stored cursor exactly as it is.
    Hold,
}

/// Decide the next stored cursor given what a run observed *and* what it
/// failed to ingest.
///
/// # The invariant this exists to enforce
///
/// **A ticket whose ingestion did not fully succeed always remains inside
/// the next run's query window.** Because the window is
/// `updated >= <cursor>` (inclusive, and further truncated *downward* to
/// minute resolution by [`jql_date`]), that reduces to a single rule:
///
/// > the stored cursor is never allowed above the `updated` timestamp of the
/// > earliest failed ticket.
///
/// Why it must be the *earliest* failure and not, say, "skip the failed
/// one": tickets arrive in `updated ASC` order, so clamping to the earliest
/// failure also re-covers every ticket after it. Re-covering is free —
/// every write on this path is an `INSERT OR REPLACE` upsert, so a re-read
/// is idempotent. Skipping is not free: it is permanent, because the ticket
/// sorts below the cursor forever after and nothing but a human edit or a
/// full `--backfill` would ever bring it back.
///
/// A failed ticket with no `updated` timestamp cannot be placed on the
/// timeline at all, so there is no safe clamp for it — the only correct
/// answer is [`CursorPlan::Hold`], leaving the whole window unchanged.
///
/// # Why a clean run never moves the cursor backwards
///
/// `stored` is the cursor already on record. A *successful* run only ever
/// advances past it: `--since 2020-01-01`, or a `--backfill` truncated by
/// `--max-tickets`, would otherwise rewrite a healthy 2026 incremental cursor
/// back to whatever old window that run happened to read, and the next
/// incremental run would then re-walk years of history. Nothing is lost when
/// that happens, but nothing is gained either, and the operator is not told.
///
/// A *failure clamp* is exempt and may regress the cursor — regressing is the
/// entire point of the clamp, and it is bounded by the window the run
/// actually queried.
///
/// Test: `plan_cursor_clamps_to_the_earliest_failure`,
/// `plan_cursor_holds_when_a_failure_has_no_timestamp`,
/// `plan_cursor_advances_to_max_when_nothing_failed`,
/// `plan_cursor_holds_on_an_empty_run`,
/// `plan_cursor_never_regresses_a_healthy_cursor`,
/// `plan_cursor_failure_clamp_may_regress_below_the_stored_cursor`.
pub fn plan_cursor(
    observed_updated: &[DateTime<Utc>],
    failed_updated: &[Option<DateTime<Utc>>],
    stored: Option<DateTime<Utc>>,
) -> CursorPlan {
    if failed_updated.iter().any(Option::is_none) {
        return CursorPlan::Hold;
    }
    if let Some(earliest_failure) = failed_updated.iter().flatten().min() {
        return CursorPlan::Advance(*earliest_failure);
    }
    match next_cursor(observed_updated) {
        // Monotonic: a clean run may only move the cursor forward.
        Some(max) => CursorPlan::Advance(stored.map_or(max, |s| s.max(max))),
        None => CursorPlan::Hold,
    }
}

/// Reject a project key that would not survive interpolation into JQL.
///
/// Why: [`build_jql`] interpolates the key directly into the query string.
/// This is not a security boundary — the value comes from `--project` or
/// `config.yaml`, both operator-controlled — but an unvalidated key is a
/// real robustness hazard: a quote or a boolean operator silently *widens*
/// the query, and tickets from another project then advance *this*
/// project's cursor, corrupting incremental state for both. A benign typo
/// (a space, a reserved word) otherwise surfaces as an opaque JIRA 400
/// instead of a local error naming the offending value.
///
/// What: JIRA project keys are `[A-Za-z][A-Za-z0-9_]*`; anything else is
/// rejected here rather than at the far end of an HTTP round-trip.
///
/// # Errors
///
/// Returns a human-readable message naming the offending value.
///
/// Test: `validate_project_key_accepts_conventional_keys`,
/// `validate_project_key_rejects_jql_metacharacters`.
pub fn validate_project_key(key: &str) -> Result<(), String> {
    let mut chars = key.chars();
    let ok = match chars.next() {
        Some(c) if c.is_ascii_alphabetic() => chars.all(|c| c.is_ascii_alphanumeric() || c == '_'),
        _ => false,
    };
    if ok {
        Ok(())
    } else {
        Err(format!(
            "invalid JIRA project key '{key}': expected a leading ASCII letter \
             followed by letters, digits or underscores (e.g. PROJ)"
        ))
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
    fn jql_date_formats_at_minute_resolution() {
        let d = Utc.with_ymd_and_hms(2026, 3, 4, 5, 6, 7).unwrap();
        assert_eq!(jql_date(d, Tz::UTC).expect("renders"), "2026-03-04 05:06");
    }

    /// `build_jql` must thread the account zone through to the literal — a
    /// bound rendered in UTC on a UTC-5 account queries the wrong five hours.
    #[test]
    fn build_jql_renders_the_bound_in_the_account_timezone() {
        let scope = SyncScope {
            project_key: "PROJ".into(),
            since: Some(dt("2026-01-03T10:00:00Z")),
        };
        assert_eq!(
            build_jql(&scope, Tz::America__New_York).expect("renders"),
            "project = PROJ AND updated >= \"2026-01-03 05:00\" ORDER BY updated ASC"
        );
    }

    #[test]
    fn build_jql_without_since_omits_updated_clause() {
        let scope = SyncScope {
            project_key: "PROJ".into(),
            since: None,
        };
        assert_eq!(
            build_jql(&scope, Tz::UTC).expect("renders"),
            "project = PROJ ORDER BY updated ASC"
        );
    }

    #[test]
    fn build_jql_with_since_includes_updated_clause() {
        let scope = SyncScope {
            project_key: "PROJ".into(),
            since: Some(dt("2026-01-01T00:00:00Z")),
        };
        assert_eq!(
            build_jql(&scope, Tz::UTC).expect("renders"),
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

    // ---- plan_cursor: the "no ticket is ever permanently skipped" rule ----

    #[test]
    fn plan_cursor_advances_to_max_when_nothing_failed() {
        let observed = vec![dt("2026-01-01T00:00:00Z"), dt("2026-03-01T00:00:00Z")];
        assert_eq!(
            plan_cursor(&observed, &[], None),
            CursorPlan::Advance(dt("2026-03-01T00:00:00Z"))
        );
    }

    #[test]
    fn plan_cursor_holds_on_an_empty_run() {
        assert_eq!(plan_cursor(&[], &[], None), CursorPlan::Hold);
    }

    /// The CRITICAL case: a ticket in the middle of the batch failed, so the
    /// cursor must clamp back to *its* timestamp — not to the batch maximum,
    /// which would put it permanently below the next query window.
    #[test]
    fn plan_cursor_clamps_to_the_earliest_failure() {
        let observed = vec![
            dt("2026-01-01T00:00:00Z"),
            dt("2026-01-03T00:00:00Z"),
            dt("2026-06-01T00:00:00Z"),
        ];
        let failed = vec![
            Some(dt("2026-01-03T00:00:00Z")),
            Some(dt("2026-04-01T00:00:00Z")),
        ];
        assert_eq!(
            plan_cursor(&observed, &failed, None),
            CursorPlan::Advance(dt("2026-01-03T00:00:00Z")),
            "the cursor must never move above the earliest failed ticket"
        );
    }

    /// A failure we cannot place on the timeline leaves the window entirely
    /// unchanged — there is no clamp that could be proven safe.
    #[test]
    fn plan_cursor_holds_when_a_failure_has_no_timestamp() {
        let observed = vec![dt("2026-06-01T00:00:00Z")];
        assert_eq!(plan_cursor(&observed, &[None], None), CursorPlan::Hold);
    }

    /// The clamped cursor must survive the JQL round-trip **on any account
    /// timezone**, not just UTC.
    ///
    /// This is the round-1 proof re-stated over instants rather than strings.
    /// Round 1 compared the two rendered literals lexically, which is only
    /// meaningful if both denote UTC — precisely the assumption round 2
    /// disputed. Here the emitted bound is resolved back into an instant the
    /// way JIRA would, and compared against the failed ticket's instant.
    #[test]
    fn clamped_cursor_keeps_the_failed_ticket_inside_the_next_window() {
        for tz in [
            Tz::UTC,
            Tz::America__New_York,
            Tz::Asia__Kolkata,
            Tz::Pacific__Kiritimati,
        ] {
            let failed_at = dt("2026-01-03T10:15:45Z");
            let CursorPlan::Advance(next) =
                plan_cursor(&[dt("2026-06-01T00:00:00Z")], &[Some(failed_at)], None)
            else {
                panic!("expected an advance");
            };
            let bound = jql_date(next, tz).expect("renders");
            let naive = chrono::NaiveDateTime::parse_from_str(&bound, "%Y-%m-%d %H:%M")
                .expect("renderer emits a parseable literal");
            let window_opens = tz
                .from_local_datetime(&naive)
                .latest()
                .expect("resolvable")
                .with_timezone(&Utc);
            assert!(
                window_opens <= failed_at,
                "{tz}: window opens at {window_opens}, after the failed ticket at \
                 {failed_at} — it would be skipped forever"
            );
        }
    }

    /// `--since 2020-01-01` against a healthy 2026 cursor must not rewind it:
    /// the run read an old window, but that says nothing about the newer
    /// tickets the stored cursor had already covered.
    #[test]
    fn plan_cursor_never_regresses_a_healthy_cursor() {
        let stored = dt("2026-06-01T00:00:00Z");
        let observed = vec![dt("2020-01-05T00:00:00Z"), dt("2020-02-01T00:00:00Z")];
        assert_eq!(
            plan_cursor(&observed, &[], Some(stored)),
            CursorPlan::Advance(stored),
            "a clean run must be monotonic in the cursor"
        );
    }

    /// …but the failure clamp is exempt, because regressing is exactly what
    /// it is for.
    #[test]
    fn plan_cursor_failure_clamp_may_regress_below_the_stored_cursor() {
        let stored = dt("2026-06-01T00:00:00Z");
        let failed_at = dt("2026-01-03T00:00:00Z");
        assert_eq!(
            plan_cursor(
                &[dt("2026-06-01T00:00:00Z")],
                &[Some(failed_at)],
                Some(stored)
            ),
            CursorPlan::Advance(failed_at),
            "a held cursor must be able to move back to re-cover a failure"
        );
    }

    // ---- validate_project_key ------------------------------------------

    #[test]
    fn validate_project_key_accepts_conventional_keys() {
        for key in ["PROJ", "API", "Proj2", "a_b9"] {
            assert!(validate_project_key(key).is_ok(), "{key} must be accepted");
        }
    }

    #[test]
    fn validate_project_key_rejects_jql_metacharacters() {
        for key in [
            "",
            "1PROJ",
            "PROJ OR project = OTHER",
            "PR\"OJ",
            "PROJ-1",
            "PROJ)",
        ] {
            assert!(
                validate_project_key(key).is_err(),
                "{key:?} must be rejected before it reaches JQL"
            );
        }
    }
}
