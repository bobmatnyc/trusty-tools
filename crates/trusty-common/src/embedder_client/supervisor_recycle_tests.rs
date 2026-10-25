//! Graceful-recycle tests for `EmbedderSupervisor` (#9828).
//!
//! Why: the embed sidecar's age was unbounded — the supervisor restarted it
//! only on a crash or a wedge, so a slow leak grew for 35 h to 68 GB. These
//! tests pin the three recycle triggers (footprint ceiling, max age, max
//! calls) and the footprint-read-failure arm against a real child process.
//! What: a POSIX-shell mock of `trusty-embedderd --stdio` (a request whose
//! text contains `slow` is answered after one second and `crawl` after two,
//! so a call can be in flight across a recycle; a `fail` file beside the
//! script makes a new mock exit at once and a `hang` file makes it never
//! answer), an injected footprint sampler, and an injected manual clock — the
//! max-age test never sleeps for the age it asserts.
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
here=$(dirname "$0")
[ -e "$here/fail" ] && exit 1
[ -e "$here/hang" ] && { echo $$ >> "$here/hang.pids"; exec sleep 600; }
while IFS= read -r line; do
  id=$(printf '%s' "$line" | sed -n 's/.*"id":\([0-9]*\).*/\1/p')
  [ -n "$id" ] || id=1
  case "$line" in *slow*) sleep 1 ;; *crawl*) sleep 2 ;; esac
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
        // #10207: the production defaults — twice the soft ceiling, 4x load.
        hard_footprint_mb: None,
        startup_load_scale_max: 4,
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
    const VARS: [&str; 7] = [
        "TRUSTY_EMBEDDERD_RECYCLE_FOOTPRINT_MB",
        "TRUSTY_EMBEDDERD_RECYCLE_HARD_FOOTPRINT_MB",
        "TRUSTY_EMBEDDERD_STARTUP_LOAD_SCALE_MAX",
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
    // #10207: the hard limit defaults to twice the soft ceiling.
    assert_eq!(def.hard_footprint_mb, None);
    assert_eq!(def.effective_hard_footprint_mb(), Some(8192));
    assert_eq!(def.startup_load_scale_max, 4);
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
    // With the soft ceiling off, an explicit hard limit stands alone.
    unsafe {
        std::env::set_var("TRUSTY_EMBEDDERD_RECYCLE_HARD_FOOTPRINT_MB", "500");
        std::env::set_var("TRUSTY_EMBEDDERD_STARTUP_LOAD_SCALE_MAX", "2");
    }
    let alone = RecycleConfig::from_env();
    assert_eq!(alone.effective_hard_footprint_mb(), Some(500));
    assert!(alone.enabled());
    assert_eq!(alone.startup_load_scale_max, 2);
    // #10207: a hard limit not above the soft ceiling is ignored.
    unsafe {
        std::env::set_var("TRUSTY_EMBEDDERD_RECYCLE_FOOTPRINT_MB", "1000");
        std::env::set_var("TRUSTY_EMBEDDERD_RECYCLE_HARD_FOOTPRINT_MB", "1000");
    }
    let low = RecycleConfig::from_env();
    assert_eq!(low.hard_footprint_mb, None);
    assert_eq!(low.effective_hard_footprint_mb(), Some(2000));
    unsafe { std::env::set_var("TRUSTY_EMBEDDERD_RECYCLE_HARD_FOOTPRINT_MB", "1500") };
    assert_eq!(
        RecycleConfig::from_env().effective_hard_footprint_mb(),
        Some(1500)
    );
    unsafe { std::env::set_var("TRUSTY_EMBEDDERD_RECYCLE_HARD_FOOTPRINT_MB", "0") };
    assert_eq!(
        RecycleConfig::from_env().effective_hard_footprint_mb(),
        None
    );

    unsafe {
        for (v, old) in VARS.iter().zip(saved) {
            match old {
                Some(val) => std::env::set_var(v, val),
                None => std::env::remove_var(v),
            }
        }
    }
}

/// Hooks with a fixed footprint for every pid and a manual clock.
fn hooks_reading(mb: u64, stats: &Arc<RecycleStats>) -> RecycleHooks {
    let (_now, clock) = manual_clock();
    RecycleHooks {
        sampler: Arc::new(move |_| Some(mb)),
        clock,
        stats: Arc::clone(stats),
    }
}

