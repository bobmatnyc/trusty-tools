//! Rate-limit backoff for `tm wait` probes (#10550).
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
//! `loop_*rate_limit*` tests in the sibling `tests.rs`.

use trusty_mpm::core::gh_session_shim::{RATE_LIMIT_REFUSAL_MARKER, RATE_LIMIT_RESET_EPOCH_OPEN};

/// Consecutive rate-limited polls that end the wait with an error.
///
/// Why: a refusal that keeps coming after its reset is not transient; the
/// wait must fail closed, not spin until the deadline.
/// What: the fifth consecutive refusal returns `Err` (exit 2).
/// Test: `rate_limit_exhaustion_errors_not_met`.
pub(crate) const MAX_CONSECUTIVE_RATE_LIMITED: u32 = 5;

/// Upper bound, in seconds, on the backoff when the reset time is unknown.
const UNKNOWN_RESET_BACKOFF_CAP_SECS: u64 = 300;

/// A probe error the shim marked as a primary GraphQL rate-limit refusal.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct RateLimited {
    /// Unix seconds at which the limit resets, when the shim could read it.
    pub(crate) reset_epoch: Option<u64>,
}

/// Classify a probe error as the shim's rate-limit refusal, or not.
///
/// Why: only the shim's own sentence earns a backoff; any other error, such as
/// a REST `HTTP 403` quota, stays a generic probe error.
/// What: finds [`RATE_LIMIT_REFUSAL_MARKER`] in the full error chain, then
/// reads the digits between [`RATE_LIMIT_RESET_EPOCH_OPEN`] and `)` after it.
/// No marker gives `None`; a marker with no readable epoch gives
/// `reset_epoch: None`.
/// Test: `classify_reads_the_shim_refusal_and_its_reset_epoch`.
pub(crate) fn classify(err: &anyhow::Error) -> Option<RateLimited> {
    let text = format!("{err:#}");
    let tail = &text[text.find(RATE_LIMIT_REFUSAL_MARKER)?..];
    let reset_epoch = tail
        .find(RATE_LIMIT_RESET_EPOCH_OPEN)
        .map(|at| &tail[at + RATE_LIMIT_RESET_EPOCH_OPEN.len()..])
        .and_then(|rest| rest.split_once(')'))
        .and_then(|(digits, _)| digits.parse::<u64>().ok());
    Some(RateLimited { reset_epoch })
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
pub(crate) fn backoff_secs(hit: RateLimited, now: u64, interval_s: u64, consecutive: u32) -> u64 {
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
