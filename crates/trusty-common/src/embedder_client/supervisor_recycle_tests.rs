//! Graceful-recycle tests for `EmbedderSupervisor` (#9828).
//!
//! Why: the embed sidecar's age was unbounded — the supervisor restarted it
//! only on a crash or a wedge, so a slow leak grew for 35 h to 68 GB. These
//! tests pin the three recycle triggers (footprint ceiling, max age, max
//! calls) and the footprint-read-failure arm against a real child process.
//! What: a POSIX-shell mock of `trusty-embedderd --stdio` (a request whose
//! text contains `slow` is answered after one second, so a call can be in
//! flight across a recycle), an injected footprint sampler, and an injected
//! manual clock — the max-age test never sleeps for the age it asserts.
//! `max_restarts: 0` makes "no failure counted" observable: one counted
//! failure would trip `should_give_up` and flip `has_given_up`.
//! Test: this module.

use super::*;
use std::os::unix::fs::PermissionsExt;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::time::Duration;

/// Write the mock sidecar; returns the guarding `TempDir` and the script path.
fn write_mock() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("create tempdir");
    let path = dir.path().join("mock-embedderd-recycle.sh");
    std::fs::write(
        &path,
        r#"#!/bin/sh
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] || id=1
  case "$line" in *slow*) sleep 1 ;; esac
  printf '{"jsonrpc":"2.0","result":{"embeddings":[[0.1]]},"id":%s}\n' "$id"
done
"#,
    )
    .expect("write mock script");
    let mut perms = std::fs::metadata(&path).expect("stat").permissions();
    perms.set_mode(0o755);
    std::fs::set_permissions(&path, perms).expect("chmod +x");
    (dir, path)
}

fn process_alive(pid: u32) -> bool {
    std::process::Command::new("kill")
        .args(["-0", &pid.to_string()])
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .status()
        .map(|s| s.success())
        .unwrap_or(false)
}

/// A recycle config with every trigger off and a 50 ms check interval; each
/// test turns on the one trigger it exercises.
fn quiet_recycle() -> RecycleConfig {
    RecycleConfig {
        footprint_ceiling_mb: 0,
        max_age_secs: 0,
        max_calls: 0,
        check_interval: Duration::from_millis(50),
        drain_secs: 10,
    }
}

fn config(recycle: RecycleConfig) -> SupervisorConfig {
    SupervisorConfig {
        startup_timeout_secs: 5,
        // #9828: zero tolerance — a recycle counted as a crash gives up.
        max_restarts: 0,
        recycle,
        ..SupervisorConfig::default()
    }
}

/// A manual clock: the test moves time; the supervisor reads it.
fn manual_clock() -> (Arc<AtomicU64>, RecycleClock) {
    let now_ms = Arc::new(AtomicU64::new(0));
    let read = Arc::clone(&now_ms);
    let clock: RecycleClock = Arc::new(move || Duration::from_millis(read.load(Ordering::Acquire)));
    (now_ms, clock)
}

