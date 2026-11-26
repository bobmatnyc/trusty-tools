//! Rate-limit backoff for `gh` probes: `tm wait` and the daemon's PR watch
//! (#10550). #11673: moved from the `tm` binary so the daemon backs off on
//! the same refusal line, with no second classifier.
//!
//! Why: the pinned gh shim answers a primary GraphQL rate-limit refusal with
//! one fixed line naming the reset epoch. `poll_until` used to count it as a
//! generic probe error, re-poll on the normal interval, and exit 2 after four
//! in a row, while the limit had not reset.
//! What: [`classify`] recognises the shim's own line, through the constants
//! exported next to it in `gh_session_shim.rs`; [`backoff_secs`] says how long
//! to wait before the next poll. The caller clamps that wait to the slice and
//! the hard deadline.
//! Test: `classify_reads_the_shim_refusal_and_its_reset_epoch` and the
//! `loop_*rate_limit*` tests in `tm wait`'s `tests.rs`;
//! `a_rate_limited_read_pauses_the_watch_until_the_reset`.

use crate::core::gh_session_shim::{
    RATE_LIMIT_REFUSAL_MARKER, RATE_LIMIT_RESET_EPOCH_OPEN, RATE_LIMIT_SHORT_WINDOW_MARKER,
};

/// Consecutive rate-limited polls that end the wait with an error.
///
/// Why: a refusal that keeps coming after its reset is not transient; the
/// wait must fail closed, not spin until the deadline.
/// What: the fifth consecutive refusal returns `Err` (exit 2).
/// Test: `rate_limit_exhaustion_errors_not_met`.
pub const MAX_CONSECUTIVE_RATE_LIMITED: u32 = 5;

/// Upper bound, in seconds, on the backoff when the reset time is unknown.
const UNKNOWN_RESET_BACKOFF_CAP_SECS: u64 = 300;

/// A probe error the shim marked as a GraphQL rate-limit refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RateLimited {
    /// Unix seconds at which the limit resets, when the shim could read it.
    pub reset_epoch: Option<u64>,
}

/// Classify a probe error as the shim's rate-limit refusal, or not.
///
/// Why: only the shim's own sentence earns a backoff; any other error, such as
/// a REST `HTTP 403` quota, stays a generic probe error.
/// What: finds [`RATE_LIMIT_REFUSAL_MARKER`] in the full error chain, then
/// reads the digits between [`RATE_LIMIT_RESET_EPOCH_OPEN`] and `)` after it.
/// A marker with no readable epoch gives `reset_epoch: None`. The shim's
/// short-window line ([`RATE_LIMIT_SHORT_WINDOW_MARKER`]) also gives
/// `reset_epoch: None`, so [`backoff_secs`] backs off exponentially instead
/// of sleeping to the hourly reset. No marker gives `None`.
/// Test: `classify_reads_the_shim_refusal_and_its_reset_epoch`,
/// `classify_reads_the_short_window_line_as_a_backoff_with_no_reset`.
pub fn classify(err: &anyhow::Error) -> Option<RateLimited> {
    let text = format!("{err:#}");
    let Some(at) = text.find(RATE_LIMIT_REFUSAL_MARKER) else {
        // #11233: quota left — a short-window limit, never a sleep to the reset.
        return text
            .contains(RATE_LIMIT_SHORT_WINDOW_MARKER)
            .then_some(RateLimited { reset_epoch: None });
    };
    let tail = &text[at..];
    let reset_epoch = tail
        .find(RATE_LIMIT_RESET_EPOCH_OPEN)
        .map(|at| &tail[at + RATE_LIMIT_RESET_EPOCH_OPEN.len()..])
        .and_then(|rest| rest.split_once(')'))
        .and_then(|(digits, _)| digits.parse::<u64>().ok());
    Some(RateLimited { reset_epoch })
}

/// The name of the GraphQL limit a classified refusal hit, for a message.
///
/// Why: #11233 — `tm wait` must not tell the user "primary" while quota
/// remains. The marker decides, not `reset_epoch`: a primary line whose reset
/// lookup failed has no epoch either.
/// What: the short-window name when [`RATE_LIMIT_SHORT_WINDOW_MARKER`] is in
/// the error chain, else the primary name.
/// Test: `classify_reads_the_short_window_line_as_a_backoff_with_no_reset`,
/// `a_short_window_refusal_is_not_reported_as_the_primary_limit`.
pub fn limit_name(err: &anyhow::Error) -> &'static str {
    if format!("{err:#}").contains(RATE_LIMIT_SHORT_WINDOW_MARKER) {
        "short-window or secondary GraphQL rate limit"
    } else {
        "primary GraphQL rate limit"
    }
}

