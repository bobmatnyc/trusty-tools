//! Tests for the `gh` poll gate (#6867).
//!
//! Why: the gate exists to STOP work, so every test here asserts that
//! something did NOT happen — no second child, no fourth poll. A guard whose
//! absence no test notices is not a guard, and each of these fails against
//! `origin/main`, where neither rule existed.
//! What: the single-flight rule under real thread contention, the
//! consecutive-timeout suspension and the sentence it hands the operator, the
//! reset on any answer, and the unwind path that must not strand a waiter.
//! No test spawns `gh` or any process — the gate takes a closure, so the
//! "child" is a counter.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use super::*;

/// A timed-out poll, as [`super::super::worktree_reclaim_gh::run_with_timeout`]
/// reports one.
fn timeout() -> Result<String, GhFailure> {
    Err(GhFailure::timeout("`gh` did not answer within 10s"))
}

/// 🔴 #6867 REGRESSION: two concurrent polls of the SAME query spawn ONE child.
///
/// Why: the leak was one orphan pair per poll, and the polls overlapped —
/// each new one fired while the previous was still blocked in the keychain.
/// On `origin/main` this counter reads 2, because nothing looked at what was
/// already in flight.
#[test]
fn two_concurrent_polls_spawn_one_child() {
    let gate = Arc::new(GhPollGate::new());
    let spawned = Arc::new(AtomicUsize::new(0));
    let started = Arc::new(std::sync::Barrier::new(2));

    let first = {
        let (gate, spawned, started) = (gate.clone(), spawned.clone(), started.clone());
        std::thread::spawn(move || {
            gate.poll(Path::new("/tmp/root"), "index", || {
                spawned.fetch_add(1, Ordering::SeqCst);
                // Hold the flight open until the second caller has had time to
                // arrive and park on it.
                started.wait();
                std::thread::sleep(std::time::Duration::from_millis(150));
                Ok("first call's answer".to_string())
            })
        })
    };

    // Wait until the first call is provably inside its closure, then join it.
    started.wait();
    let second = gate.poll(Path::new("/tmp/root"), "index", || {
        spawned.fetch_add(1, Ordering::SeqCst);
        Ok("a second child ran".to_string())
    });

    let first = first.join().expect("first poll thread");
    assert_eq!(
        spawned.load(Ordering::SeqCst),
        1,
        "an overlapping poll must join the in-flight call, not spawn a second `gh`"
    );
    assert_eq!(first.as_deref().ok(), Some("first call's answer"));
    assert_eq!(
        second.as_deref().ok(),
        Some("first call's answer"),
        "the waiter must receive the in-flight call's result"
    );
}

/// A DIFFERENT branch is a different question and must not be answered from
/// another branch's reply.
#[test]
fn a_different_call_for_the_same_root_is_not_shared() {
    let gate = GhPollGate::new();
    let root = Path::new("/tmp/root");
    let a = gate.poll(root, "head:feat/a", || Ok("A".to_string()));
    let b = gate.poll(root, "head:feat/b", || Ok("B".to_string()));
    assert_eq!(a.as_deref().ok(), Some("A"));
    assert_eq!(b.as_deref().ok(), Some("B"));
}

/// 🔴 #6867 REGRESSION: after three consecutive timeouts the fourth call is
/// SKIPPED, and says so in the sentence the doctor renders.
///
/// Why: this is the whole point. On `origin/main` the fourth call spawns
/// another `gh`, which blocks in the keychain, which leaks another pair.
#[test]
fn a_fourth_call_is_skipped_after_three_timeouts() {
    let gate = GhPollGate::new();
    let root = Path::new("/tmp/wedged");
    let spawned = AtomicUsize::new(0);

    for _ in 0..TIMEOUT_STRIKES {
        let out = gate.poll(root, "index", || {
            spawned.fetch_add(1, Ordering::SeqCst);
            timeout()
        });
        assert!(out.is_err(), "a timeout must be reported as a failure");
    }
    assert_eq!(spawned.load(Ordering::SeqCst), TIMEOUT_STRIKES as usize);

    let fourth = gate
        .poll(root, "index", || {
            spawned.fetch_add(1, Ordering::SeqCst);
            timeout()
        })
        .expect_err("a suspended root must refuse the call");
    assert_eq!(
        spawned.load(Ordering::SeqCst),
        TIMEOUT_STRIKES as usize,
        "the fourth call must spawn nothing"
    );
    let line = fourth.to_string();
    assert!(line.contains("gh polling suspended"), "{line}");
    assert!(line.contains("3 consecutive timeouts"), "{line}");
    assert!(line.contains("next retry at"), "{line}");
    assert!(
        !fourth.timed_out(),
        "a skip is not itself a timeout — counting it would make the suspension permanent"
    );
}

