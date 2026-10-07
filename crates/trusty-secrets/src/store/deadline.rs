//! The request deadline a server body runs under, read by the CLI runner
//! (#7524 P2-M1).
//!
//! Why: a client waited 30 s for a reply while one vendor CLI call could
//! take 60 s and a write several of them, all inside the index lock. The
//! client reported a transport timeout, and then the server committed the
//! write. One whole-operation deadline per request, shorter than the
//! client's wait, has to reach every CLI call the request makes. The
//! [`crate::store::SecretBackend`] trait is synchronous and carries no
//! deadline, so the server sets it for the thread its method body runs on.
//! What: [`within`] runs a closure with a deadline set for the current
//! thread and restores the previous one afterwards, panic included;
//! [`remaining`] is the time left, `None` outside any request; [`passed`]
//! says whether it has run out. A body runs on one blocking-pool thread, and
//! every backend call it makes runs synchronously on that thread.
//! Test: `deadline_within_sets_and_restores_the_thread_deadline`,
//! `runner_refuses_to_spawn_after_the_request_deadline`,
//! `runner_kills_the_cli_at_the_request_deadline`.

use std::cell::Cell;
use std::time::{Duration, Instant};

thread_local! {
    static DEADLINE: Cell<Option<Instant>> = const { Cell::new(None) };
}

/// Restores the thread's previous deadline on drop.
struct Restore(Option<Instant>);

impl Drop for Restore {
    fn drop(&mut self) {
        DEADLINE.set(self.0);
    }
}

/// Run `f` with `deadline` as this thread's request deadline.
///
/// What: an enclosing deadline that is earlier wins. The previous deadline
/// is restored when `f` returns or unwinds.
// #7524 P2-M1: only the server's router sets one; a `cli-backends` build
// without `server` reaches it from tests alone.
#[cfg_attr(not(all(unix, feature = "server")), allow(dead_code))]
pub(crate) fn within<T>(deadline: Instant, f: impl FnOnce() -> T) -> T {
    let previous = DEADLINE.get();
    let effective = previous.map_or(deadline, |p| p.min(deadline));
    let _restore = Restore(previous);
    DEADLINE.set(Some(effective));
    f()
}

/// Time left before this thread's request deadline; `None` outside a request.
pub(crate) fn remaining() -> Option<Duration> {
    DEADLINE
        .get()
        .map(|deadline| deadline.saturating_duration_since(Instant::now()))
}

/// Whether this thread's request deadline has run out.
#[cfg_attr(not(all(unix, feature = "server")), allow(dead_code))]
pub(crate) fn passed() -> bool {
    remaining() == Some(Duration::ZERO)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: #7524 P2-M1 — a deadline left behind on a pooled thread would cut
    /// short the next request that thread serves.
    /// Test: itself.
    #[test]
    fn deadline_within_sets_and_restores_the_thread_deadline() {
        assert_eq!(remaining(), None);
        let later = Instant::now() + Duration::from_secs(60);
        let sooner = Instant::now() + Duration::from_secs(1);
        within(later, || {
            assert!(remaining().is_some_and(|left| left > Duration::from_secs(30)));
            // An inner, later deadline never extends the outer one.
            within(later + Duration::from_secs(60), || {
                assert!(remaining().is_some_and(|left| left <= Duration::from_secs(60)));
            });
            within(sooner, || {
                assert!(remaining().is_some_and(|left| left <= Duration::from_secs(1)));
            });
            assert!(remaining().is_some_and(|left| left > Duration::from_secs(30)));
        });
        assert_eq!(remaining(), None);
        within(Instant::now(), || assert!(passed()));
        let unwound = std::panic::catch_unwind(|| within(later, || panic!("unwind")));
        assert!(unwound.is_err());
        assert_eq!(remaining(), None, "a panic left the deadline set");
    }
}