/// Real `gh` rate-limit texts the daemon backs off on (#11673).
///
/// Why: the daemon's `gh` runs bound to the real binary (#9750), not a pinned
/// session's shim, so the shim's line never reaches it.
const GH_RATE_LIMIT_TEXTS: &[&str] = &[
    "api rate limit exceeded",
    "api rate limit already exceeded",
    "secondary rate limit",
    "http 429",
];

/// [`classify`], plus the real `gh` rate-limit refusals.
///
/// Why: #11673 — the daemon's PR watch must back off on a live limit too.
/// `tm wait` keeps [`classify`], whose contract (#10550) counts an unmarked
/// refusal as a generic probe error.
/// What: the shim's line first (with its reset epoch); else any
/// [`GH_RATE_LIMIT_TEXTS`] entry, or `HTTP 403` beside "rate limit", in the
/// error chain, case-insensitive, with no reset epoch. A bare 403 is not one.
/// Test: `classify_any_reads_real_gh_rate_limit_stderr`.
pub fn classify_any(err: &anyhow::Error) -> Option<RateLimited> {
    classify(err).or_else(|| {
        let text = format!("{err:#}").to_ascii_lowercase();
        let hit = GH_RATE_LIMIT_TEXTS.iter().any(|n| text.contains(n))
            || (text.contains("http 403") && text.contains("rate limit"));
        hit.then_some(RateLimited { reset_epoch: None })
    })
}

/// Seconds to wait before the next poll after a rate-limited one.
///
/// Why: re-polling before the reset spends a call the limit will refuse.
/// What: with a reset epoch still ahead, `reset - now`. With a reset already
/// passed, the normal `interval_s`. With no reset, `interval_s` doubled per
/// consecutive refusal (`consecutive` starts at 1: 20, 40, 80 s…), capped at
/// 5 minutes but never below `interval_s`. The result is unclamped; the caller
/// bounds it by the slice and the hard deadline.
/// Test: `loop_backs_off_to_the_reset_on_a_rate_limited_probe`,
/// `loop_backs_off_exponentially_when_the_reset_is_unknown`.
pub fn backoff_secs(hit: RateLimited, now: u64, interval_s: u64, consecutive: u32) -> u64 {
    match hit.reset_epoch {
        Some(reset) if reset > now => reset - now,
        Some(_) => interval_s,
        None => {
            let doublings = consecutive.saturating_sub(1).min(16);
            interval_s
                .saturating_mul(1u64 << doublings)
                .min(UNKNOWN_RESET_BACKOFF_CAP_SECS)
                .max(interval_s)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// #11233: the shim's quota-left line is a short backoff with no reset,
    /// for both classifiers, even though it carries gh's own limit text.
    #[test]
    fn classify_reads_the_short_window_line_as_a_backoff_with_no_reset() {
        let line = format!(
            "`gh project item-list 45` failed (exit status: 1): \
             {RATE_LIMIT_SHORT_WINDOW_MARKER} (gh account 'bot' on github.com); the hourly \
             GraphQL quota still has 4186 points left, so this is not the primary limit. tm \
             did not retry it. gh said: GraphQL: API rate limit already exceeded for user ID 1."
        );
        let err = anyhow::anyhow!(line);
        let short = Some(RateLimited { reset_epoch: None });
        assert_eq!(classify(&err), short);
        assert_eq!(classify_any(&err), short);
        assert_eq!(
            limit_name(&err),
            "short-window or secondary GraphQL rate limit"
        );
        let primary = anyhow::anyhow!("{RATE_LIMIT_REFUSAL_MARKER} (gh account 'bot')");
        assert_eq!(limit_name(&primary), "primary GraphQL rate limit");
        assert_eq!(
            backoff_secs(RateLimited { reset_epoch: None }, 1_000, 20, 1),
            20
        );
        assert!(!RATE_LIMIT_SHORT_WINDOW_MARKER.contains(RATE_LIMIT_REFUSAL_MARKER));
    }

    #[test]
    fn classify_any_reads_real_gh_rate_limit_stderr() {
        let hit = |s: &str| {
            classify_any(&anyhow::anyhow!(
                "`gh pr view 7` failed (exit status: 1): {s}"
            ))
        };
        for real in [
            "GraphQL: API rate limit exceeded for user ID 1234.",
            "gh: API rate limit exceeded for user ID 1234. (HTTP 403)",
            "GraphQL: API rate limit already exceeded for user ID 1234.",
            "gh: You have exceeded a secondary rate limit. Please wait a few minutes before you try again. (HTTP 403)",
            "HTTP 429: Too Many Requests (https://api.github.com/graphql)",
        ] {
            assert_eq!(hit(real), Some(RateLimited { reset_epoch: None }), "{real}");
        }
        for other in [
            "HTTP 403: Resource not accessible by integration",
            "connection reset",
        ] {
            assert_eq!(hit(other), None, "{other}");
        }
        assert_eq!(
            classify(&anyhow::anyhow!("gh: API rate limit exceeded (HTTP 403)")),
            None
        );
    }
}
