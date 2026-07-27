//! Rendering instants as JQL date literals (issue #3966, PR #4067 review
//! round 2).
//!
//! # Why this module exists
//!
//! **JIRA evaluates a JQL date literal in the timezone of the account
//! executing the query, not in UTC.** Confirmed by Atlassian staff on the
//! developer community thread "JQL on search with updated date time does not
//! work correctly" (a GMT+3 instance queried from GMT+8: the literal was
//! interpreted as GMT+8 wall-clock and converted to UTC for comparison), and
//! by JRACLOUD-74279, which exists precisely to *request* an option to
//! evaluate date JQL in the instance timezone instead of the account's.
//!
//! The JQL grammar has no timezone syntax — the literal is a naive
//! `"yyyy-MM-dd HH:mm"` — so a UTC-formatted literal sent by an
//! `America/New_York` account denotes an instant **five hours later** than
//! intended. For `tga jira sync` that is not a cosmetic offset: the entire
//! cursor invariant in [`super::sync::plan_cursor`] is stated over *instants*
//! but enforced through this string. A five-hour forward reinterpretation
//! dwarfs the ≤59s downward truncation the invariant's safety margin relies
//! on, and every ticket in the gap is dropped permanently.
//!
//! # The property this module guarantees
//!
//! [`jql_date`] renders `dt` such that, **however the account's zone resolves
//! the emitted literal, the resulting instant is never later than `dt`**.
//! Erring downward is free — the window merely re-reads a little, and every
//! write on this path is an idempotent upsert. Erring upward is what loses
//! tickets.
//!
//! That is stronger than "convert to local time and truncate", because local
//! wall-clock is not a bijection with instants:
//!
//! - **DST fall-back**: a repeated local hour maps one literal to *two*
//!   instants, and JIRA may pick the later one.
//! - **DST spring-forward**: a skipped local hour maps a literal to *no*
//!   instant, and a lenient parser typically shifts it forward.
//!
//! So the renderer proposes a literal, resolves it back the way JIRA would in
//! the worst case, and steps back a minute until the worst case satisfies the
//! property. On a zone with no transition in play — the overwhelmingly common
//! case, and every case on a UTC account — the first candidate is accepted
//! and this costs nothing.

use chrono::{DateTime, Duration, LocalResult, NaiveDateTime, TimeZone, Timelike, Utc};
use chrono_tz::Tz;

use crate::collect::errors::{CollectError, Result};

/// Upper bound on the step-back search, in minutes.
///
/// The previous value (180) came with the claim that "historic DST shifts
/// never exceed two hours, so 180 minutes cannot be reached". **That claim is
/// false.** `Antarctica/Casey` folds exactly three hours (UTC+11 → UTC+08,
/// e.g. 2010-03-05), so the largest fold in the IANA database landed exactly
/// ON the old ceiling: an exhaustive sweep of all 19,093 transitions in the
/// database saturated the loop 8 times (PR #4067 review round 3). The emitted
/// literals happened to stay safe, but with zero margin — a 181-minute fold
/// would have placed the literal inside the fold, resolving up to three hours
/// *after* `dt`, which is precisely the ticket-losing failure this module
/// exists to prevent.
///
/// The bound that actually holds is not about transition sizes at all. Writing
/// `resolved(c) = c - offset(c)` and `dt ≈ c0 - offset(c0)`, the property
/// `resolved(c0 - k) <= dt` is satisfied once `k` exceeds the difference
/// between any two offsets the zone can present, which is bounded by the full
/// span of UTC offsets in the database: −12:00 … +14:00, i.e. 26 hours. So
/// 1560 minutes is sufficient for *any* zone, including one with a fold larger
/// than any yet observed, and does not rest on an empirical claim about
/// tzdata's contents that a future release could falsify.
///
/// It is a ceiling, not a target: every real zone converges in at most one
/// fold-width (≤180 minutes today), and a UTC account converges immediately.
const MAX_STEP_BACK_MINUTES: i64 = 1560;

/// Parse an IANA timezone name (as JIRA reports it on `/myself`).
///
/// # Errors
///
/// [`CollectError::Config`] naming the unparseable value.
///
/// Test: `parse_timezone_accepts_iana_names`, `parse_timezone_rejects_garbage`.
pub fn parse_timezone(name: &str) -> Result<Tz> {
    name.parse::<Tz>().map_err(|_| {
        CollectError::Config(format!(
            "unrecognised JIRA timezone '{name}': expected an IANA name such as \
             `UTC` or `America/New_York`. Set `jira.timezone` in config.yaml to \
             override what the JIRA account reports."
        ))
    })
}

