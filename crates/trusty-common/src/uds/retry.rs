//! Bounded connect retry for the shared UDS client (#8267).
//!
//! Why: [`super::rpc::send_framed_request_capped`] dialled once. A healthy
//! daemon under load still refuses or half-opens that single connect — on
//! macOS a bound listener whose accept queue is full answers ECONNREFUSED
//! (`unp_connect` rejects once `so_qlen >= so_qlimit`, measured in
//! [`super::probe`]), and a connect that wins the race but loses the accept
//! surfaces as `ENOTCONN` on the first write. One attempt turned that
//! microsecond-wide window into a hard `-32603` for the whole MCP session,
//! because nothing in the chain ever dialled again. A stdio bridge launched
//! before its daemon is listening hit the same wall from the other side: the
//! socket file does not exist yet, so the dial fails with ENOENT and the
//! bridge is dead for the session.
//!
//! What: [`ConnectRetry`] states the bound — how many attempts, and the
//! doubling backoff between them — and [`with_connect_retry`] drives it. Only
//! a provably transient dial failure is retried; a security refusal, an
//! encode failure, or anything past the request bytes leaving this process is
//! terminal on the first attempt. Every attempt logs, and an exhausted retry
//! returns [`super::UdsRpcError::ConnectRetriesExhausted`], which names the
//! socket, the attempt count and the last OS error. It never degrades to a
//! warning-plus-success: the caller gets an `Err` or a real connection.
//!
//! The sleeper is the caller's, so a test drives the schedule on a virtual
//! clock rather than on wall time.
//!
//! Test: `backoff_doubles_and_stops_at_the_ceiling`,
//! `retry_stops_after_exactly_the_policys_attempt_count`,
//! `retry_returns_the_first_success_without_further_attempts`,
//! `a_non_transient_failure_is_not_retried`,
//! `a_late_non_transient_failure_is_reported_verbatim`,
//! `a_failed_half_close_is_never_retried`,
//! `a_single_attempt_policy_returns_the_underlying_error_unwrapped`,
//! plus the client-level `uds_client_*` tests in [`super::rpc`].

use std::future::Future;
use std::io::ErrorKind;
use std::path::Path;
use std::time::Duration;

use super::UdsSecurityError;
use super::rpc::UdsRpcError;

/// How many times a transient dial is retried, and how long between tries.
///
/// Why a value rather than a constant: the two call shapes want different
/// bounds. A per-request dial happens while an operator waits, so its budget
/// has to stay inside the tightest caller timeout in the workspace (200 ms);
/// a stdio bridge's first dial happens inside the MCP client's initialize
/// window and can afford seconds, because the alternative is a dead session.
///
/// What: `attempts` is the TOTAL number of dials, not the number of retries —
/// `attempts: 1` disables the retry entirely. The delay after attempt `n` is
/// `initial_backoff * 2^(n-1)`, clamped to `max_backoff`.
///
/// Test: `backoff_doubles_and_stops_at_the_ceiling`,
/// `backoff_floor_sums_every_delay_the_policy_will_sleep`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnectRetry {
    /// Total dials, including the first. Values below 1 are treated as 1.
    pub attempts: u32,
    /// Delay after the first failed attempt.
    pub initial_backoff: Duration,
    /// Ceiling the doubling backoff never exceeds.
    pub max_backoff: Duration,
}

impl ConnectRetry {
    /// The bound every ordinary request dial uses.
    ///
    /// Why 3 × 20 ms: the accept queue this exists to ride out drains in
    /// microseconds, so a short schedule covers it. The 60 ms floor
    /// (20 + 40) fits inside the tightest UDS timeout in the workspace —
    /// `trusty-audit`'s 200 ms `CONNECT_TIMEOUT` — so no existing caller
    /// trades a named dial failure for a timeout.
    #[must_use]
    pub const fn per_request() -> Self {
        Self {
            attempts: 3,
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(100),
        }
    }

    /// The bound a process's FIRST dial uses, when the daemon may still be
    /// coming up.
    ///
    /// Why 8 attempts from 100 ms: the 2.7-second floor is long enough for a
    /// daemon started in the same instant as its bridge to bind its socket, and
    /// short enough that a genuinely absent daemon is reported well inside an
    /// MCP client's initialize window rather than at the end of it. See
    /// `trusty_mcp::DaemonBridgeJsonRpc::forward`, which spends this budget
    /// once and then drops to [`ConnectRetry::per_request`].
    #[must_use]
    pub const fn startup() -> Self {
        Self {
            attempts: 8,
            initial_backoff: Duration::from_millis(100),
            max_backoff: Duration::from_millis(500),
        }
    }

