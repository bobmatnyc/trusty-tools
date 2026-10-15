//! Tests for [`super::MaintenanceLease`] (#8733).
//!
//! Each lease opens its own descriptor, and `flock` conflicts per open file
//! description, so two leases in one process compete exactly as two
//! processes would.

use std::fs::{File, OpenOptions};
use std::io::Write as _;
use std::path::Path;
use std::sync::{Arc, Barrier, Mutex};
use std::time::{Duration, Instant};

use super::{LeaseStatus, MAINTENANCE_LOCK_FILE, MaintenanceLease, gate_path, recorded_holder_pid};

/// A pid no live holder has, seeded to catch a loser reading a stale file.
const STALE_PID: &str = "4000000000\n";

/// Races per call of `only_one_of_two_leases_on_a_root_is_held`: the pid
/// window is microseconds wide, so one race rarely lands in it (#8733).
const RACE_ROUNDS: usize = 50;

/// Two leases on one root, raced from two threads: exactly one is held, and
/// the loser names the winner's pid, never none and never a previous
/// holder's stale pid (#8733).
#[test]
fn only_one_of_two_leases_on_a_root_is_held() {
    for round in 0..RACE_ROUNDS {
        let root = tempfile::tempdir().expect("tempdir");
        // #8733: odd rounds start from a crashed holder's leftover pid.
        if round % 2 == 1 {
            std::fs::write(root.path().join(MAINTENANCE_LOCK_FILE), STALE_PID).expect("seed");
        }
        race_two_leases(root.path());
    }
}

fn race_two_leases(root: &Path) {
    let leases = [
        Arc::new(MaintenanceLease::new(root)),
        Arc::new(MaintenanceLease::new(root)),
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

fn open_rw(path: &Path) -> File {
    OpenOptions::new()
        .read(true)
        .write(true)
        .create(true)
        .truncate(false)
        .open(path)
        .expect("open")
}

/// A winner that has taken the lock but not yet written its pid holds the
/// pid gate; a loser arriving in that window waits for the write instead of
/// reporting no pid (#8733).
#[test]
fn a_loser_waits_for_the_winners_pid_write() {
    let root = tempfile::tempdir().expect("tempdir");
    // A bound far past the writer's delay, so only the gate decides the read.
    let loser = MaintenanceLease::new(root.path()).with_gate_wait(Duration::from_secs(30));
    // A winner mid-acquisition: gate and lease locked, pid not yet written.
    let gate = open_rw(&gate_path(loser.path()));
    gate.lock().expect("gate");
    let mut winner = open_rw(loser.path());
    winner.lock().expect("lease");
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_millis(20));
        winner.write_all(b"4242\n").expect("pid write");
        drop(gate);
        winner
    });

    let status = loser.try_hold();
    let _winner = writer.join().expect("writer");

    assert_eq!(
        status,
        LeaseStatus::HeldElsewhere {
            holder_pid: Some(4242)
        }
    );
}

/// A pid gate held by a stalled process delays a contender by the bound at
/// most; the election, which rests on the lease lock alone, still proceeds.
#[test]
fn a_wedged_pid_gate_does_not_block_the_election() {
    let root = tempfile::tempdir().expect("tempdir");
    let bound = Duration::from_millis(50);
    let lease = MaintenanceLease::new(root.path()).with_gate_wait(bound);
    let gate = open_rw(&gate_path(lease.path()));
    gate.lock().expect("gate");

    let started = Instant::now();
    let status = lease.try_hold();
    let waited = started.elapsed();

    assert!(
        status.is_held(),
        "a free lease is taken past a wedged gate: {status:?}"
    );
    assert!(
        waited >= bound,
        "the gate is polled for its bound: {waited:?}"
    );
    assert!(
        waited < Duration::from_secs(5),
        "the bound holds: {waited:?}"
    );
    assert_eq!(recorded_holder_pid(lease.path()), Some(std::process::id()));
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
