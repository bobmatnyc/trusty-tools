//! The egress guard's daemon verdict client (#8454 VC, #8433).
//!
//! Why: a channel-tainted session's gated call needs the policy readiness
//! verdict, and the inline load costs ~1.5 s per call. The daemon answers it
//! from its policy snapshot. This is a Fail-Open Check site: no arm may make
//! a call ready that the inline check (`policy_ready`) would not.
//! What: [`verdict_query`] posts the hook's home to the daemon's
//! `VERDICT_ROUTE` over the daemon socket (`mpm.channels.egress_verdict`,
//! #6288) and parses the reply into a closed [`DaemonVerdict`].
//! [`cached_policy_ready`] turns it into the taint arm's readiness: a served
//! verdict stands; a reply that serves none, no daemon, and a timeout run the
//! inline load; a non-2xx status or a malformed reply is not ready.
//! Test: `egress_verdict_client_tests.rs`.

use std::path::Path;
use std::time::Duration;

use trusty_mpm::channels::snapshot::MAX_AGE;
use trusty_mpm::client::{DaemonCallError, DaemonClient};
use trusty_mpm::core::channels_readiness::{NOT_READY_CHECK, NotReady};
use trusty_mpm::core::egress_verdict::{Unserved, VERDICT_ROUTE, VerdictReply, VerdictRequest};

/// How long the whole query may take, dial and reply included (R2).
pub(crate) const VERDICT_QUERY_TIMEOUT: Duration = Duration::from_millis(300);

/// The daemon's verdict, as the hook reads it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum DaemonVerdict {
    /// The policy is ready.
    Ready,
    /// The policy is not ready, for this branch.
    NotReady(NotReady),
    /// No verdict was served; the hook loads the policy inline.
    Unserved(Unserved),
}

/// Why the query produced no [`DaemonVerdict`]. Each text is fixed: no
/// reply byte reaches a deny reason.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub(crate) enum QueryError {
    /// No daemon answered: an absent or refusing socket, or an address that
    /// names no socket (a refusal sentinel, #6288).
    #[error("no daemon answered")]
    Unreachable,
    /// The daemon did not answer within [`VERDICT_QUERY_TIMEOUT`].
    #[error("the daemon did not answer within {0:?}")]
    TimedOut(Duration),
    /// The daemon refused; the status its RPC error code projects onto.
    #[error("the daemon answered HTTP {0}")]
    Status(u16),
    /// The daemon's reply is not a verdict reply.
    #[error("the daemon's reply is malformed")]
    Malformed,
}

impl QueryError {
    /// True when no daemon answered, so the hook runs the inline load; an
    /// answer that came back unusable is not one, and denies.
    /// Test: `every_query_error_maps_to_inline_or_deny`.
    pub(crate) fn asked_no_one(&self) -> bool {
        matches!(self, Self::Unreachable | Self::TimedOut(_))
    }

    /// The taint arm's readiness for a query that failed with `self`.
    ///
    /// What: `inline` when [`Self::asked_no_one`], else
    /// [`NotReady::Unanswered`], a `POLICY_CHECK` deny.
    /// Test: `every_query_error_maps_to_inline_or_deny`.
    pub(crate) fn readiness(
        self,
        inline: impl FnOnce() -> Result<(), NotReady>,
    ) -> Result<(), NotReady> {
        if self.asked_no_one() {
            inline()
        } else {
            Err(NotReady::Unanswered {
                detail: self.to_string(),
            })
        }
    }
}

/// Ask the daemon at `url` for the policy verdict under `home`.
///
/// Why (R2): a slow or absent daemon costs at most [`VERDICT_QUERY_TIMEOUT`]
/// before the inline load. #6288: the query goes over the daemon socket only.
/// What: `url` must be the `unix:` socket locator; anything else (a refusal
/// sentinel) is `Unreachable` with no I/O. Sends one `VerdictRequest` under
/// [`VERDICT_QUERY_TIMEOUT`]. A timeout is `TimedOut`; a socket that is absent
/// or refuses the dial is `Unreachable`; any other exchange failure (EOF, a
/// cut frame, a daemon that predates the method) is `Malformed`; a refusal is
/// `Status`; the result goes to [`parse`].
/// Test: `daemon_unreachable_falls_back_to_inline_load_never_allow`,
/// `daemon_timeout_falls_back_to_inline_load`,
/// `non_2xx_daemon_answer_denies`, `verdict_query_sends_its_home`,
/// `daemon_closing_without_a_reply_denies`,
/// `daemon_cutting_the_body_short_denies`,
/// `daemon_stalling_the_body_falls_back_to_inline_load`.
pub(crate) async fn verdict_query(url: &str, home: &Path) -> Result<DaemonVerdict, QueryError> {
    let Some(socket) = trusty_mpm::core::socket_from_locator(url) else {
        return Err(QueryError::Unreachable);
    };
    let request = VerdictRequest {
        home: home.to_path_buf(),
    };
    let response = DaemonClient::over_socket(socket)
        .post(VERDICT_ROUTE)
        .json(&request)
        .timeout(VERDICT_QUERY_TIMEOUT)
        .send()
        .await
        .map_err(|e| send_error(&e))?;
    let status = response.status();
    if !status.is_success() {
        return Err(QueryError::Status(status.as_u16()));
    }
    let body = response.text().await.map_err(|_| QueryError::Malformed)?;
    parse(body.as_bytes())
}

