//! Tests for [`super::MaintenanceLease`] (#8733).
//!
//! Each lease opens its own descriptor, and `flock` conflicts per open file
//! description, so two leases in one process compete exactly as two
//! processes would.

use std::sync::{Arc, Barrier, Mutex};

use super::{LeaseStatus, MAINTENANCE_LOCK_FILE, MaintenanceLease, recorded_holder_pid};

/// Two leases on one root, raced from two threads: exactly one is held, and
/// the loser names the winner's pid.
#[test]
fn only_one_of_two_leases_on_a_root_is_held() {
    let root = tempfile::tempdir().expect("tempdir");
    let leases = [
        Arc::new(MaintenanceLease::new(root.path())),
        Arc::new(MaintenanceLease::new(root.path())),
    ];
    let barrier = Arc::new(Barrier::new(2));
    let joins: Vec<_> = leases
        .iter()
        .map(|lease| {
            let (lease, barrier) = (Arc::clone(lease), Arc::clone(&barrier));
            std::thread::spawn(move || {
                barrier.wait();
                lease.try_hold()
            })
        })
        .collect();
    let statuses: Vec<LeaseStatus> = joins
        .into_iter()
        .map(|j| j.join().expect("thread"))
        .collect();

    assert_eq!(
        statuses.iter().filter(|s| s.is_held()).count(),
        1,
        "exactly one lease may be held: {statuses:?}"
    );
    let pid = Some(std::process::id());
    assert!(
        statuses.contains(&LeaseStatus::HeldElsewhere { holder_pid: pid }),
        "the loser must report the holder's pid: {statuses:?}"
    );
    // The holder keeps it: asking again stays held, and the loser stays out.
    let holder = statuses
        .iter()
        .position(LeaseStatus::is_held)
        .expect("one held");
    assert!(leases[holder].try_hold().is_held());
    assert!(!leases[1 - holder].try_hold().is_held());
    assert_eq!(recorded_holder_pid(leases[holder].path()), pid);
}

/// A holder that goes away — the kernel drops `flock` when a process exits
/// or crashes, which dropping the lease reproduces — is replaced on the next
/// attempt.
#[test]
fn a_released_lease_is_taken_over() {
    let root = tempfile::tempdir().expect("tempdir");
    let first = MaintenanceLease::new(root.path());
    let second = MaintenanceLease::new(root.path());
    assert!(first.try_hold().is_held());
    assert!(!second.try_hold().is_held());

    drop(first);

    assert!(
        second.try_hold().is_held(),
        "the survivor must take over once the holder is gone"
    );
}

/// A lock file that cannot be created fails closed and says so at warn.
#[test]
fn an_uncreatable_lock_file_fails_closed() {
    use tracing_subscriber::layer::SubscriberExt as _;

    #[derive(Clone, Default)]
    struct Warns(Arc<Mutex<Vec<String>>>);
    impl<S: tracing::Subscriber> tracing_subscriber::Layer<S> for Warns {
        fn on_event(
            &self,
            event: &tracing::Event<'_>,
            _: tracing_subscriber::layer::Context<'_, S>,
        ) {
            if *event.metadata().level() == tracing::Level::WARN {
                let mut text = String::new();
                event.record(&mut |f: &tracing::field::Field, v: &dyn std::fmt::Debug| {
                    text.push_str(&format!("{}={v:?} ", f.name()));
                });
                self.0.lock().expect("warn log").push(text);
            }
        }
    }

    let root = tempfile::tempdir().expect("tempdir");
    // The parent directory does not exist, so the lock file cannot be created.
    let lease = MaintenanceLease::new(&root.path().join("missing"));
    let warns = Warns::default();
    let seen = Arc::clone(&warns.0);
    let subscriber = tracing_subscriber::registry().with(warns);

    let (status, again) =
        tracing::subscriber::with_default(subscriber, || (lease.try_hold(), lease.try_hold()));

    assert!(
        matches!(status, LeaseStatus::Unavailable { .. }),
        "an uncreatable lock file must not be held: {status:?}"
    );
    assert!(!again.is_held(), "a retry must stay closed");
    assert!(!lease.path().exists());
    let warns = seen.lock().expect("warn log");
    assert_eq!(
        warns.len(),
        1,
        "one warn per status change, not per retry: {warns:?}"
    );
    assert!(
        warns[0].contains("fail closed") && warns[0].contains(MAINTENANCE_LOCK_FILE),
        "the warn must name the lock file and the fail-closed outcome: {warns:?}"
    );
}