/// Format `dt` as a JQL date-time literal (`"yyyy-MM-dd HH:mm"`) in `tz`.
///
/// # Guarantee
///
/// The returned literal, evaluated in `tz` by any reasonable resolution of an
/// ambiguous or non-existent local time, denotes an instant `<= dt`. See the
/// module header for why "never later" is the property that matters and
/// "never earlier" is not.
///
/// Test: `jql_date_is_identity_on_a_utc_account`,
/// `jql_date_never_opens_after_the_intended_instant`,
/// `jql_date_stays_within_a_minute_below_the_intended_instant`,
/// `jql_date_is_safe_across_a_dst_fall_back_fold`,
/// `jql_date_is_safe_across_the_largest_fold_in_the_tz_database`.
///
/// # Errors
///
/// [`CollectError::Config`] if no candidate within [`MAX_STEP_BACK_MINUTES`]
/// satisfies the guarantee. That is unreachable for any zone whose offsets lie
/// in the −12:00 … +14:00 range the ceiling is derived from, so it means the
/// zone definition is pathological. The bound is **rejected rather than
/// emitted**: an unvalidated literal is exactly how this window opens late and
/// drops tickets, and a run that fails with a remediation hint is recoverable
/// where a silently-wrong backfill is not.
pub fn jql_date(dt: DateTime<Utc>, tz: Tz) -> Result<String> {
    let mut candidate = truncate_to_minute(dt.with_timezone(&tz).naive_local());
    for _ in 0..MAX_STEP_BACK_MINUTES {
        match worst_case_instant(candidate, tz) {
            Some(resolved) if resolved <= dt => return Ok(format_literal(candidate)),
            _ => candidate -= Duration::minutes(1),
        }
    }
    // Saturation. Never emit the last candidate on the assumption that 26h of
    // step-back "must" be enough — that assumption is what the old 180-minute
    // ceiling encoded, and it was wrong.
    Err(CollectError::Config(format!(
        "could not render a safe JQL date bound for {dt} in timezone '{tz}': no \
         literal within {MAX_STEP_BACK_MINUTES} minutes below it resolves to an \
         instant at or before it. This means the zone presents an offset range \
         wider than the IANA −12:00…+14:00 span. Set `jira.timezone` to `UTC` in \
         config.yaml to bypass zone resolution."
    )))
}

/// The latest instant a naive local literal could denote in `tz` — the worst
/// case for our "never later than `dt`" guarantee.
///
/// `None` means the literal names a local time that does not exist (a
/// spring-forward gap); the caller steps back rather than guessing how JIRA
/// would coerce it.
fn worst_case_instant(naive: NaiveDateTime, tz: Tz) -> Option<DateTime<Utc>> {
    match tz.from_local_datetime(&naive) {
        LocalResult::Single(t) => Some(t.with_timezone(&Utc)),
        LocalResult::Ambiguous(a, b) => Some(a.max(b).with_timezone(&Utc)),
        LocalResult::None => None,
    }
}

fn truncate_to_minute(naive: NaiveDateTime) -> NaiveDateTime {
    naive
        .with_second(0)
        .and_then(|n| n.with_nanosecond(0))
        .unwrap_or(naive)
}