/// Poll `cond` every 20 ms for up to `secs`; true once it holds.
async fn wait_until(secs: u64, mut cond: impl FnMut() -> bool) -> bool {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(secs);
    while tokio::time::Instant::now() < deadline {
        if cond() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    cond()
}

/// Ceiling 100 MB, the first sidecar reads 101 MB: exactly one recycle to a
/// new pid, the call in flight across it is answered, no failure counted.
#[tokio::test]
async fn recycle_on_footprint_ceiling_keeps_inflight_call_and_counts_no_failure() {
    let (_dir, binary) = write_mock();
    let recycle = RecycleConfig {
        footprint_ceiling_mb: 100,
        ..quiet_recycle()
    };
    let (supervisor, client_slot, pid_slot) =
        EmbedderSupervisor::spawn_stdio(binary, config(recycle))
            .await
            .expect("spawn_stdio");
    let first_pid = pid_slot.load(Ordering::Acquire);
    assert!(first_pid > 0);

    let stats = Arc::new(RecycleStats::new());
    let fed = Arc::new(AtomicU32::new(first_pid));
    let sampler: FootprintSampler = Arc::new(move |pid| {
        Some(if pid == fed.load(Ordering::Acquire) {
            101
        } else {
            50
        })
    });
    let (_now, clock) = manual_clock();
    let supervisor = supervisor.with_recycle_hooks(RecycleHooks {
        sampler,
        clock,
        stats: Arc::clone(&stats),
    });

    // Start a call that stays in flight for ~1 s on the first sidecar.
    let client = client_slot.read().await.clone();
    let inflight = tokio::spawn(async move { client.embed_batch(vec!["slow".into()]).await });
    tokio::time::sleep(Duration::from_millis(100)).await;

    let handle = supervisor.start_supervisor_task();
    assert!(
        wait_until(5, || pid_slot.load(Ordering::Acquire) != first_pid).await,
        "the supervisor never recycled a sidecar reading 101 MB against a 100 MB ceiling"
    );
    let answered = inflight.await.expect("join in-flight call");
    assert!(
        answered.is_ok(),
        "in-flight call failed across the recycle: {answered:?}"
    );
    assert!(
        wait_until(5, || !process_alive(first_pid)).await,
        "the recycled sidecar {first_pid} was never stopped after draining"
    );

    // Several more check intervals: the new sidecar reads 50 MB, so no second
    // recycle may happen.
    tokio::time::sleep(Duration::from_millis(400)).await;
    let snap = stats.snapshot();
    assert_eq!(snap.recycles_total, 1, "exactly one recycle: {snap:?}");
    assert_eq!(snap.footprint, 1, "{snap:?}");
    assert_eq!(snap.last_trigger.as_deref(), Some("footprint"));
    assert_eq!(snap.last_old_pid, Some(first_pid));
    assert_eq!(snap.last_new_pid, Some(pid_slot.load(Ordering::Acquire)));
    assert!(
        !handle.has_given_up(),
        "a recycle was counted as a crash (max_restarts=0 gave up)"
    );

    // The new sidecar serves.
    let client = client_slot.read().await.clone();
    assert!(client.embed_batch(vec!["after".into()]).await.is_ok());
    handle.shutdown().await;
}

/// Max age 3600 s on an injected clock: advancing it past the age recycles
/// exactly once, with no real wait for the age itself.
#[tokio::test]
async fn recycle_on_max_age_uses_the_injected_clock() {
    let (_dir, binary) = write_mock();
    let recycle = RecycleConfig {
        max_age_secs: 3600,
        ..quiet_recycle()
    };
    let (supervisor, client_slot, pid_slot) =
        EmbedderSupervisor::spawn_stdio(binary, config(recycle))
            .await
            .expect("spawn_stdio");
    let first_pid = pid_slot.load(Ordering::Acquire);
    let stats = Arc::new(RecycleStats::new());
    let (now_ms, clock) = manual_clock();
    let supervisor = supervisor.with_recycle_hooks(RecycleHooks {
        sampler: Arc::new(|_| Some(1)),
        clock,
        stats: Arc::clone(&stats),
    });
    let handle = supervisor.start_supervisor_task();

    // Below the age: nothing happens.
    now_ms.store(3_599_000, Ordering::Release);
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(pid_slot.load(Ordering::Acquire), first_pid);
    assert_eq!(stats.snapshot().recycles_total, 0);

    now_ms.store(3_601_000, Ordering::Release);
    assert!(
        wait_until(5, || pid_slot.load(Ordering::Acquire) != first_pid).await,
        "max-age never recycled the sidecar"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    let snap = stats.snapshot();
    assert_eq!(snap.recycles_total, 1, "{snap:?}");
    assert_eq!(snap.max_age, 1, "{snap:?}");
    assert_eq!(snap.last_trigger.as_deref(), Some("max_age"));
    assert!(!handle.has_given_up());
    let client = client_slot.read().await.clone();
    assert!(client.embed_batch(vec!["after".into()]).await.is_ok());
    handle.shutdown().await;
}

/// Max calls 3: the third embed call recycles the sidecar once; the startup
/// probe is not counted and the new sidecar starts from zero.
#[tokio::test]
async fn recycle_on_max_calls_counts_embed_calls_only() {
    let (_dir, binary) = write_mock();
    let recycle = RecycleConfig {
        max_calls: 3,
        ..quiet_recycle()
    };
    let (supervisor, client_slot, pid_slot) =
        EmbedderSupervisor::spawn_stdio(binary, config(recycle))
            .await
            .expect("spawn_stdio");
    let first_pid = pid_slot.load(Ordering::Acquire);
    let stats = Arc::new(RecycleStats::new());
    let (_now, clock) = manual_clock();
    let supervisor = supervisor.with_recycle_hooks(RecycleHooks {
        sampler: Arc::new(|_| Some(1)),
        clock,
        stats: Arc::clone(&stats),
    });
    let handle = supervisor.start_supervisor_task();

    for i in 0..2 {
        let client = client_slot.read().await.clone();
        assert!(client.embed_batch(vec![format!("c{i}")]).await.is_ok());
    }
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert_eq!(
        pid_slot.load(Ordering::Acquire),
        first_pid,
        "two calls (plus the startup probe) must not reach max_calls=3"
    );

    let client = client_slot.read().await.clone();
    assert!(client.embed_batch(vec!["c2".into()]).await.is_ok());
    assert!(
        wait_until(5, || pid_slot.load(Ordering::Acquire) != first_pid).await,
        "max-calls never recycled the sidecar"
    );
    tokio::time::sleep(Duration::from_millis(400)).await;
    let snap = stats.snapshot();
    assert_eq!(snap.recycles_total, 1, "{snap:?}");
    assert_eq!(snap.max_calls, 1, "{snap:?}");
    assert!(!handle.has_given_up());
    handle.shutdown().await;
}

/// Fail-open check: a footprint sampler that always fails is neither read as
/// "under the ceiling" forever nor a recycle storm — three failed reads
/// recycle once (`footprint_unreadable`), then the trigger stays latched off
/// while reads keep failing.
#[tokio::test]
async fn footprint_read_failure_recycles_once_then_latches() {
    let (_dir, binary) = write_mock();
    let recycle = RecycleConfig {
        footprint_ceiling_mb: 100,
        ..quiet_recycle()
    };
    let (supervisor, _client_slot, pid_slot) =
        EmbedderSupervisor::spawn_stdio(binary, config(recycle))
            .await
            .expect("spawn_stdio");
    let first_pid = pid_slot.load(Ordering::Acquire);
    let stats = Arc::new(RecycleStats::new());
    let (_now, clock) = manual_clock();
    let supervisor = supervisor.with_recycle_hooks(RecycleHooks {
        sampler: Arc::new(|_| None),
        clock,
        stats: Arc::clone(&stats),
    });
    let handle = supervisor.start_supervisor_task();

    assert!(
        wait_until(5, || pid_slot.load(Ordering::Acquire) != first_pid).await,
        "an unreadable footprint was treated as under the ceiling"
    );
    // ~30 more check intervals with reads still failing.
    tokio::time::sleep(Duration::from_millis(1500)).await;
    let snap = stats.snapshot();
    assert_eq!(snap.recycles_total, 1, "recycle storm: {snap:?}");
    assert_eq!(snap.footprint_unreadable, 1, "{snap:?}");
    assert!(snap.footprint_read_errors >= 6, "{snap:?}");
    assert!(!handle.has_given_up());
    handle.shutdown().await;
}

/// The env knobs and their defaults.
#[test]
#[serial_test::serial]
fn recycle_config_from_env_defaults_and_overrides() {
    const VARS: [&str; 5] = [
        "TRUSTY_EMBEDDERD_RECYCLE_FOOTPRINT_MB",
        "TRUSTY_EMBEDDERD_RECYCLE_MAX_AGE_SECS",
        "TRUSTY_EMBEDDERD_RECYCLE_MAX_CALLS",
        "TRUSTY_EMBEDDERD_RECYCLE_CHECK_SECS",
        "TRUSTY_EMBEDDERD_RECYCLE_DRAIN_SECS",
    ];
    // The shared env lock (#7253): `serial` alone excludes nothing outside it.
    let _env = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(|e| e.into_inner());
    let saved: Vec<_> = VARS.iter().map(|v| std::env::var(v).ok()).collect();
    // SAFETY: test-only; `ENV_LOCK` is held for the whole mutation window.
    unsafe {
        for v in VARS {
            std::env::remove_var(v);
        }
    }
    let def = RecycleConfig::from_env();
    assert_eq!(def.footprint_ceiling_mb, 4096);
    assert_eq!(def.max_age_secs, 21_600);
    assert_eq!(def.max_calls, 100_000);
    assert_eq!(def.check_interval, Duration::from_secs(30));
    assert_eq!(def.drain_secs, 60);
    assert_eq!(SupervisorConfig::from_env().recycle.max_age_secs, 21_600);

    unsafe {
        std::env::set_var("TRUSTY_EMBEDDERD_RECYCLE_FOOTPRINT_MB", "0");
        std::env::set_var("TRUSTY_EMBEDDERD_RECYCLE_MAX_CALLS", "7");
        std::env::set_var("TRUSTY_EMBEDDERD_RECYCLE_CHECK_SECS", "0");
    }
    let over = RecycleConfig::from_env();
    assert_eq!(over.footprint_ceiling_mb, 0);
    assert_eq!(over.max_calls, 7);
    // A zero check interval would spin; it is clamped to one second.
    assert_eq!(over.check_interval, Duration::from_secs(1));

    unsafe {
        for (v, old) in VARS.iter().zip(saved) {
            match old {
                Some(val) => std::env::set_var(v, val),
                None => std::env::remove_var(v),
            }
        }
    }
}