    /// No retry — one dial, and its error verbatim.
    ///
    /// For a caller whose own loop already owns the retry decision.
    #[must_use]
    pub const fn single_attempt() -> Self {
        Self {
            attempts: 1,
            initial_backoff: Duration::ZERO,
            max_backoff: Duration::ZERO,
        }
    }

    /// The delay this policy sleeps after failed attempt `attempt` (1-based).
    ///
    /// Test: `backoff_doubles_and_stops_at_the_ceiling`.
    #[must_use]
    pub fn backoff_after(&self, attempt: u32) -> Duration {
        // 2^31 ms is already far past any sane ceiling, and clamping the shift
        // keeps `checked_pow` from being the thing that decides the answer.
        let shift = attempt.saturating_sub(1).min(31);
        self.initial_backoff
            .checked_mul(1u32 << shift)
            .unwrap_or(self.max_backoff)
            .min(self.max_backoff)
    }

    /// Total time this policy sleeps if every attempt fails.
    ///
    /// Why it is public: a test asserting "the bound was actually honoured"
    /// needs the figure without restating the schedule, and a caller sizing its
    /// own timeout needs it for the same reason.
    ///
    /// Test: `backoff_floor_sums_every_delay_the_policy_will_sleep`.
    #[must_use]
    pub fn backoff_floor(&self) -> Duration {
        (1..self.attempts.max(1))
            .map(|attempt| self.backoff_after(attempt))
            .sum()
    }
}

impl Default for ConnectRetry {
    fn default() -> Self {
        Self::per_request()
    }
}

/// Run `attempt` until it succeeds or the policy's bound is spent.
///
/// Why the sleeper is a parameter: the regression tests assert the attempt
/// count and the summed backoff, and doing that against `tokio::time::sleep`
/// would make them wall-clock tests of a schedule measured in milliseconds.
/// The injected sleeper also lets a test make the socket appear between two
/// attempts, which is the exact sequence #8267 is about.
///
/// What: attempt `n` runs, and on a transient failure the policy's delay for
/// `n` is slept before `n + 1`. A non-transient failure returns immediately —
/// retrying a socket this process refuses to trust would only delay a loud
/// refusal. Once more than one attempt has been made, the failure is reported
/// as [`UdsRpcError::ConnectRetriesExhausted`] so the count and the last OS
/// error travel with it; a single-attempt policy returns the underlying error
/// unchanged, which is what every caller saw before #8267.
///
/// # Errors
///
/// The last attempt's error, wrapped per above. Never `Ok` without a value the
/// attempt itself produced.
///
/// Test: `retry_stops_after_exactly_the_policys_attempt_count`,
/// `retry_returns_the_first_success_without_further_attempts`,
/// `a_non_transient_failure_is_not_retried`,
/// `a_single_attempt_policy_returns_the_underlying_error_unwrapped`.
pub(super) async fn with_connect_retry<T, A, AFut, S, SFut>(
    path: &Path,
    policy: ConnectRetry,
    sleep: S,
    mut attempt: A,
) -> Result<T, UdsRpcError>
where
    A: FnMut(u32) -> AFut,
    AFut: Future<Output = Result<T, UdsRpcError>>,
    S: Fn(Duration) -> SFut,
    SFut: Future<Output = ()>,
{
    let total = policy.attempts.max(1);
    let mut n: u32 = 1;
    // Kept only so the recovery line can name what the earlier attempts hit —
    // #8267's whole complaint is that the failure left no trace an operator
    // could read afterwards.
    let mut last_failure: Option<String> = None;
    loop {
        let err = match attempt(n).await {
            Ok(value) => {
                if let Some(last) = last_failure {
                    // WARN, not DEBUG: default verbosity is warn, so a DEBUG
                    // line records the recovery nowhere an operator will see.
                    tracing::warn!(
                        socket = %path.display(),
                        attempts = n,
                        last_error = %last,
                        "unix socket dial succeeded after a bounded retry"
                    );
                }
                return Ok(value);
            }
            Err(err) => err,
        };

        // A failure that is not transient is the caller's answer as it stands.
        // Wrapping it would report an attempt count below the bound and imply a
        // retry that never happened.
        if !is_transient(&err) {
            return Err(err);
        }

        if n >= total {
            if n == 1 {
                return Err(err);
            }
            // ERROR, not WARN: only ERROR reaches `errors.jsonl`, and a stdio
            // bridge's stderr is swallowed by its MCP client. This line is the
            // after-the-fact trace #8267 exists to produce.
            tracing::error!(
                socket = %path.display(),
                attempts = n,
                error = %err,
                "unix socket dial failed after every bounded retry"
            );
            return Err(UdsRpcError::ConnectRetriesExhausted {
                path: path.to_path_buf(),
                attempts: n,
                source: Box::new(err),
            });
        }

        let delay = policy.backoff_after(n);
        tracing::debug!(
            socket = %path.display(),
            attempt = n,
            attempts = total,
            delay_ms = delay.as_millis() as u64,
            error = %err,
            "unix socket dial failed; retrying after backoff"
        );
        last_failure = Some(err.to_string());
        sleep(delay).await;
        n += 1;
    }
}

