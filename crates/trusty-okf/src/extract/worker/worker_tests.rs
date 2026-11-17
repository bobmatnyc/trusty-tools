//! Worker-client tests that need a crate-private hook or the process layer
//! (#10746 S10): the forced reap timeout (criterion 124) and a host write
//! that blocks because the worker never reads stdin (criterion 122).

use std::sync::atomic::AtomicBool;
use std::time::{Duration, Instant};

use super::process::{Hooks, IoFail, WorkerProcess};
use super::*;
use crate::extract::{Extractor, TypeHint};

#[path = "../../../tests/worker_support/fake.rs"]
mod fake;

use fake::{Fake, group_gone, within};

fn quick() -> Limits {
    Limits {
        item: Duration::from_secs(2),
        handshake: Duration::from_secs(2),
        shutdown: Duration::from_secs(1),
        reap: Duration::from_secs(1),
        ..Limits::default()
    }
}

fn config(dir: &std::path::Path, fake: &Fake) -> WorkerConfig {
    let mut cfg = WorkerConfig::new(fake.write(dir, "worker"), dir);
    cfg.sandbox = SandboxPolicy::Off;
    cfg.limits = quick();
    cfg
}

/// Why: criterion 124 — a child that cannot be reaped in time must not
/// hold the import; a reaper thread takes it and the run records it.
/// What: the reap step is forced past its deadline by the test hook; an
/// item times out; `extract` returns within the item deadline, the 2 s
/// TERM grace and the reap deadline; `end_run` reports `ReapTimeout`; the
/// next item is refused without a new spawn; the group is still gone.
/// Test: self-contained.
#[test]
fn c124_forced_reap_timeout_returns_within_the_deadline() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = config(dir.path(), &Fake::default());
    cfg.hooks = Hooks { reap_timeout: true };
    let budget = cfg.limits.item + Duration::from_secs(2) + cfg.limits.reap;
    let (code, took, run, groups, again) = within(budget + Duration::from_secs(5), move || {
        let ex = WorkerExtractor::new(cfg).unwrap();
        let local = Locator::DocStore {
            path: "/nonexistent".into(),
            extensions: Vec::new(),
            recursive: true,
        };
        ex.begin_run(&SourceSpec::new(
            "local",
            None,
            local,
            "2026-11-04T00:00:00Z",
        ));
        let hint = TypeHint::for_path("a.pdf");
        let t = Instant::now();
        let code = ex.extract(b"HANG", &hint).unwrap_err().code;
        let took = t.elapsed();
        let again = ex.extract(b"fine", &hint).unwrap_err().code;
        let groups = ex.spawned_groups();
        (code, took, ex.end_run(), groups, again)
    });
    assert_eq!(code, "timeout");
    assert!(took <= budget + Duration::from_millis(500), "{took:?}");
    assert_eq!(run.errors, vec![WorkerError::ReapTimeout]);
    assert_eq!(again, "reap_timeout");
    assert_eq!(groups.len(), 1, "no second worker after a reap timeout");
    assert!(group_gone(groups[0]));
}

/// Why: criterion 122 — the host's write to a worker that never reads
/// stdin must not hang the host.
/// What: a 1 MiB write to a `no_read` double fails `Timeout` at its
/// deadline; the kill sequence then empties the process group.
/// Test: self-contained.
#[test]
fn blocked_host_write_times_out_and_the_worker_is_killed() {
    let dir = tempfile::tempdir().unwrap();
    let fake = Fake {
        mode: "no_read",
        ..Fake::default()
    };
    let path = fake.write(dir.path(), "worker");
    let scratch = dir.path().to_path_buf();
    let (fail, pgid, errs) = within(Duration::from_secs(1 + 2 + 1 + 5), move || {
        let lim = quick();
        let mut p =
            WorkerProcess::spawn(&path, &scratch, SandboxPolicy::Off, lim, Hooks::default())
                .unwrap();
        let never = AtomicBool::new(false);
        let fail = p.send(
            vec![0u8; 1 << 20],
            Instant::now() + Duration::from_secs(1),
            &never,
        );
        let pgid = p.pgid();
        (fail, pgid, p.kill())
    });
    assert_eq!(fail, Err(IoFail::Timeout));
    assert!(errs.is_empty(), "{errs:?}");
    assert!(group_gone(pgid));
}