/// #9828 row 1: a hand-built zero check interval, recycling on or off, must
/// not panic the supervision task (which would take the sidecar with it).
#[tokio::test]
async fn zero_check_interval_does_not_panic_the_supervision_task() {
    for recycle_on in [true, false] {
        let (_dir, binary) = write_mock();
        let mut recycle = quiet_recycle();
        recycle.check_interval = Duration::ZERO;
        if recycle_on {
            recycle.footprint_ceiling_mb = 100;
        }
        let (supervisor, client_slot, pid_slot) =
            EmbedderSupervisor::spawn_stdio(binary, config(recycle))
                .await
                .expect("spawn_stdio");
        let pid = pid_slot.load(Ordering::Acquire);
        let stats = Arc::new(RecycleStats::new());
        let handle = supervisor
            .with_recycle_hooks(hooks_reading(50, &stats))
            .start_supervisor_task();
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert!(
            !handle.join.is_finished(),
            "supervision task ended (recycle_on={recycle_on}) on a zero check interval"
        );
        assert!(process_alive(pid), "sidecar died (recycle_on={recycle_on})");
        let client = client_slot.read().await.clone();
        assert!(client.embed_batch(vec!["zero".into()]).await.is_ok());
        assert_eq!(stats.snapshot().recycles_total, 0);
        handle.shutdown().await;
    }
}

/// #9828 row 2: with `drain_secs: 0`, calls that run longer than the drain
/// bound (two seconds each, the third queued on the in-flight semaphore) but
/// inside the per-call timeout all complete across a recycle.
#[tokio::test]
async fn recycle_drain_waits_for_calls_slower_than_drain_secs() {
    let (_dir, binary) = write_mock();
    let mut recycle = quiet_recycle();
    recycle.footprint_ceiling_mb = 100;
    recycle.drain_secs = 0;
    let (supervisor, client_slot, pid_slot) =
        EmbedderSupervisor::spawn_stdio(binary, config(recycle))
            .await
            .expect("spawn_stdio");
    let first_pid = pid_slot.load(Ordering::Acquire);
    let stats = Arc::new(RecycleStats::new());
    let fed = first_pid;
    let (_now, clock) = manual_clock();
    let supervisor = supervisor.with_recycle_hooks(RecycleHooks {
        sampler: Arc::new(move |pid| Some(if pid == fed { 101 } else { 50 })),
        clock,
        stats: Arc::clone(&stats),
    });
    let mut calls = Vec::new();
    for i in 0..3 {
        let client = client_slot.read().await.clone();
        calls.push(tokio::spawn(async move {
            client.embed_batch(vec![format!("crawl {i}")]).await
        }));
    }
    tokio::time::sleep(Duration::from_millis(50)).await;
    let handle = supervisor.start_supervisor_task();
    assert!(
        wait_until(5, || pid_slot.load(Ordering::Acquire) != first_pid).await,
        "no recycle"
    );
    for (i, call) in calls.into_iter().enumerate() {
        let answered = call.await.expect("join call");
        assert!(
            answered.is_ok(),
            "call {i} failed across the recycle: {answered:?}"
        );
    }
    assert!(
        wait_until(5, || !process_alive(first_pid)).await,
        "the recycled sidecar was never stopped after its calls finished"
    );
    assert!(!handle.has_given_up());
    handle.shutdown().await;
}