/// Whether this failure means "try again", as opposed to "stop".
///
/// Why the set is this narrow: a retry is only safe while the request bytes
/// have provably not reached the peer.
///
/// A dial that never completed qualifies. So does `ENOTCONN` from
/// [`UdsRpcError::Write`], which since #8267 covers the `write_all` + `flush`
/// phase ONLY — a failure there leaves the peer without a newline-terminated
/// frame, and the server frames on `read_until(b'\n')`, so it never dispatches.
///
/// [`UdsRpcError::HalfClose`] is deliberately absent even at the same errno.
/// That phase runs after the frame is on the wire, and macOS answers ENOTCONN
/// there once the peer has closed — which is what a server that read the frame,
/// replied and dropped looks like. Retrying it would deliver a second copy of a
/// request the daemon had already executed. Every read failure is out for the
/// same reason.
///
/// Test: `a_non_transient_failure_is_not_retried`,
/// `transient_classification_covers_the_three_dial_errnos`,
/// `a_failed_half_close_is_never_retried`.
fn is_transient(err: &UdsRpcError) -> bool {
    match err {
        UdsRpcError::Dial { source, .. } => dial_is_transient(source),
        UdsRpcError::Write { source, .. } => source.kind() == ErrorKind::NotConnected,
        _ => false,
    }
}

