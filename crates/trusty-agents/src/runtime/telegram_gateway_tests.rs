//! Tests for the API host's Telegram gateway supervisor (#8190).
//!
//! Why: every condition #8190 states is decided by a function in
//! `telegram_gateway.rs` that can be driven without a bot token, a network, or
//! a real `getUpdates` — the start decision, the restart backoff, the
//! stand-down on a taken lock, and the shutdown that drops the poll future and
//! with it the PID guard. A live bot is out of scope here, exactly as it is for
//! `crate::telegram`.
//! What: the supervisor's poll attempt and lock probe are injected, so an
//! error arm is a closure that returns `Err` rather than a revoked token.
//! Every test that holds a shutdown sender keeps it alive deliberately — a
//! dropped sender resolves the receiver and reads as a requested shutdown.

use super::*;
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};
use tokio::sync::mpsc;

/// An RAII stand-in for the PID guard `run_telegram_bot` holds.
///
/// Why: condition 5 is "shutdown releases the lock", and the lock is released
/// by the poll future being DROPPED. A drop flag is how a test observes that.
struct DropFlag(Arc<AtomicUsize>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn binding(json: serde_json::Value) -> crate::api::server::agent_channels::Binding {
    serde_json::from_value(json).expect("test binding must parse")
}

fn telegram_binding(enabled: bool, receive_enabled: bool) -> serde_json::Value {
    serde_json::json!({
        "id": "tg-1",
        "name": "Masa DM",
        "provider": "telegram",
        "target": "123456",
        "enabled": enabled,
        "send_enabled": true,
        "receive_enabled": receive_enabled,
    })
}

// ---------------------------------------------------------------- decision

/// Condition 1: a token plus an enabled receiving binding starts the gateway.
#[test]
fn telegram_gateway_starts_with_token_and_enabled_binding() {
    assert_eq!(decide(true, None, None), GatewayDecision::Start);
}

/// Condition 2: no enabled Telegram channel means no poller, with one reason.
#[test]
fn telegram_gateway_skips_without_an_enabled_binding() {
    let GatewayDecision::Skip(reason) = decide(false, None, None) else {
        panic!("a host with no Telegram channel must not poll");
    };
    assert!(
        reason.contains("no enabled Telegram channel"),
        "the skip must say why: {reason}"
    );
}

/// Condition 2: an unresolvable token means no poller, with one reason.
#[test]
fn telegram_gateway_skips_without_a_resolvable_token() {
    let GatewayDecision::Skip(reason) = decide(true, Some("no such credential".into()), None)
    else {
        panic!("a host whose token will not resolve must not poll into a 401 loop");
    };
    assert!(
        reason.contains("no such credential"),
        "the skip must carry the resolution failure: {reason}"
    );
}

/// Condition 3: a live lock holder means this host refuses to start a second
/// poller, and says whose PID holds it.
#[test]
fn telegram_gateway_refuses_when_another_poller_holds_the_lock() {
    let GatewayDecision::Skip(reason) = decide(true, None, Some(4242)) else {
        panic!("a second concurrent getUpdates is a live outage; it must be refused");
    };
    assert!(
        reason.contains("4242") && reason.contains("refusing to start a second poller"),
        "the refusal must name the holder: {reason}"
    );
}

// ------------------------------------------------------------------- scan

#[test]
fn telegram_gateway_counts_an_enabled_receiving_binding() {
    assert!(binding_receives_telegram(&binding(telegram_binding(
        true, true
    ))));
}

#[test]
fn telegram_gateway_ignores_a_disabled_or_send_only_binding() {
    assert!(!binding_receives_telegram(&binding(telegram_binding(
        false, true
    ))));
    assert!(!binding_receives_telegram(&binding(telegram_binding(
        true, false
    ))));
    let slack = binding(serde_json::json!({
        "id": "sl-1", "name": "DM", "provider": "slack", "target": "D123",
        "enabled": true, "send_enabled": true, "receive_enabled": true,
    }));
    assert!(!binding_receives_telegram(&slack));
}

#[test]
fn telegram_gateway_counts_a_routed_global_channel() {
    let routed = crate::channels::Channel {
        provider: "telegram".into(),
        enabled: true,
        receive_enabled: true,
        route_to: vec!["izzie".into()],
        ..Default::default()
    };
    assert!(global_channel_receives_telegram(&routed));
    let unrouted = crate::channels::Channel {
        route_to: vec![],
        ..routed.clone()
    };
    assert!(
        !global_channel_receives_telegram(&unrouted),
        "a global channel that wakes nobody is not a reason to poll"
    );
}

/// Fail-open check: a roster this host cannot read warns and scans nothing —
/// it never silently decides that no assistant wants Telegram.
#[test]
fn telegram_gateway_roster_failure_warns_and_scans_nothing() {
    assert!(roster_or_warn(Err(anyhow::anyhow!("HOME is not set"))).is_empty());
    assert_eq!(
        roster_or_warn(Ok(vec!["izzie".to_string()])),
        vec!["izzie".to_string()]
    );
}

/// Fail-open check: one unreadable channel file is skipped with a warning, and
/// never decides the gateway for the other assistants.
#[test]
fn telegram_gateway_an_unreadable_channel_file_warns_and_is_skipped() {
    let broken: Result<(String, String, Vec<u8>), &str> = Err("channels.json is not JSON");
    assert!(bindings_or_warn("izzie", broken).is_empty());
    let ok: Result<(String, String, Vec<u8>), &str> = Ok(("path".into(), "rev".into(), vec![1, 2]));
    assert_eq!(bindings_or_warn("izzie", ok), vec![1, 2]);
}

// ---------------------------------------------------------------- backoff

#[test]
fn telegram_gateway_backoff_doubles_then_caps() {
    assert_eq!(backoff_for(0), FIRST_BACKOFF);
    assert_eq!(backoff_for(1), FIRST_BACKOFF);
    assert_eq!(backoff_for(2), Duration::from_secs(10));
    assert_eq!(backoff_for(3), Duration::from_secs(20));
    assert_eq!(backoff_for(20), MAX_BACKOFF);
}

// ------------------------------------------------------------- supervisor

/// Condition 4: a failing poller is logged and restarted with a growing
/// backoff, and the failure never ends the supervisor.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_retries_a_failed_poller_with_backoff() {
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        supervise(
            move || {
                let started = started_tx.clone();
                async move {
                    let _ = started.send(tokio::time::Instant::now());
                    Err(anyhow::anyhow!("getUpdates failed: 401 Unauthorized"))
                }
            },
            || None,
            rx,
        )
        .await
    });
    let first = started_rx.recv().await.expect("first attempt");
    let second = started_rx.recv().await.expect("restart after the failure");
    let third = started_rx.recv().await.expect("second restart");
    assert_eq!(second - first, Duration::from_secs(5));
    assert_eq!(third - second, Duration::from_secs(10));
    tx.send(()).expect("supervisor is still running");
    assert_eq!(
        task.await.expect("no panic"),
        GatewayExit::ShutdownRequested
    );
}