/// Suspension is per registry root: one wedged repository must not stop the
/// survey from reading a healthy one.
#[test]
fn suspension_does_not_spread_to_another_root() {
    let gate = GhPollGate::new();
    for _ in 0..TIMEOUT_STRIKES {
        let _ = gate.poll(Path::new("/tmp/wedged"), "index", timeout);
    }
    let other = gate.poll(Path::new("/tmp/healthy"), "index", || Ok("ok".to_string()));
    assert_eq!(other.as_deref().ok(), Some("ok"));
}

/// An answer — even a failing one — clears the strikes, because the process
/// returned and leaked nothing.
#[test]
fn an_answer_clears_the_strikes() {
    let gate = GhPollGate::new();
    let root = Path::new("/tmp/root");
    let _ = gate.poll(root, "index", timeout);
    let _ = gate.poll(root, "index", timeout);
    // An exit-4 auth failure: instant, not a hang.
    let _ = gate.poll(root, "index", || Err(GhFailure::new("`gh` exited 4: auth")));
    // Two more timeouts must not reach the threshold from here.
    let _ = gate.poll(root, "index", timeout);
    let _ = gate.poll(root, "index", timeout);

    let spawned = AtomicUsize::new(0);
    let out = gate.poll(root, "index", || {
        spawned.fetch_add(1, Ordering::SeqCst);
        Ok("ran".to_string())
    });
    assert_eq!(out.as_deref().ok(), Some("ran"));
    assert_eq!(
        spawned.load(Ordering::SeqCst),
        1,
        "the counter must have restarted at the answer, leaving only two strikes"
    );
}

/// An expired suspension lets the next call through rather than latching.
#[test]
fn an_expired_suspension_lets_the_next_call_through() {
    let mut state = GateState::default();
    let root = Path::new("/tmp/root");
    for _ in 0..TIMEOUT_STRIKES {
        note_outcome(&mut state.roots, root, &timeout());
    }
    let now = Instant::now();
    assert!(
        take_suspension(&mut state, root, now).is_some(),
        "the suspension must hold while its deadline is in the future"
    );
    let after = now + BACKOFF_MAX + Duration::from_secs(1);
    assert!(
        take_suspension(&mut state, root, after).is_none(),
        "an expired deadline must not keep refusing calls"
    );
    assert!(
        take_suspension(&mut state, root, now).is_none(),
        "the expired deadline must have been cleared, not re-evaluated"
    );
}

/// The interval grows per additional timeout and stops at the ceiling.
#[test]
fn backoff_grows_and_then_saturates() {
    assert_eq!(backoff_for(TIMEOUT_STRIKES), BACKOFF_BASE);
    assert_eq!(backoff_for(TIMEOUT_STRIKES + 1), BACKOFF_BASE * 2);
    assert_eq!(backoff_for(TIMEOUT_STRIKES + 2), BACKOFF_BASE * 4);
    assert_eq!(backoff_for(TIMEOUT_STRIKES + 40), BACKOFF_MAX);
    assert_eq!(backoff_for(u32::MAX), BACKOFF_MAX);
}

/// A panicking poll must release its waiters instead of parking them forever
/// on a call that will never complete.
#[test]
fn a_panicking_run_releases_its_waiters() {
    let gate = Arc::new(GhPollGate::new());
    let started = Arc::new(std::sync::Barrier::new(2));

    let panicker = {
        let (gate, started) = (gate.clone(), started.clone());
        std::thread::spawn(move || {
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                gate.poll(Path::new("/tmp/root"), "index", || {
                    started.wait();
                    std::thread::sleep(std::time::Duration::from_millis(100));
                    panic!("the poll blew up");
                })
            }));
        })
    };

    started.wait();
    let waiter = gate.poll(Path::new("/tmp/root"), "index", || {
        Ok("the waiter ran its own call".to_string())
    });
    panicker
        .join()
        .expect("the panicking thread must be joinable");

    // Either outcome is acceptable — the waiter may take the failure the drop
    // guard published, or (if it arrived after the slot cleared) run its own
    // call. What must never happen is a hang, which is what this test would
    // exhibit as a timeout rather than an assertion failure.
    match waiter {
        Ok(out) => assert_eq!(out, "the waiter ran its own call"),
        Err(f) => assert!(f.to_string().contains("panicked"), "{f}"),
    }
}

/// The process-wide gate is one instance, not one per call.
#[test]
fn the_shared_gate_is_a_single_instance() {
    assert!(std::ptr::eq(shared(), shared()));
}
