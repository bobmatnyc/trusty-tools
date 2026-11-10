//! #10738: a test-only park point inside the staged-swap detach windows.
//!
//! Why: between `take_corpus_store` and the re-attach, the abort and the
//! promotion run with no corpus wired and no indexer lock held. A test has to
//! act inside that span deterministically, without a sleep.
//! What: a test registers a [`Parked`] for one index and [`Window`]; the swap
//! code calls [`at`] right after the take. `at` signals `reached`, then waits
//! for `release`. A dropped `release` panics the run inside the window, as a
//! panicked or cancelled reindex task would. An index with nothing registered
//! passes straight through.
//! Test: the detach-window cases in `identity_10738_tests`.

use std::collections::HashMap;
use std::sync::{Mutex, OnceLock};

use tokio::sync::oneshot;

/// Which detach window to park in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub(crate) enum Window {
    /// `abort_staged_corpus_swap`, after the staging store is taken.
    Abort,
    /// `commit_staged_corpus_swap`, after the staging store is released.
    Promotion,
}

struct Park {
    reached: oneshot::Sender<()>,
    release: oneshot::Receiver<()>,
}

type Parks = Mutex<HashMap<(String, Window), Park>>;

fn parks() -> &'static Parks {
    static PARKS: OnceLock<Parks> = OnceLock::new();
    PARKS.get_or_init(Parks::default)
}

/// The test's half of a park: await `reached`, then send or drop `release`.
pub(crate) struct Parked {
    pub(crate) reached: oneshot::Receiver<()>,
    pub(crate) release: oneshot::Sender<()>,
}

/// Park the next run of `window` for `index_id`.
pub(crate) fn park(index_id: &str, window: Window) -> Parked {
    let (reached_tx, reached_rx) = oneshot::channel();
    let (release_tx, release_rx) = oneshot::channel();
    parks().lock().unwrap_or_else(|e| e.into_inner()).insert(
        (index_id.to_string(), window),
        Park {
            reached: reached_tx,
            release: release_rx,
        },
    );
    Parked {
        reached: reached_rx,
        release: release_tx,
    }
}

/// Park here when a test registered `window` for `index_id`.
pub(super) async fn at(index_id: &str, window: Window) {
    let park = parks()
        .lock()
        .unwrap_or_else(|e| e.into_inner())
        .remove(&(index_id.to_string(), window));
    let Some(park) = park else {
        return;
    };
    let _ = park.reached.send(());
    if park.release.await.is_err() {
        panic!("#10738 test: the staged run died in the {window:?} detach window");
    }
}