/// Condition 4: a poller that RETURNS while the host still serves has stopped
/// receiving, which must never be silent — it is restarted too.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_restarts_a_poller_that_returned_ok() {
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (tx, rx) = oneshot::channel();
    let task = tokio::spawn(async move {
        supervise(
            move || {
                let started = started_tx.clone();
                async move {
                    let _ = started.send(());
                    Ok(())
                }
            },
            || None,
            rx,
        )
        .await
    });
    started_rx.recv().await.expect("first attempt");
    started_rx
        .recv()
        .await
        .expect("the poller is restarted, not left stopped");
    tx.send(()).expect("supervisor is still running");
    assert_eq!(
        task.await.expect("no panic"),
        GatewayExit::ShutdownRequested
    );
}

/// Condition 3, at runtime: a standalone `--telegram` that took the lock while
/// we were backing off keeps it — this host stands down instead of racing it.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_stops_when_the_lock_is_taken_during_backoff() {
    let probes = Arc::new(AtomicUsize::new(0));
    let probes_for_lock = Arc::clone(&probes);
    let (tx, rx) = oneshot::channel();
    let exit = supervise(
        || async { Err(anyhow::anyhow!("poller died")) },
        move || {
            if probes_for_lock.fetch_add(1, Ordering::SeqCst) == 0 {
                None
            } else {
                Some(4242)
            }
        },
        rx,
    )
    .await;
    assert_eq!(exit, GatewayExit::LockTaken(4242));
    assert_eq!(
        probes.load(Ordering::SeqCst),
        2,
        "probed before each attempt"
    );
    drop(tx);
}

/// Condition 5: shutdown stops the poller and drops the in-flight poll future,
/// which is what releases the PID lock the real gateway holds.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock() {
    let released = Arc::new(AtomicUsize::new(0));
    let released_in_poller = Arc::clone(&released);
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (tx, rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        supervise(
            move || {
                let guard = DropFlag(Arc::clone(&released_in_poller));
                let started = started_tx.clone();
                async move {
                    let _lock = guard;
                    let _ = started.send(());
                    std::future::pending::<Result<()>>().await
                }
            },
            || None,
            rx,
        )
        .await
    });
    started_rx.recv().await.expect("the poller is running");
    assert_eq!(released.load(Ordering::SeqCst), 0, "lock still held");

    ApiGateway {
        shutdown: Some(tx),
        handle: Some(handle),
    }
    .shutdown()
    .await;

    assert_eq!(
        released.load(Ordering::SeqCst),
        1,
        "shutdown must drop the poll future so the PID guard releases the lock"
    );
}

/// A host that decided not to poll still gets a handle, and stopping it does
/// nothing rather than panicking.
#[tokio::test]
async fn telegram_gateway_inert_handle_shutdown_is_a_no_op() {
    ApiGateway::inert().shutdown().await;
}

/// Fail-open check: a supervisor that panicked is reported, not propagated —
/// the API host's shutdown completes either way.
#[tokio::test]
async fn telegram_gateway_shutdown_reports_a_supervisor_that_panicked() {
    let (tx, _rx) = oneshot::channel();
    let handle: JoinHandle<GatewayExit> =
        tokio::spawn(async move { panic!("supervisor exploded") });
    ApiGateway {
        shutdown: Some(tx),
        handle: Some(handle),
    }
    .shutdown()
    .await;
}

/// Fail-open check: a supervisor that will not unwind is aborted at the grace
/// deadline, so shutdown can never hang the API host holding the lock.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_shutdown_aborts_a_stuck_supervisor() {
    let released = Arc::new(AtomicUsize::new(0));
    let released_in_task = Arc::clone(&released);
    let (tx, _rx) = oneshot::channel();
    let handle: JoinHandle<GatewayExit> = tokio::spawn(async move {
        let _lock = DropFlag(released_in_task);
        std::future::pending::<GatewayExit>().await
    });
    let started = tokio::time::Instant::now();
    ApiGateway {
        shutdown: Some(tx),
        handle: Some(handle),
    }
    .shutdown()
    .await;
    assert!(tokio::time::Instant::now() - started >= SHUTDOWN_GRACE);
    tokio::task::yield_now().await;
    assert_eq!(
        released.load(Ordering::SeqCst),
        1,
        "the abort must drop the task so the lock is released"
    );
}