/// #9828 row 5(a): every fresh sidecar already reads over the ceiling. The
/// recycles back off instead of repeating every second check.
#[tokio::test]
async fn recycle_backs_off_when_a_fresh_sidecar_is_already_over_the_ceiling() {
    let (_dir, binary) = write_mock();
    let mut recycle = quiet_recycle();
    recycle.footprint_ceiling_mb = 100;
    let (supervisor, client_slot, _pid_slot) =
        EmbedderSupervisor::spawn_stdio(binary, config(recycle))
            .await
            .expect("spawn_stdio");
    let stats = Arc::new(RecycleStats::new());
    let handle = supervisor
        .with_recycle_hooks(hooks_reading(200, &stats))
        .start_supervisor_task();
    tokio::time::sleep(Duration::from_secs(3)).await;
    let snap = stats.snapshot();
    // Without a back-off: one recycle per ~120 ms, about 25 in 3 s. With it:
    // holds of 100, 200, 400, 800 ms... about 5.
    assert!(
        (2..=8).contains(&snap.recycles_total),
        "recycle storm: {snap:?}"
    );
    assert!(snap.backoff_level >= 2, "{snap:?}");
    assert_eq!(snap.backoff_cause.as_deref(), Some("premature_recycle"));
    assert!(snap.backoff_until_unix_secs.is_some(), "{snap:?}");
    assert!(!handle.has_given_up());
    let client = client_slot.read().await.clone();
    assert!(client.embed_batch(vec!["after".into()]).await.is_ok());
    handle.shutdown().await;
}

/// #9828 row 5(b): a replacement that cannot start is retried with growing
/// spacing, not on every tick; the old sidecar keeps serving.
#[tokio::test]
async fn recycle_spawn_failures_back_off() {
    let (dir, binary) = write_mock();
    let mut recycle = quiet_recycle();
    recycle.footprint_ceiling_mb = 100;
    let (supervisor, client_slot, pid_slot) =
        EmbedderSupervisor::spawn_stdio(binary, config(recycle))
            .await
            .expect("spawn_stdio");
    let first_pid = pid_slot.load(Ordering::Acquire);
    std::fs::write(dir.path().join("fail"), b"").expect("write fail marker");
    let stats = Arc::new(RecycleStats::new());
    let handle = supervisor
        .with_recycle_hooks(hooks_reading(200, &stats))
        .start_supervisor_task();

    let start = tokio::time::Instant::now();
    let mut seen = 0;
    let mut attempts = Vec::new();
    while start.elapsed() < Duration::from_secs(3) {
        let n = stats.snapshot().spawn_failures;
        if n != seen {
            seen = n;
            attempts.push(start.elapsed());
        }
        tokio::time::sleep(Duration::from_millis(5)).await;
    }
    let snap = stats.snapshot();
    // Without a back-off: one attempt per 50 ms tick, about 50 in 3 s.
    assert!(
        (3..=8).contains(&snap.spawn_failures),
        "spawn retried without back-off: {snap:?} at {attempts:?}"
    );
    let gaps: Vec<_> = attempts.windows(2).map(|w| w[1] - w[0]).collect();
    let (first, last) = (gaps[0], gaps[gaps.len() - 1]);
    assert!(last >= first * 2, "attempt spacing did not grow: {gaps:?}");
    assert_eq!(snap.recycles_total, 0, "{snap:?}");
    assert!(snap.backoff_level >= 3, "{snap:?}");
    assert_eq!(snap.backoff_cause.as_deref(), Some("spawn_failure"));
    assert_eq!(pid_slot.load(Ordering::Acquire), first_pid);
    let client = client_slot.read().await.clone();
    assert!(client.embed_batch(vec!["kept".into()]).await.is_ok());
    assert!(!handle.has_given_up());
    handle.shutdown().await;
}