fn format_literal(naive: NaiveDateTime) -> String {
    naive.format("%Y-%m-%d %H:%M").to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn dt(s: &str) -> DateTime<Utc> {
        DateTime::parse_from_rfc3339(s)
            .expect("valid rfc3339 fixture")
            .with_timezone(&Utc)
    }

    /// Resolve a literal the way JIRA would, taking the worst case for us.
    fn evaluate(literal: &str, tz: Tz) -> DateTime<Utc> {
        let naive = NaiveDateTime::parse_from_str(literal, "%Y-%m-%d %H:%M")
            .expect("renderer emits a parseable literal");
        worst_case_instant(naive, tz).expect("renderer never emits a nonexistent local time")
    }

    #[test]
    fn parse_timezone_accepts_iana_names() {
        assert_eq!(parse_timezone("UTC").expect("utc"), Tz::UTC);
        assert_eq!(
            parse_timezone("America/New_York").expect("ny"),
            Tz::America__New_York
        );
    }

    #[test]
    fn parse_timezone_rejects_garbage() {
        let err = parse_timezone("Mars/Olympus").expect_err("must reject");
        assert!(
            err.to_string().contains("Mars/Olympus"),
            "must name the value: {err}"
        );
    }

    /// On a UTC account the renderer is exactly the old behaviour — this is
    /// what makes the fix strictly safe to deploy.
    #[test]
    fn jql_date_is_identity_on_a_utc_account() {
        assert_eq!(
            jql_date(dt("2026-01-03T10:15:45Z"), Tz::UTC).expect("renders"),
            "2026-01-03 10:15"
        );
    }

    /// THE property the whole cursor invariant rests on, asserted directly
    /// rather than inferred from a mock: on a non-UTC account the emitted
    /// bound must evaluate to an instant at or before the one it encodes.
    ///
    /// Before this fix the renderer emitted UTC wall-clock, so an
    /// `America/New_York` account turned `10:15Z` into a bound at `15:15Z` —
    /// five hours *after* the ticket it was meant to re-cover.
    #[test]
    fn jql_date_never_opens_after_the_intended_instant() {
        let zones = [
            Tz::UTC,
            Tz::America__New_York, // UTC-5 / -4
            Tz::America__Los_Angeles,
            Tz::Europe__Amsterdam,   // UTC+1 / +2
            Tz::Asia__Kolkata,       // UTC+5:30, no DST
            Tz::Pacific__Kiritimati, // UTC+14, the extreme
            Tz::Pacific__Midway,     // UTC-11
        ];
        let instants = [
            "2026-01-03T10:15:45Z",
            "2026-06-15T23:59:59Z",
            "2026-03-08T06:30:00Z", // US spring-forward day
            "2026-11-01T05:30:00Z", // US fall-back day
            "2026-01-01T00:00:00Z",
        ];
        for tz in zones {
            for instant in instants {
                let intended = dt(instant);
                let literal = jql_date(intended, tz).expect("renders");
                let evaluated = evaluate(&literal, tz);
                assert!(
                    evaluated <= intended,
                    "{tz} @ {instant}: bound `{literal}` evaluates to {evaluated}, \
                     which is AFTER the instant it must re-cover"
                );
            }
        }
    }

    /// Erring downward is free, but it must stay tight — an unbounded
    /// step-back would make every incremental run re-read hours of tickets.
    #[test]
    fn jql_date_stays_within_a_minute_below_the_intended_instant() {
        for tz in [Tz::UTC, Tz::America__New_York, Tz::Asia__Kolkata] {
            let intended = dt("2026-06-15T12:34:56Z");
            let evaluated = evaluate(&jql_date(intended, tz).expect("renders"), tz);
            assert!(
                intended - evaluated < Duration::minutes(1),
                "{tz}: bound regressed {} below the intended instant",
                intended - evaluated
            );
        }
    }

    /// During a DST fall-back fold one local literal denotes two instants.
    /// Naive local formatting would emit a literal JIRA could resolve to the
    /// *later* one, opening the window up to an hour late.
    #[test]
    fn jql_date_is_safe_across_a_dst_fall_back_fold() {
        let tz = Tz::America__New_York;
        // 2026-11-01: 01:00-02:00 local occurs twice (05:00Z and 06:00Z).
        for minute in 0..60 {
            let intended = dt("2026-11-01T05:00:00Z") + Duration::minutes(minute);
            let literal = jql_date(intended, tz).expect("renders");
            let evaluated = evaluate(&literal, tz);
            assert!(
                evaluated <= intended,
                "fold minute {minute}: `{literal}` -> {evaluated} > {intended}"
            );
        }
    }

    /// The largest fold in the IANA database, which the old 180-minute
    /// ceiling could absorb only by coincidence (PR #4067 review round 3).
    ///
    /// `Antarctica/Casey` steps UTC+11 → UTC+08 — a THREE-hour fold, so the
    /// required step-back reached exactly the old ceiling and the loop
    /// saturated, emitting an unvalidated literal. The sweep below covers the
    /// whole fold with margin either side and asserts both halves of the
    /// contract: the bound is renderable at all (no saturation error), and it
    /// never opens after the instant it encodes.
    ///
    /// The `<= 3h` assertion is the one that would have caught the original
    /// defect: it pins the step-back to converging on the fold width rather
    /// than running to whatever the ceiling happens to be.
    #[test]
    fn jql_date_is_safe_across_the_largest_fold_in_the_tz_database() {
        let tz = Tz::Antarctica__Casey;
        // 2010-03-05 local 03:00 (+11) rewinds to 00:00 (+08). Sweep a full
        // day around it so the exact transition instant need not be pinned.
        let start = dt("2010-03-04T06:00:00Z");
        for minute in 0..(24 * 60) {
            let intended = start + Duration::minutes(minute);
            let literal = jql_date(intended, tz)
                .unwrap_or_else(|e| panic!("minute {minute}: bound must be renderable: {e}"));
            let evaluated = evaluate(&literal, tz);
            assert!(
                evaluated <= intended,
                "minute {minute}: `{literal}` -> {evaluated} > {intended}"
            );
            assert!(
                intended - evaluated <= Duration::hours(3),
                "minute {minute}: stepped back {} — the search must converge on \
                 the fold width, not run to the ceiling",
                intended - evaluated
            );
        }
    }

    /// The spring-forward gap names local times that do not exist; the
    /// renderer must step back out of the gap rather than emit one.
    #[test]
    fn jql_date_is_safe_across_a_dst_spring_forward_gap() {
        let tz = Tz::America__New_York;
        for minute in 0..60 {
            let intended = dt("2026-03-08T07:00:00Z") + Duration::minutes(minute);
            let literal = jql_date(intended, tz).expect("renders");
            let evaluated = evaluate(&literal, tz);
            assert!(evaluated <= intended, "gap minute {minute}: `{literal}`");
        }
    }
}
