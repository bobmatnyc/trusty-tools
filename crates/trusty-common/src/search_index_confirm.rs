//! Confirming a `search.index.create` the daemon never answered in time
//! (#7237).
//!
//! Why: [`super::CREATE_TIMEOUT`] caps the create at one second because it runs
//! on a session-launch hot path, and until now a create the daemon completed
//! LATE was indistinguishable from one it refused. On 2026-09-10 the `writing`
//! project's index had been cold-parked with 32,754 chunks. The create arrived
//! while the daemon was reloading it, the daemon registered the index 3.8 s
//! later, and by then the client had already recorded `NotConfirmed`, withheld
//! the id, and fired a reindex the daemon answered with `unknown index: writing`.
//! The session then ran its whole length unpinned, against an index that
//! existed.
//!
//! What: [`confirm_after_no_answer`] re-reads the daemon's registry until an
//! index whose `root_path` IS this tree appears, or [`CONFIRM_DEADLINE`]
//! elapses. It is reached ONLY from [`super::reconcile::CreateOutcome::Unanswered`]
//! — a call that timed out, that the peer hung up on, or that failed mid-read —
//! never from a refusal, so no daemon error can become a confirmed index. The
//! match is [`super::reconcile::index_id_serving_root`], the same root-identity
//! comparison the #6864 collision recovery uses, so a confirmation names an
//! index the daemon actually holds for this root rather than one it was merely
//! asked for.
//!
//! The trade-off this picks: a session launch pays roughly [`CONFIRM_DEADLINE`]
//! on top of the create's own budget — see that constant for why one in-flight
//! registry read can carry it a second further — and only when the daemon left
//! the create unanswered. An ordinary create answers in milliseconds and never reaches
//! here. A launch that exhausts the deadline is told so in one `warn` and still
//! withholds the pin — the daemon may finish registering afterwards, and nothing
//! here retries.
//!
//! Test: `confirm_within_stops_at_the_deadline_and_withholds`,
//! `confirm_within_never_confirms_an_index_at_another_root` below, plus
//! `a_late_create_is_confirmed_by_polling_the_registry`,
//! `a_create_the_daemon_hung_up_on_is_confirmed_by_polling_the_registry` and
//! `an_unconfirmed_registration_fires_no_reindex` in `search_index_tests.rs`.

use std::path::Path;
use std::time::{Duration, Instant};

use super::reconcile::{ListFailure, fetch_index_list, index_id_serving_root};

/// How long [`confirm_after_no_answer`] keeps asking, before giving up (#7237).
///
/// Sized against the failure it exists for: the measured cold reload took
/// 3.8 s from the client's first byte, of which the create's own
/// [`super::CREATE_TIMEOUT`] covers the first second. Four seconds leaves
/// margin over the rest without turning "the daemon is wedged" into a launch
/// that hangs.
///
/// Not the total wall time: the deadline is tested only after a registry read
/// returns, and each read carries its own one-second budget, so a read that
/// starts just under the deadline can push the confirm to roughly five seconds.
/// The alternative — cancelling a read mid-flight — would throw away the answer
/// this poll exists to get.
const CONFIRM_DEADLINE: Duration = Duration::from_secs(4);

/// How long [`confirm_after_no_answer`] waits between two registry reads.
///
/// A read that FAILS has already spent its own ~1 s budget, so this gap only
/// paces the case where the daemon answers promptly with a registry that does
/// not name this tree yet.
const CONFIRM_POLL_GAP: Duration = Duration::from_millis(250);

/// The id the daemon ended up registering for `root`, or `None` (#7237).
///
/// Why: see the module doc. A create the daemon never answered is not evidence
/// that it refused, and the registry is the one place that settles which of the
/// two happened.
/// What: polls `search.indexes.list` through [`fetch_index_list`] until
/// [`index_id_serving_root`] names an index for `root`, then returns that id —
/// which may differ from `index_id`, exactly as the #6864 recovery's does. Gives
/// up at [`CONFIRM_DEADLINE`] with one `warn` and `None`, which leaves the
/// caller's pin unadvanced. Never propagates an error and never sleeps past the
/// deadline.
/// Test: `confirm_within_stops_at_the_deadline_and_withholds`,
/// `a_late_create_is_confirmed_by_polling_the_registry`,
/// `a_create_the_daemon_hung_up_on_is_confirmed_by_polling_the_registry`.
pub(super) fn confirm_after_no_answer(
    socket: &Path,
    index_id: &str,
    root: &Path,
) -> Option<String> {
    confirm_within(socket, index_id, root, CONFIRM_DEADLINE, CONFIRM_POLL_GAP)
}