/// #9828 row 5: while a replacement spawn hangs on its startup probe, the
/// old sidecar crashes; the crash path respawns it (after its 2 s back-off)
/// before the probe's own 5 s timeout would even have freed the loop.
#[tokio::test]
async fn old_sidecar_crash_during_a_blocked_respawn_is_handled() {
    let (dir, binary) = write_mock();
    let mut recycle = quiet_recycle();
    recycle.footprint_ceiling_mb = 100;
    let cfg = SupervisorConfig {
        max_restarts: 3,
        ..config(recycle)
    };
    let (supervisor, client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
        .await
        .expect("spawn_stdio");
    let first_pid = pid_slot.load(Ordering::Acquire);
    let hang = dir.path().join("hang");
    std::fs::write(&hang, b"").expect("write hang marker");
    let stats = Arc::new(RecycleStats::new());
    let handle = supervisor
        .with_recycle_hooks(hooks_reading(200, &stats))
        .start_supervisor_task();
    // The first judged check (~100 ms) starts a replacement that never answers.
    tokio::time::sleep(Duration::from_millis(400)).await;
    std::fs::remove_file(&hang).expect("remove hang marker");
    let killed = std::process::Command::new("kill")
        .args(["-9", &first_pid.to_string()])
        .status()
        .expect("kill");
    assert!(killed.success());
    assert!(
        wait_until(4, || {
            let pid = pid_slot.load(Ordering::Acquire);
            pid != 0 && pid != first_pid && process_alive(pid)
        })
        .await,
        "the old sidecar's crash went unhandled behind the blocked respawn"
    );
    let client = client_slot.read().await.clone();
    assert!(client.embed_batch(vec!["back".into()]).await.is_ok());
    assert!(!handle.has_given_up());
    handle.shutdown().await;
}

/// The back-off doubles per step from twice the base and stops at one hour.
#[test]
fn backoff_delay_doubles_and_caps() {
    let base = Duration::from_secs(30);
    assert_eq!(
        supervisor_recycle::backoff_delay(base, 1),
        Duration::from_secs(60)
    );
    assert_eq!(
        supervisor_recycle::backoff_delay(base, 2),
        Duration::from_secs(120)
    );
    assert_eq!(
        supervisor_recycle::backoff_delay(base, 40),
        Duration::from_secs(3600)
    );
}

/// #10207: how the configured hard limit resolves against the soft ceiling.
#[test]
fn hard_footprint_limit_resolution() {
    let with = |soft, hard| RecycleConfig {
        footprint_ceiling_mb: soft,
        hard_footprint_mb: hard,
        ..quiet_recycle()
    };
    assert_eq!(with(100, None).effective_hard_footprint_mb(), Some(200));
    assert_eq!(with(100, Some(0)).effective_hard_footprint_mb(), None);
    assert_eq!(
        with(100, Some(150)).effective_hard_footprint_mb(),
        Some(150)
    );
    // Not above the soft ceiling: ignored, the default applies.
    assert_eq!(
        with(100, Some(100)).effective_hard_footprint_mb(),
        Some(200)
    );
    assert_eq!(with(100, Some(60)).effective_hard_footprint_mb(), Some(200));
    assert_eq!(with(0, None).effective_hard_footprint_mb(), None);
    assert_eq!(with(0, Some(300)).effective_hard_footprint_mb(), Some(300));
    assert!(with(0, Some(300)).enabled());
    assert!(!with(0, None).enabled());
}

/// #10207: the startup probe scales with `1 + load1/ncpu` up to the cap, and
/// a respawn never gets less than the 120 s floor.
#[test]
fn startup_timeout_scales_with_load_and_floors_respawns() {
    use supervisor_hard_limit::{SpawnKind, scaled_startup_timeout};
    let base = Duration::from_secs(90);
    let secs = |kind, load, cap| scaled_startup_timeout(base, kind, load, cap).as_secs_f64();
    // Idle, unreadable, or nonsense load: no scaling.
    assert_eq!(secs(SpawnKind::First, Some(0.0), 4), 90.0);
    assert_eq!(secs(SpawnKind::First, None, 4), 90.0);
    assert_eq!(secs(SpawnKind::First, Some(f64::NAN), 4), 90.0);
    assert_eq!(secs(SpawnKind::First, Some(-1.0), 4), 90.0);
    // The failing host: load 8.8 on 16 CPUs.
    assert!((secs(SpawnKind::First, Some(0.55), 4) - 139.5).abs() < 1e-6);
    // Capped, and the cap can turn scaling off.
    assert_eq!(secs(SpawnKind::First, Some(9.0), 4), 360.0);
    assert_eq!(secs(SpawnKind::First, Some(9.0), 1), 90.0);
    assert_eq!(secs(SpawnKind::First, Some(9.0), 0), 90.0);
    // A respawn is floored at 120 s before scaling; a larger base wins.
    assert_eq!(secs(SpawnKind::Respawn, Some(0.0), 4), 120.0);
    assert_eq!(secs(SpawnKind::Respawn, Some(1.0), 4), 240.0);
    let long = scaled_startup_timeout(Duration::from_secs(180), SpawnKind::Respawn, None, 4);
    assert_eq!(long, Duration::from_secs(180));
}

/// #10207: a base too large to scale is returned unscaled, never a panic.
#[test]
fn startup_timeout_with_an_extreme_base_does_not_panic() {
    use supervisor_hard_limit::{SpawnKind, scaled_startup_timeout};
    for base in [Duration::MAX, Duration::from_secs(u64::MAX)] {
        for kind in [SpawnKind::First, SpawnKind::Respawn] {
            assert_eq!(scaled_startup_timeout(base, kind, Some(3.0), 4), base);
        }
    }
    // The first-spawn helper callers outside the crate use agrees.
    let cfg = SupervisorConfig {
        startup_timeout_secs: u64::MAX,
        ..SupervisorConfig::default()
    };
    assert_eq!(
        cfg.first_spawn_probe_timeout(Some(3.0)),
        Duration::from_secs(u64::MAX)
    );
    let cfg = SupervisorConfig {
        startup_timeout_secs: 90,
        ..SupervisorConfig::default()
    };
    assert_eq!(
        cfg.first_spawn_probe_timeout(Some(0.5)),
        Duration::from_secs(135)
    );
}

/// A `cat` child wrapped as a tracked client, so `RecycleWatch::due` can be
/// judged without a mock sidecar. The child is killed on drop.
fn cat_tracked() -> (tokio::process::Child, Arc<TrackedClient>) {
    let mut child = tokio::process::Command::new("cat")
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .kill_on_drop(true)
        .spawn()
        .expect("spawn cat");
    let stdin = child.stdin.take().expect("stdin");
    let stdout = child.stdout.take().expect("stdout");
    (
        child,
        TrackedClient::new(StdioEmbedderClient::new(stdin, stdout)),
    )
}

/// #10207 error arm: while a spawn-failure back-off hold runs, a footprint
/// over the soft ceiling is not judged, but one over the hard limit is.
#[tokio::test]
async fn hard_limit_fires_during_a_backoff_hold() {
    let (child, tracked) = cat_tracked();
    let pid = child.id().expect("cat pid");
    let mut cfg = quiet_recycle();
    cfg.footprint_ceiling_mb = 100;
    // A 30 s check interval makes the first hold 60 s.
    cfg.check_interval = Duration::from_secs(30);
    let footprint = Arc::new(AtomicU64::new(150));
    let read = Arc::clone(&footprint);
    let (_now, clock) = manual_clock();
    let hooks = RecycleHooks {
        sampler: Arc::new(move |_| Some(read.load(Ordering::Acquire))),
        clock,
        stats: Arc::new(RecycleStats::new()),
    };
    let mut watch = supervisor_recycle::RecycleWatch::new(&hooks);
    watch.after_spawn_failure(&cfg, &hooks);
    assert!(hooks.stats.snapshot().backoff_level >= 1);
    for _ in 0..3 {
        assert_eq!(watch.due(&cfg, &hooks, pid, &tracked), None, "held");
    }
    footprint.store(201, Ordering::Release);
    assert_eq!(
        watch.due(&cfg, &hooks, pid, &tracked),
        Some(RecycleTrigger::HardFootprint)
    );
    // Off, the same breach is held like any other trigger.
    cfg.hard_footprint_mb = Some(0);
    assert_eq!(watch.due(&cfg, &hooks, pid, &tracked), None);
    drop(child);
}

/// #10207 error arm: a footprint read that fails during a back-off hold is
/// counted, so a hard limit that cannot see the sidecar shows in `/health`.
#[tokio::test]
async fn failed_footprint_read_during_a_hold_is_counted() {
    let (child, tracked) = cat_tracked();
    let pid = child.id().expect("cat pid");
    let mut cfg = quiet_recycle();
    cfg.footprint_ceiling_mb = 100;
    cfg.check_interval = Duration::from_secs(30);
    let (_now, clock) = manual_clock();
    let hooks = RecycleHooks {
        sampler: Arc::new(|_| None),
        clock,
        stats: Arc::new(RecycleStats::new()),
    };
    let mut watch = supervisor_recycle::RecycleWatch::new(&hooks);
    watch.after_spawn_failure(&cfg, &hooks);
    for _ in 0..3 {
        assert_eq!(watch.due(&cfg, &hooks, pid, &tracked), None, "held");
    }
    assert_eq!(hooks.stats.snapshot().footprint_read_errors, 3);
    drop(child);
}

/// #10207 regression: the replacement can never start, so every footprint
/// recycle keeps the old sidecar; its footprint then crosses the hard limit
/// (twice the 100 MB ceiling). The old sidecar is killed, no live sidecar
/// stays over the limit, and one serves again once spawning works.
#[tokio::test]
async fn hard_limit_kills_a_sidecar_whose_replacement_cannot_start() {
    let (dir, binary) = write_mock();
    let mut recycle = quiet_recycle();
    recycle.footprint_ceiling_mb = 100;
    let cfg = SupervisorConfig {
        max_restarts: 5,
        ..config(recycle)
    };
    let (supervisor, client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
        .await
        .expect("spawn_stdio");
    let first_pid = pid_slot.load(Ordering::Acquire);
    let fail = dir.path().join("fail");
    std::fs::write(&fail, b"").expect("write fail marker");
    let stats = Arc::new(RecycleStats::new());
    let old_mb = Arc::new(AtomicU64::new(150));
    let read = Arc::clone(&old_mb);
    let (_now, clock) = manual_clock();
    let handle = supervisor
        .with_recycle_hooks(RecycleHooks {
            sampler: Arc::new(move |pid| {
                Some(if pid == first_pid {
                    read.load(Ordering::Acquire)
                } else {
                    50
                })
            }),
            clock,
            stats: Arc::clone(&stats),
        })
        .start_supervisor_task();

    assert!(
        wait_until(5, || stats.snapshot().spawn_failures >= 2).await,
        "replacement spawns never failed: {:?}",
        stats.snapshot()
    );
    assert!(
        process_alive(first_pid),
        "old sidecar died before the breach"
    );
    old_mb.store(250, Ordering::Release);
    assert!(
        wait_until(3, || !process_alive(first_pid)).await,
        "a sidecar over the hard limit was kept: {:?}",
        stats.snapshot()
    );
    std::fs::remove_file(&fail).expect("remove fail marker");
    assert!(
        wait_until(10, || {
            let pid = pid_slot.load(Ordering::Acquire);
            pid != 0 && pid != first_pid && process_alive(pid)
        })
        .await,
        "no sidecar came back once spawning worked"
    );
    let client = client_slot.read().await.clone();
    assert!(client.embed_batch(vec!["back".into()]).await.is_ok());
    let snap = stats.snapshot();
    assert_eq!(snap.hard_footprint_kills, 1, "{snap:?}");
    assert_eq!(snap.recycles_total, 0, "{snap:?}");
    assert!(!handle.has_given_up());
    handle.shutdown().await;
}

/// #10207: the old sidecar crosses the hard limit while its replacement
/// hangs on the startup probe (floored at 120 s). The replacement is
/// abandoned and the old sidecar killed within a few check intervals.
#[tokio::test]
async fn hard_limit_abandons_a_hanging_replacement_spawn() {
    let (dir, binary) = write_mock();
    let mut recycle = quiet_recycle();
    recycle.footprint_ceiling_mb = 100;
    let cfg = SupervisorConfig {
        max_restarts: 5,
        ..config(recycle)
    };
    let (supervisor, client_slot, pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
        .await
        .expect("spawn_stdio");
    let first_pid = pid_slot.load(Ordering::Acquire);
    let hang = dir.path().join("hang");
    std::fs::write(&hang, b"").expect("write hang marker");
    let stats = Arc::new(RecycleStats::new());
    let old_mb = Arc::new(AtomicU64::new(150));
    let read = Arc::clone(&old_mb);
    let (_now, clock) = manual_clock();
    let handle = supervisor
        .with_recycle_hooks(RecycleHooks {
            sampler: Arc::new(move |pid| {
                Some(if pid == first_pid {
                    read.load(Ordering::Acquire)
                } else {
                    50
                })
            }),
            clock,
            stats: Arc::clone(&stats),
        })
        .start_supervisor_task();
    // The first judged check (~100 ms) starts a replacement that never answers.
    tokio::time::sleep(Duration::from_millis(400)).await;
    std::fs::remove_file(&hang).expect("remove hang marker");
    old_mb.store(250, Ordering::Release);
    assert!(
        wait_until(3, || !process_alive(first_pid)).await,
        "the hard limit waited behind a hanging replacement: {:?}",
        stats.snapshot()
    );
    // The abandoned replacement is gone too, not left running.
    let pids = std::fs::read_to_string(dir.path().join("hang.pids")).expect("replacement pid");
    let pids: Vec<u32> = pids.lines().map(|l| l.parse().expect("pid")).collect();
    assert!(!pids.is_empty());
    assert!(
        wait_until(3, || pids.iter().all(|&p| !process_alive(p))).await,
        "an abandoned replacement is still running: {pids:?}"
    );
    assert!(
        wait_until(5, || {
            let pid = pid_slot.load(Ordering::Acquire);
            pid != 0 && pid != first_pid && process_alive(pid)
        })
        .await,
        "no sidecar came back after the hard kill"
    );
    let client = client_slot.read().await.clone();
    assert!(client.embed_batch(vec!["back".into()]).await.is_ok());
    assert_eq!(stats.snapshot().hard_footprint_kills, 1);
    assert!(!handle.has_given_up());
    handle.shutdown().await;
}

/// #10207 error arm: while a replacement spawn hangs, failed footprint reads
/// of the old sidecar are counted rather than read as "under the limit".
#[tokio::test]
async fn failed_footprint_read_during_a_hanging_replacement_is_counted() {
    let (dir, binary) = write_mock();
    let mut recycle = quiet_recycle();
    recycle.footprint_ceiling_mb = 100;
    let (supervisor, _client_slot, _pid_slot) =
        EmbedderSupervisor::spawn_stdio(binary, config(recycle))
            .await
            .expect("spawn_stdio");
    let hang = dir.path().join("hang");
    std::fs::write(&hang, b"").expect("write hang marker");
    let stats = Arc::new(RecycleStats::new());
    // 0 = the read fails.
    let footprint = Arc::new(AtomicU64::new(150));
    let read = Arc::clone(&footprint);
    let (_now, clock) = manual_clock();
    let handle = supervisor
        .with_recycle_hooks(RecycleHooks {
            sampler: Arc::new(move |_| Some(read.load(Ordering::Acquire)).filter(|&mb| mb > 0)),
            clock,
            stats: Arc::clone(&stats),
        })
        .start_supervisor_task();
    // The first judged check (~100 ms) starts a replacement that never answers.
    tokio::time::sleep(Duration::from_millis(400)).await;
    footprint.store(0, Ordering::Release);
    assert!(
        wait_until(3, || stats.snapshot().footprint_read_errors >= 3).await,
        "failed reads behind a hanging replacement were not counted: {:?}",
        stats.snapshot()
    );
    assert_eq!(stats.snapshot().spawn_failures, 0, "the spawn still hangs");
    // Free the loop: a hard breach abandons the hanging replacement.
    std::fs::remove_file(&hang).expect("remove hang marker");
    footprint.store(250, Ordering::Release);
    assert!(
        wait_until(3, || stats.snapshot().hard_footprint_kills == 1).await,
        "{:?}",
        stats.snapshot()
    );
    handle.shutdown().await;
}

/// #10207 ruling D: a sidecar that crosses the hard limit after every
/// respawn trips `max_restarts` instead of looping.
#[tokio::test]
async fn repeated_hard_limit_breaches_give_up() {
    let (_dir, binary) = write_mock();
    let mut recycle = quiet_recycle();
    recycle.footprint_ceiling_mb = 100;
    let cfg = SupervisorConfig {
        max_restarts: 2,
        ..config(recycle)
    };
    let (supervisor, _client_slot, _pid_slot) = EmbedderSupervisor::spawn_stdio(binary, cfg)
        .await
        .expect("spawn_stdio");
    let stats = Arc::new(RecycleStats::new());
    let handle = supervisor
        .with_recycle_hooks(hooks_reading(250, &stats))
        .start_supervisor_task();
    assert!(
        wait_until(10, || handle.has_given_up()).await,
        "repeated hard-limit kills never gave up: {:?}",
        stats.snapshot()
    );
    assert_eq!(stats.snapshot().hard_footprint_kills, 3);
    handle.shutdown().await;
}