/// The [`QueryError`] for a failed exchange.
fn send_error(e: &DaemonCallError) -> QueryError {
    if e.is_timeout() {
        QueryError::TimedOut(VERDICT_QUERY_TIMEOUT)
    } else if e.is_connect() {
        QueryError::Unreachable
    } else {
        QueryError::Malformed
    }
}

/// Parse a 2xx reply body.
///
/// Why: only a reply that is exactly one `VerdictReply` may carry a verdict.
/// What: a body that does not parse, and a `NotReady` whose slug is not
/// `NOT_READY_CHECK`, are `Malformed`. A verdict for fingerprint 0 or older
/// than the snapshot's max age is `Unserved`, so the hook loads inline.
/// Test: `malformed_daemon_answer_denies`,
/// `zero_fingerprint_answer_falls_back_to_inline_load`,
/// `stale_daemon_answer_falls_back_to_inline_load`.
pub(crate) fn parse(body: &[u8]) -> Result<DaemonVerdict, QueryError> {
    let reply: VerdictReply = serde_json::from_slice(body).map_err(|_| QueryError::Malformed)?;
    let (fingerprint, age_ms, verdict) = match reply {
        VerdictReply::Unserved { why } => return Ok(DaemonVerdict::Unserved(why)),
        VerdictReply::Ready {
            fingerprint,
            age_ms,
        } => (fingerprint, age_ms, DaemonVerdict::Ready),
        VerdictReply::NotReady {
            slug,
            not_ready,
            fingerprint,
            age_ms,
        } => {
            if slug != NOT_READY_CHECK {
                return Err(QueryError::Malformed);
            }
            (fingerprint, age_ms, DaemonVerdict::NotReady(not_ready))
        }
    };
    // #8454 VC: the route refuses both; checked again on the hook's side.
    if fingerprint == 0 {
        return Ok(DaemonVerdict::Unserved(Unserved::NoFingerprint));
    }
    if Duration::from_millis(age_ms) > MAX_AGE {
        return Ok(DaemonVerdict::Unserved(Unserved::Stale));
    }
    Ok(verdict)
}

/// The policy readiness for a tainted gated call: the daemon's verdict, or
/// `inline` (the S4 `policy_ready`) where none is served.
///
/// Why (R2 fallbacks): no branch may make a call ready that `inline` would
/// refuse, and an answer the hook cannot read must not fall through.
/// What: no `home` runs `inline` (there is nothing to ask under). `Ready`
/// and `NotReady` stand; `Unserved`, no daemon and a timeout run `inline`; a non-2xx status or a malformed reply is
/// [`NotReady::Unanswered`], a `POLICY_CHECK` deny.
/// Test: `daemon_unreachable_falls_back_to_inline_load_never_allow`,
/// `malformed_daemon_answer_denies`, `ready_answer_skips_the_inline_load`,
/// `unserved_answers_fall_back_to_inline_load`,
/// `no_home_runs_the_inline_load_once`.
pub(crate) async fn cached_policy_ready(
    url: &str,
    home: Option<&Path>,
    inline: impl FnOnce() -> Result<(), NotReady>,
) -> Result<(), NotReady> {
    let Some(home) = home else {
        return inline();
    };
    match verdict_query(url, home).await {
        Ok(DaemonVerdict::Ready) => Ok(()),
        Ok(DaemonVerdict::NotReady(not_ready)) => Err(not_ready),
        Ok(DaemonVerdict::Unserved(_)) => inline(),
        Err(e) => e.readiness(inline),
    }
}

#[cfg(test)]
#[path = "egress_verdict_client_tests.rs"]
mod tests;
