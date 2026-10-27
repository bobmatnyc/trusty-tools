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
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

/// A step-mode pace's ends: where each point's index goes, and its go signal.
type StepEnds = (Sender<usize>, Arc<Mutex<Receiver<()>>>);

struct Pace {
    delay: Duration,
    park_at: Option<usize>,
    /// Index of the next point this pace sees.
    next: usize,
    reached: Option<Sender<()>>,
    release: Option<Receiver<()>>,
    /// #10335: step mode; every point reports its index and waits for a go.
    step: Option<StepEnds>,
}

/// The test's end of a step-mode pace (#10335): `reached` yields each
/// point's index as the loop stops before it; one send on `go` lets it index
/// that point. Dropping `go` lets every remaining point run.
pub(crate) struct Steps {
    pub(crate) reached: Receiver<usize>,
    pub(crate) go: Sender<()>,
}

/// Stop before every point of `drawer`'s next group writes until the test
/// sends a go (#10335), so a test decides what the clock reads between ticks.
pub(crate) fn arm_steps(drawer: &str) -> Steps {
    let (reached_tx, reached) = channel();
    let (go, go_rx) = channel();
    let pace = Pace {
        delay: Duration::ZERO,
        park_at: None,
        next: 0,
        reached: None,
        release: None,
        step: Some((reached_tx, Arc::new(Mutex::new(go_rx)))),
    };
    install(drawer, pace);
    Steps { reached, go }
}

fn install(drawer: &str, pace: Pace) {
    let mut paces = PACES.lock().unwrap_or_else(PoisonError::into_inner);
    paces.retain(|(d, _)| d != drawer);
    paces.push((drawer.to_string(), pace));
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
        next: 0,
        reached: Some(reached_tx),
        release: Some(release_rx),
        step: None,
    };
    install(drawer, pace);
    Parked { reached, release }
}

/// Called by the index loop before each point of `drawer`.
pub(super) fn pace(drawer: &str) {
    let (delay, park, step) = {
        let mut paces = PACES.lock().unwrap_or_else(PoisonError::into_inner);
        let Some((_, p)) = paces.iter_mut().find(|(d, _)| d == drawer) else {
            return;
        };
        let n = p.next;
        p.next += 1;
        let park = (p.park_at == Some(n)).then(|| (p.reached.take(), p.release.take()));
        let step = p
            .step
            .as_ref()
            .map(|(tx, go)| (n, tx.clone(), Arc::clone(go)));
        (p.delay, park, step)
    };
    if let Some((n, reached, go)) = step {
        // A dropped receiver or sender means the test is done; run freely.
        if reached.send(n).is_ok() {
            let _ = go.lock().unwrap_or_else(PoisonError::into_inner).recv();
        }
    }
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
