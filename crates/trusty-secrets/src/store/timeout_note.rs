//! What the last backend time-out on this thread was (#11215).
//!
//! Why: a Keychain call cut at its time limit folds into the dataless
//! `backend_timeout` kind, so neither the reply nor the audit row said how
//! long the call waited, or whether it was refused at once because an earlier
//! call on the item still hangs. The [`crate::store::SecretBackend`] trait and
//! the server's error kinds carry no such data, and a request body runs every
//! backend call synchronously on one thread (`store::deadline`).
//! What: [`note`] records a [`TimeoutNote`] for the current thread each time
//! the time limit builds a [`crate::api::SecretsError::Timeout`]; [`last`]
//! reads the latest one; [`scoped`] runs a request body with an empty note
//! and hands back the note it left. The note holds a backend id, a duration
//! and a flag: never a vault, key or value.
//! Test: `time_limited_timeout_notes_its_wait_and_an_abandoned_item_refusal`,
//! `server_keychain_timeout_reply_and_audit_name_the_wait_then_the_refusal`.

use std::cell::RefCell;
use std::time::Duration;

/// One backend time-out, as the time limit saw it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct TimeoutNote {
    /// The backend id, e.g. `keychain`.
    pub(crate) backend: String,
    /// How long this call waited; zero when it never started.
    pub(crate) waited: Duration,
    /// `true` when the call was refused at once because an earlier,
    /// abandoned call on the same item is still running.
    pub(crate) abandoned_item: bool,
}

thread_local! {
    static LAST: RefCell<Option<TimeoutNote>> = const { RefCell::new(None) };
}

/// Record a time-out on this thread; the latest one wins.
pub(crate) fn note(backend: String, waited: Duration, abandoned_item: bool) {
    LAST.set(Some(TimeoutNote {
        backend,
        waited,
        abandoned_item,
    }));
}

/// The latest time-out noted on this thread, if any.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) fn last() -> Option<TimeoutNote> {
    LAST.with_borrow(Clone::clone)
}

/// Run `f` with no note on this thread, then return its result and the note
/// it left. The thread's previous note is restored afterwards.
#[cfg_attr(not(feature = "server"), allow(dead_code))]
pub(crate) fn scoped<T>(f: impl FnOnce() -> T) -> (T, Option<TimeoutNote>) {
    let previous = LAST.take();
    let value = f();
    let left = LAST.replace(previous);
    (value, left)
}