/// The dial half of [`is_transient`]: ENOENT before the socket exists,
/// ECONNREFUSED / ENOTCONN / EAGAIN from a listener that is up but saturated.
///
/// A security refusal (wrong mode, wrong owner, not a socket) is never
/// transient — it describes the file, not the moment.
fn dial_is_transient(source: &UdsSecurityError) -> bool {
    match source {
        UdsSecurityError::Connect { source, .. } => matches!(
            source.kind(),
            ErrorKind::ConnectionRefused
                | ErrorKind::NotConnected
                | ErrorKind::WouldBlock
                | ErrorKind::Interrupted
        ),
        UdsSecurityError::StatForConnect { source, .. } => source.kind() == ErrorKind::NotFound,
        _ => false,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;
    use std::path::PathBuf;

    fn sock() -> PathBuf {
        PathBuf::from("/tmp/trusty-retry-test.sock")
    }

    fn transient() -> UdsRpcError {
        UdsRpcError::Dial {
            path: sock(),
            source: UdsSecurityError::Connect {
                path: sock(),
                source: std::io::Error::from(ErrorKind::ConnectionRefused),
            },
        }
    }

    fn permanent() -> UdsRpcError {
        UdsRpcError::Dial {
            path: sock(),
            source: UdsSecurityError::UntrustedSocket {
                path: sock(),
                reason: "socket is mode 0755, not 0600".to_string(),
            },
        }
    }

    #[test]
    fn backoff_doubles_and_stops_at_the_ceiling() {
        let policy = ConnectRetry {
            attempts: 6,
            initial_backoff: Duration::from_millis(20),
            max_backoff: Duration::from_millis(100),
        };
        let schedule: Vec<u64> = (1..=5)
            .map(|n| policy.backoff_after(n).as_millis() as u64)
            .collect();
        assert_eq!(schedule, vec![20, 40, 80, 100, 100]);
    }

    #[test]
    fn backoff_floor_sums_every_delay_the_policy_will_sleep() {
        // Three attempts means two sleeps, never three.
        assert_eq!(
            ConnectRetry::per_request().backoff_floor(),
            Duration::from_millis(60)
        );
        assert_eq!(
            ConnectRetry::single_attempt().backoff_floor(),
            Duration::ZERO
        );
    }

    #[test]
    fn transient_classification_covers_the_three_dial_errnos() {
        for kind in [
            ErrorKind::ConnectionRefused,
            ErrorKind::NotConnected,
            ErrorKind::WouldBlock,
            ErrorKind::Interrupted,
        ] {
            let err = UdsRpcError::Dial {
                path: sock(),
                source: UdsSecurityError::Connect {
                    path: sock(),
                    source: std::io::Error::from(kind),
                },
            };
            assert!(is_transient(&err), "{kind:?} should be retried");
        }
        let absent = UdsRpcError::Dial {
            path: sock(),
            source: UdsSecurityError::StatForConnect {
                path: sock(),
                source: std::io::Error::from(ErrorKind::NotFound),
            },
        };
        assert!(is_transient(&absent), "an absent socket should be retried");

        // A read failure is never retried: the peer may already have acted.
        assert!(!is_transient(&UdsRpcError::NoResponse { path: sock() }));
        assert!(!is_transient(&UdsRpcError::Write {
            path: sock(),
            source: std::io::Error::from(ErrorKind::BrokenPipe),
        }));
        assert!(is_transient(&UdsRpcError::Write {
            path: sock(),
            source: std::io::Error::from(ErrorKind::NotConnected),
        }));
    }

    /// The duplicate-delivery guard: the same errno means opposite things in
    /// the send phase and the half-close phase, and only the send phase may be
    /// repeated.
    #[test]
    fn a_failed_half_close_is_never_retried() {
        for kind in [
            ErrorKind::NotConnected,
            ErrorKind::BrokenPipe,
            ErrorKind::ConnectionReset,
        ] {
            let err = UdsRpcError::HalfClose {
                path: sock(),
                source: std::io::Error::from(kind),
            };
            assert!(
                !is_transient(&err),
                "the frame is already on the wire; {kind:?} must not be retried"
            );
        }
    }

    /// A non-transient failure on attempt 2 reports itself, not an exhausted
    /// retry — the bound was never spent, and saying "2 attempts failed" would
    /// claim a count below the policy's own.
    #[tokio::test]
    async fn a_late_non_transient_failure_is_reported_verbatim() {
        let seen: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let err = with_connect_retry::<(), _, _, _, _>(
            &sock(),
            ConnectRetry::per_request(),
            |_d| async {},
            |n| {
                seen.borrow_mut().push(n);
                async move {
                    if n == 1 {
                        Err(transient())
                    } else {
                        Err(permanent())
                    }
                }
            },
        )
        .await
        .expect_err("the second attempt fails permanently");

        assert_eq!(seen.into_inner(), vec![1, 2]);
        assert!(
            matches!(err, UdsRpcError::Dial { .. }),
            "expected the permanent error verbatim, got {err:?}"
        );
    }

    #[tokio::test]
    async fn retry_stops_after_exactly_the_policys_attempt_count() {
        let slept: RefCell<Vec<Duration>> = RefCell::new(Vec::new());
        let seen: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let policy = ConnectRetry::per_request();

        let err = with_connect_retry::<(), _, _, _, _>(
            &sock(),
            policy,
            |d| {
                slept.borrow_mut().push(d);
                async {}
            },
            |n| {
                seen.borrow_mut().push(n);
                async { Err(transient()) }
            },
        )
        .await
        .expect_err("every attempt fails");

        assert_eq!(seen.into_inner(), vec![1, 2, 3]);
        let slept = slept.into_inner();
        assert_eq!(
            slept,
            vec![Duration::from_millis(20), Duration::from_millis(40)]
        );
        assert_eq!(
            slept.iter().sum::<Duration>(),
            policy.backoff_floor(),
            "the driver must sleep the policy's whole floor"
        );
        assert!(
            matches!(&err, UdsRpcError::ConnectRetriesExhausted { attempts, .. } if *attempts == 3),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn retry_returns_the_first_success_without_further_attempts() {
        let seen: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let got = with_connect_retry(
            &sock(),
            ConnectRetry::per_request(),
            |_d| async {},
            |n| {
                seen.borrow_mut().push(n);
                async move { if n < 2 { Err(transient()) } else { Ok(n) } }
            },
        )
        .await
        .expect("the second attempt succeeds");

        assert_eq!(got, 2);
        assert_eq!(seen.into_inner(), vec![1, 2]);
    }

    #[tokio::test]
    async fn a_non_transient_failure_is_not_retried() {
        let seen: RefCell<Vec<u32>> = RefCell::new(Vec::new());
        let err = with_connect_retry::<(), _, _, _, _>(
            &sock(),
            ConnectRetry::startup(),
            |_d| async { panic!("a permanent refusal must not sleep") },
            |n| {
                seen.borrow_mut().push(n);
                async { Err(permanent()) }
            },
        )
        .await
        .expect_err("a security refusal is terminal");

        assert_eq!(seen.into_inner(), vec![1]);
        assert!(matches!(err, UdsRpcError::Dial { .. }), "got {err:?}");
    }

    #[tokio::test]
    async fn a_single_attempt_policy_returns_the_underlying_error_unwrapped() {
        let err = with_connect_retry::<(), _, _, _, _>(
            &sock(),
            ConnectRetry::single_attempt(),
            |_d| async { panic!("a single-attempt policy must not sleep") },
            |_n| async { Err(transient()) },
        )
        .await
        .expect_err("one attempt, one error");

        assert!(matches!(err, UdsRpcError::Dial { .. }), "got {err:?}");
    }
}
