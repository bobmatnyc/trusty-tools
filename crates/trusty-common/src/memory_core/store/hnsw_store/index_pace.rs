//! #9728 test seam: pace the graph-index loop of one drawer's group write.
//!
//! Why: the op-budget tests need a group whose indexing is slow, or stalls at
//! a chosen point, without a 60 s wait or a 4096-point graph.
//! What: [`arm`] registers a per-point delay and an optional park point for a
//! drawer key; the group write calls [`pace`] before indexing each point.
//! Process-wide, because the write runs on a blocking thread; keyed by
//! drawer, so a test paces only its own group.
//! Test: `a_legal_group_slower_than_the_budget_completes_without_tripping`,
//! `a_stall_inside_a_large_group_trips_one_budget_after_the_stall`.

use std::sync::mpsc::{Receiver, Sender, channel};
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

struct Pace {
    delay: Duration,
    park_at: Option<usize>,
    reached: Option<Sender<()>>,
    release: Option<Receiver<()>>,
}

static PACES: Mutex<Vec<(String, Pace)>> = Mutex::new(Vec::new());

/// The test's end of a park: `reached` fires when the loop parks; dropping
/// (or sending on) `release` lets it go on.
pub(crate) struct Parked {
    pub(crate) reached: Receiver<()>,
    pub(crate) release: Sender<()>,
}

/// Sleep `delay` before indexing each point of `drawer`'s next group writes,
/// and park before point `park_at`, if given.
pub(crate) fn arm(drawer: &str, delay: Duration, park_at: Option<usize>) -> Parked {
    let (reached_tx, reached) = channel();
    let (release, release_rx) = channel();
    let pace = Pace {
        delay,
        park_at,
        reached: Some(reached_tx),
        release: Some(release_rx),
    };
    let mut paces = PACES.lock().unwrap_or_else(PoisonError::into_inner);
    paces.retain(|(d, _)| d != drawer);
    paces.push((drawer.to_string(), pace));
    Parked { reached, release }
}

/// Called by the index loop before point `n` of `drawer`.
pub(super) fn pace(drawer: &str, n: usize) {
    let (delay, park) = {
        let mut paces = PACES.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((_, p)) = paces.iter_mut().find(|(d, _)| d == drawer) else {
            return;
        };
        let park = (p.park_at == Some(n)).then(|| (p.reached.take(), p.release.take()));
        (p.delay, park)
    };
    if let Some((reached, release)) = park {
        if let Some(reached) = reached {
            let _ = reached.send(());
        }
        if let Some(release) = release {
            let _ = release.recv();
        }
    }
    std::thread::sleep(delay);
}