/// [`confirm_after_no_answer`] with the two budgets supplied.
///
/// Why: the real deadline is seconds long, so a test that drove it would spend
/// them. Taking both as parameters lets the give-up and wrong-root arms be
/// asserted in milliseconds while the one end-to-end test still exercises the
/// production constants.
/// What: read, match, sleep, repeat — the sleep is clamped to whatever is left
/// of `deadline` so no wait is started that outlives it, and the loop always
/// performs at least one read even when `deadline` is zero. `deadline` is tested
/// between reads, not during one, so the final read can still overrun it by its
/// own budget; see [`CONFIRM_DEADLINE`].
/// Test: `confirm_within_stops_at_the_deadline_and_withholds`,
/// `confirm_within_never_confirms_an_index_at_another_root`.
fn confirm_within(
    socket: &Path,
    index_id: &str,
    root: &Path,
    deadline: Duration,
    gap: Duration,
) -> Option<String> {
    let started = Instant::now();
    let mut reads: u32 = 0;
    loop {
        reads += 1;
        // #7237: matched on the TREE, so only an index the daemon really holds
        // for this root can confirm the registration.
        if let Some(body) = fetch_index_list(socket, ListFailure::Quiet)
            && let Some(registered) = index_id_serving_root(&body, root)
        {
            tracing::info!(
                "trusty-search registered {} as index '{registered}' after the create call \
                 went unanswered ({reads} registry read(s), {:?}); pinning it (#7237)",
                root.display(),
                started.elapsed()
            );
            return Some(registered);
        }
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            break;
        }
        std::thread::sleep(gap.min(deadline - elapsed));
    }
    tracing::warn!(
        "trusty-search has not registered {} after {reads} registry read(s) over {deadline:?}; \
         it may still be registering '{index_id}' in the background, but nothing here retries \
         and the id is withheld so nothing pins an index that may not exist (#7237)",
        root.display()
    );
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::uds_mock::{self, MockFuture, RpcError};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicU32, Ordering};

    /// Run `body` against a mock daemon answering every method through
    /// `handler`, on a socket this function owns.
    fn with_daemon<T>(
        handler: impl Fn(&str, serde_json::Value) -> MockFuture + Send + Sync + 'static,
        body: impl FnOnce(&Path) -> T,
    ) -> T {
        let dir = tempfile::tempdir().expect("tempdir for the mock socket");
        let socket = dir.path().join("s.sock");
        let daemon = uds_mock::spawn_blocking_at(socket.clone(), handler);
        let out = body(&socket);
        drop(daemon);
        out
    }

    /// An `indexes` listing naming one entry.
    fn listing(id: &str, root: &Path) -> serde_json::Value {
        serde_json::json!({
            "indexes": [{ "id": id, "root_path": root.to_string_lossy() }]
        })
    }

    /// A daemon that never registers this tree leaves the pin unadvanced
    /// (#7237).
    ///
    /// Why: requirement 3 of the fix — the withholding #5091 introduced must
    /// survive it. A create that went unanswered because the daemon is wedged,
    /// or because the id is genuinely never going to exist, must still end in
    /// `None`, and the poll must stop rather than run forever.
    /// What: a daemon whose registry stays empty; asserts `None` and that the
    /// loop performed more than one read and returned inside a bound well under
    /// the production deadline.
    /// Test: this test.
    #[test]
    fn confirm_within_stops_at_the_deadline_and_withholds() {
        let reads = Arc::new(AtomicU32::new(0));
        let counter = Arc::clone(&reads);
        let started = Instant::now();

        let confirmed = with_daemon(
            move |_method, _params| {
                counter.fetch_add(1, Ordering::SeqCst);
                Box::pin(async { Ok(serde_json::json!({ "indexes": [] })) })
            },
            |socket| {
                confirm_within(
                    socket,
                    "never-registered",
                    Path::new("/nonexistent/never/registered"),
                    Duration::from_millis(120),
                    Duration::from_millis(20),
                )
            },
        );

        assert_eq!(
            confirmed, None,
            "an index the daemon never registers must stay unpinned (#5091)"
        );
        assert!(
            reads.load(Ordering::SeqCst) > 1,
            "the confirm must poll rather than read once, saw {} read(s)",
            reads.load(Ordering::SeqCst)
        );
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "the poll must stop at its deadline, took {:?}",
            started.elapsed()
        );
    }

    /// A registry entry for ANOTHER tree never confirms this one (#7237).
    ///
    /// Why: the fail-open check. The whole confirm exists to turn a silent
    /// daemon into a pinnable id, and the one way that could go wrong is
    /// accepting some other index as evidence. The match is on the tree, so a
    /// busy daemon full of other projects confirms nothing here.
    /// What: a daemon whose registry names one index at an unrelated root;
    /// asserts `None`.
    /// Test: this test.
    #[test]
    fn confirm_within_never_confirms_an_index_at_another_root() {
        let confirmed = with_daemon(
            move |_method, _params| {
                let body = listing("someone-else", Path::new("/nonexistent/other/tree"));
                Box::pin(async move { Ok(body) })
            },
            |socket| {
                confirm_within(
                    socket,
                    "mine",
                    Path::new("/nonexistent/my/tree"),
                    Duration::from_millis(60),
                    Duration::from_millis(20),
                )
            },
        );

        assert_eq!(
            confirmed, None,
            "an index registered at a DIFFERENT tree is not this registration"
        );
    }

    /// A refusing registry is a failed read, not a confirmation (#7237).
    ///
    /// Why: the second half of the fail-open check — a daemon ERROR must never
    /// read as "the index is there". [`fetch_index_list`] answers `None` for a
    /// refusal, and the loop has to treat that as "not yet", not as a match.
    /// What: a daemon that refuses every method; asserts `None`.
    /// Test: this test.
    #[test]
    fn confirm_within_treats_a_refusing_registry_as_no_answer() {
        let confirmed = with_daemon(
            move |_method, _params| {
                Box::pin(async { Err(RpcError::internal("the registry is unavailable")) })
            },
            |socket| {
                confirm_within(
                    socket,
                    "mine",
                    Path::new("/nonexistent/my/tree"),
                    Duration::from_millis(60),
                    Duration::from_millis(20),
                )
            },
        );

        assert_eq!(confirmed, None, "a daemon error confirms nothing");
    }
}
