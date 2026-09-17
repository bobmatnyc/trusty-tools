//! Tests for the API host's per-bot Telegram gateway (#8190).
//!
//! Why: every condition #8190 states is decided by a function here that can be
//! driven without a bot token, a network, or a real `getUpdates` — which bots
//! this host must poll, the restart backoff, the wait on a lock another
//! process holds, the rescan that picks up a binding enabled later, and the
//! shutdown that drops each poll future and with it its lock guard. A live bot
//! is out of scope here, exactly as it is for `crate::telegram`.
//! What: the scan's credential resolution, the supervisor's poll attempt, its
//! lock probe and its decision are all injected, so an error arm is a closure
//! that returns `Err` rather than a revoked token. Timing assertions run on a
//! paused clock.
//! Test: this module is itself the coverage.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use tokio::sync::{Mutex, mpsc, oneshot};
use trusty_common::credentials::Secret;

use super::scan::{
    SkippedBinding, TelegramEntry, binding_receives_telegram, bindings_or_warn,
    global_channel_receives_telegram, group_by_token, roster_or_warn,
};
use super::supervisor::{
    FIRST_BACKOFF, GatewayDecision, MAX_BACKOFF, RunningBot, backoff_for, decide, supervise,
    supervise_bot,
};
use super::*;

/// An RAII stand-in for the lock guard `run_telegram_bot_for` holds.
///
/// Why: "shutdown releases the lock" is only observable as the poll future
/// being DROPPED, because that is what closes the descriptor holding the
/// `flock`. A drop counter is how a test sees that happen.
struct DropFlag(Arc<AtomicUsize>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.fetch_add(1, Ordering::SeqCst);
    }
}

fn entry(owner: &str, id: &str, credential_ref: Option<&str>) -> TelegramEntry {
    TelegramEntry {
        owner: owner.to_string(),
        binding_id: id.to_string(),
        credential_ref: credential_ref.map(str::to_string),
    }
}

/// A resolver that maps each credential reference to a distinct fake token,
/// and refuses anything it was not told about.
fn resolver(
    pairs: &[(&'static str, &'static str)],
) -> impl Fn(Option<&str>) -> Result<String, String> + use<> {
    let owned: Vec<(String, String)> = pairs
        .iter()
        .map(|(k, v)| ((*k).to_string(), (*v).to_string()))
        .collect();
    move |reference| {
        let key = reference.unwrap_or("telegram");
        owned
            .iter()
            .find(|(k, _)| k == key)
            .map(|(_, v)| v.clone())
            .ok_or_else(|| format!("no credential stored for `{key}`"))
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

// ------------------------------------------------------------------- scan

/// Owner ruling 2026-09-16: two assistants with two different bound tokens get
/// two pollers, each delivering only to its own assistant.
#[test]
fn telegram_gateway_two_tokens_are_two_pollers() {
    let entries = [
        entry("izzie", "tg-izzie", Some("telegram/izzie")),
        entry("cto-assistant", "tg-cto", Some("telegram/cto")),
    ];
    let (bots, skipped) = group_by_token(
        &entries,
        &resolver(&[
            ("telegram/izzie", "111:izzie-token"),
            ("telegram/cto", "222:cto-token"),
        ]),
    );
    assert!(skipped.is_empty(), "both tokens resolve: {skipped:?}");
    assert_eq!(bots.len(), 2, "two tokens, two pollers");
    let owners: Vec<Vec<String>> = bots
        .iter()
        .map(|b| b.owners().unwrap_or_default().to_vec())
        .collect();
    assert!(
        owners.contains(&vec!["izzie".to_string()]),
        "izzie's bot delivers only to izzie: {owners:?}"
    );
    assert!(
        owners.contains(&vec!["cto-assistant".to_string()]),
        "cto's bot delivers only to cto-assistant: {owners:?}"
    );
    assert!(
        owners.iter().all(|o| o.len() == 1),
        "no bot may wake an assistant bound to the other bot: {owners:?}"
    );
}

/// Owner ruling 2026-09-16: two bindings sharing ONE token are one poller that
/// dispatches by binding — Telegram terminates the older `getUpdates` when a
/// second one starts on the same bot, so two pollers would be a live outage.
#[test]
fn telegram_gateway_one_token_two_bindings_is_one_poller() {
    let entries = [
        entry("izzie", "tg-izzie", Some("telegram")),
        entry("writing-assistant", "tg-writing", None),
    ];
    let (bots, skipped) = group_by_token(&entries, &resolver(&[("telegram", "111:shared")]));
    assert!(skipped.is_empty(), "{skipped:?}");
    assert_eq!(bots.len(), 1, "one token is one poller");
    assert_eq!(
        bots[0].owners().unwrap_or_default(),
        ["izzie".to_string(), "writing-assistant".to_string()],
        "the single poller dispatches to both bindings' assistants"
    );
}

/// A binding whose token will not resolve is skipped, and the reason is
/// recorded rather than logged into a void.
#[test]
fn telegram_gateway_an_unresolvable_token_is_skipped_with_a_reason() {
    let entries = [
        entry("izzie", "tg-izzie", Some("telegram/izzie")),
        entry("ghost", "tg-ghost", Some("telegram/missing")),
    ];
    let (bots, skipped) = group_by_token(
        &entries,
        &resolver(&[("telegram/izzie", "111:izzie-token")]),
    );
    assert_eq!(bots.len(), 1, "the resolvable binding still polls");
    assert_eq!(skipped.len(), 1);
    assert_eq!(skipped[0].owner, "ghost");
    assert_eq!(skipped[0].binding_id, "tg-ghost");
    assert!(
        skipped[0].reason.contains("telegram/missing"),
        "the reason must name what did not resolve: {}",
        skipped[0].reason
    );
}

/// The gateway's start condition is the inbound path's claim condition.
#[test]
fn telegram_gateway_counts_an_enabled_receiving_binding() {
    assert!(binding_receives_telegram(&binding(telegram_binding(
        true, true
    ))));
}

/// A disabled, send-only, or targetless binding wants no updates.
#[test]
fn telegram_gateway_ignores_a_disabled_or_send_only_binding() {
    assert!(!binding_receives_telegram(&binding(telegram_binding(
        false, true
    ))));
    assert!(!binding_receives_telegram(&binding(telegram_binding(
        true, false
    ))));
    let mut blank = binding(telegram_binding(true, true));
    blank.target.clear();
    assert!(!binding_receives_telegram(&blank));
    let mut slack = binding(telegram_binding(true, true));
    slack.provider = "slack".into();
    assert!(!binding_receives_telegram(&slack));
}

/// A harness-wide `[[channels]]` telegram entry counts only when it routes to
/// an assistant — a global that wakes nobody is not a reason to poll.
#[test]
fn telegram_gateway_counts_a_routed_global_channel() {
    let routed = crate::channels::Channel {
        provider: "telegram".into(),
        enabled: true,
        receive_enabled: true,
        route_to: vec!["izzie".into()],
        ..crate::channels::Channel::default()
    };
    assert!(global_channel_receives_telegram(&routed));
    let unrouted = crate::channels::Channel {
        route_to: vec![],
        ..routed
    };
    assert!(!global_channel_receives_telegram(&unrouted));
}

/// Fail-open check: a roster this host cannot read is a recorded warning and
/// an empty scan, never a silent "nobody wants Telegram".
#[test]
fn telegram_gateway_roster_failure_warns_and_scans_nothing() {
    let mut warnings = Vec::new();
    let names = roster_or_warn(Err(anyhow::anyhow!("permission denied")), &mut warnings);
    assert!(names.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(
        warnings[0].contains("permission denied"),
        "the warning must carry the cause: {}",
        warnings[0]
    );
}

/// Fail-open check: one unreadable channel file skips ONE assistant, with a
/// recorded warning, and decides nothing for the others.
#[test]
fn telegram_gateway_an_unreadable_channel_file_warns_and_is_skipped() {
    let mut warnings = Vec::new();
    let loaded: Result<(String, String, Vec<u8>), &str> = Err("bad json");
    let bindings = bindings_or_warn("izzie", loaded, &mut warnings);
    assert!(bindings.is_empty());
    assert_eq!(warnings.len(), 1);
    assert!(warnings[0].contains("izzie"), "{}", warnings[0]);
    assert!(warnings[0].contains("bad json"), "{}", warnings[0]);
}

// --------------------------------------------------------------- decision

/// A resolvable bot means poll.
#[test]
fn telegram_gateway_polls_a_scanned_bot() {
    let bot = crate::telegram::TelegramBot::new(
        Secret::new("111:token".into()),
        Some(vec!["izzie".into()]),
        vec!["telegram".into()],
    );
    assert!(matches!(
        decide(vec![bot], &[]),
        GatewayDecision::Poll(bots) if bots.len() == 1
    ));
}

/// No Telegram channel at all means no poller, with one actionable reason.
#[test]
fn telegram_gateway_skips_without_an_enabled_binding() {
    let GatewayDecision::Skip(reason) = decide(Vec::new(), &[]) else {
        panic!("a host with no Telegram channel must not poll");
    };
    assert!(
        reason.contains("no enabled Telegram channel"),
        "the skip must say why: {reason}"
    );
}

/// Bindings that exist but resolve no token get a DIFFERENT reason, because
/// the operator's fix is different.
#[test]
fn telegram_gateway_skips_when_no_binding_resolves_a_token() {
    let skipped = [SkippedBinding {
        owner: "izzie".into(),
        binding_id: "tg-izzie".into(),
        reason: "no credential stored for `telegram/izzie`".into(),
    }];
    let GatewayDecision::Skip(reason) = decide(Vec::new(), &skipped) else {
        panic!("a host whose tokens will not resolve must not poll into a 401 loop");
    };
    assert!(
        reason.contains("telegram/izzie") && reason.contains("tg-izzie"),
        "the skip must carry the resolution failure: {reason}"
    );
}

/// The restart ladder doubles from five seconds and stops at the cap.
#[test]
fn telegram_gateway_backoff_doubles_then_caps() {
    assert_eq!(backoff_for(0), FIRST_BACKOFF, "0 is treated as the first");
    assert_eq!(backoff_for(1), FIRST_BACKOFF);
    assert_eq!(backoff_for(2), FIRST_BACKOFF * 2);
    assert_eq!(backoff_for(3), FIRST_BACKOFF * 4);
    assert_eq!(backoff_for(20), MAX_BACKOFF, "never past the cap");
    assert_eq!(backoff_for(u32::MAX), MAX_BACKOFF, "and never overflows");
}

// ------------------------------------------------------------- supervisor

/// Record each attempt's entry instant and report it to the test.
type Attempts = Arc<Mutex<Vec<tokio::time::Instant>>>;

/// #8190 finding 1 (HIGH): a live lock holder at start used to make the host
/// stand down PERMANENTLY, so an app relaunch racing the previous process's
/// exit left Telegram dead until someone noticed. The wait is now temporary.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_waits_out_a_lock_holder_then_polls() {
    let probes = Arc::new(AtomicUsize::new(0));
    let (started_tx, mut started_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let probes_for_lock = Arc::clone(&probes);
    let task = tokio::spawn(async move {
        supervise_bot(
            "telegram".into(),
            move || {
                let tx = started_tx.clone();
                async move {
                    let _ = tx.send(());
                    std::future::pending::<()>().await;
                    Ok(())
                }
            },
            move || {
                // Held for the first three probes, then released.
                (probes_for_lock.fetch_add(1, Ordering::SeqCst) < 3)
                    .then_some(crate::telegram::LockHolder { pid: Some(4242) })
            },
            shutdown_rx,
        )
        .await;
    });

    started_rx
        .recv()
        .await
        .expect("the poller must start once the lock frees");
    assert!(
        probes.load(Ordering::SeqCst) >= 4,
        "the holder must have been re-probed, not accepted once"
    );
    let _ = shutdown_tx.send(());
    task.await.expect("supervisor joins");
}

/// A failed poller is retried on the doubling ladder, and the host keeps
/// serving throughout.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_retries_a_failed_poller_with_backoff() {
    let attempts: Attempts = Arc::new(Mutex::new(Vec::new()));
    let (tick_tx, mut tick_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let recorded = Arc::clone(&attempts);
    let task = tokio::spawn(async move {
        supervise_bot(
            "telegram".into(),
            move || {
                let recorded = Arc::clone(&recorded);
                let tick = tick_tx.clone();
                async move {
                    recorded.lock().await.push(tokio::time::Instant::now());
                    let _ = tick.send(());
                    Err(anyhow::anyhow!("getUpdates refused"))
                }
            },
            || None,
            shutdown_rx,
        )
        .await;
    });

    for _ in 0..3 {
        tick_rx.recv().await.expect("three attempts");
    }
    let _ = shutdown_tx.send(());
    task.await.expect("supervisor joins");

    let seen = attempts.lock().await;
    assert_eq!(seen.len(), 3);
    assert_eq!(seen[1].duration_since(seen[0]), FIRST_BACKOFF);
    assert_eq!(seen[2].duration_since(seen[1]), FIRST_BACKOFF * 2);
}

/// #8190 finding 3 (MEDIUM): the failure count never reset, so a poller that
/// ran healthily for an hour restarted at the five-minute ceiling after one
/// blip. A run that outlasts the worst backoff now clears the ladder.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_resets_the_backoff_after_a_healthy_run() {
    let attempts: Attempts = Arc::new(Mutex::new(Vec::new()));
    let (tick_tx, mut tick_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let recorded = Arc::clone(&attempts);
    let round = Arc::new(AtomicUsize::new(0));
    let task = tokio::spawn(async move {
        supervise_bot(
            "telegram".into(),
            move || {
                let recorded = Arc::clone(&recorded);
                let tick = tick_tx.clone();
                let round = Arc::clone(&round);
                async move {
                    recorded.lock().await.push(tokio::time::Instant::now());
                    let _ = tick.send(());
                    // Attempt 2 is the healthy run: it outlasts MAX_BACKOFF.
                    if round.fetch_add(1, Ordering::SeqCst) == 1 {
                        tokio::time::sleep(MAX_BACKOFF).await;
                    }
                    Err(anyhow::anyhow!("getUpdates refused"))
                }
            },
            || None,
            shutdown_rx,
        )
        .await;
    });

    for _ in 0..3 {
        tick_rx.recv().await.expect("three attempts");
    }
    let _ = shutdown_tx.send(());
    task.await.expect("supervisor joins");

    let seen = attempts.lock().await;
    assert_eq!(
        seen[2].duration_since(seen[1]),
        MAX_BACKOFF + FIRST_BACKOFF,
        "a healthy run restarts the ladder at 5s, not the 10s a second \
         consecutive failure would have used"
    );
}

/// A poll future that RETURNS while the host is serving means updates stopped
/// arriving, which must never be silent — it is restarted like a failure.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_restarts_a_poller_that_returned_ok() {
    let calls = Arc::new(AtomicUsize::new(0));
    let (tick_tx, mut tick_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let counter = Arc::clone(&calls);
    let task = tokio::spawn(async move {
        supervise_bot(
            "telegram".into(),
            move || {
                counter.fetch_add(1, Ordering::SeqCst);
                let tick = tick_tx.clone();
                async move {
                    let _ = tick.send(());
                    Ok(())
                }
            },
            || None,
            shutdown_rx,
        )
        .await;
    });
    tick_rx.recv().await.expect("first run");
    tick_rx
        .recv()
        .await
        .expect("restarted after a clean return");
    let _ = shutdown_tx.send(());
    task.await.expect("supervisor joins");
    assert!(calls.load(Ordering::SeqCst) >= 2);
}

/// Shutdown drops the in-flight poll future, and with it the lock guard the
/// poller holds — the property that keeps the NEXT start from waiting.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock() {
    let dropped = Arc::new(AtomicUsize::new(0));
    let (ready_tx, ready_rx) = oneshot::channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let flag = Arc::clone(&dropped);
    let ready = Arc::new(Mutex::new(Some(ready_tx)));
    let task = tokio::spawn(async move {
        supervise_bot(
            "telegram".into(),
            move || {
                let guard = DropFlag(Arc::clone(&flag));
                let ready = Arc::clone(&ready);
                async move {
                    if let Some(tx) = ready.lock().await.take() {
                        let _ = tx.send(());
                    }
                    std::future::pending::<()>().await;
                    drop(guard);
                    Ok(())
                }
            },
            || None,
            shutdown_rx,
        )
        .await;
    });
    ready_rx.await.expect("the poller started");
    assert_eq!(dropped.load(Ordering::SeqCst), 0, "still polling");
    let _ = shutdown_tx.send(());
    task.await.expect("supervisor joins");
    assert_eq!(
        dropped.load(Ordering::SeqCst),
        1,
        "shutdown must drop the poll future, releasing the gateway lock"
    );
}

/// A poller task that never unwinds is aborted rather than hanging the host.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_shutdown_aborts_a_stuck_supervisor() {
    let (tx, _rx) = oneshot::channel();
    let handle = tokio::spawn(async { std::future::pending::<()>().await });
    let running = RunningBot::new(tx, handle);
    // Returns rather than hanging: the grace period expires and the task is
    // aborted, which drops whatever guard it held.
    running.stop().await;
}

/// A fake poller the outer supervisor can start, recording which bots it saw.
fn fake_start(
    started: Arc<Mutex<Vec<String>>>,
) -> impl FnMut(&crate::telegram::TelegramBot) -> RunningBot {
    move |bot| {
        let label = bot.label();
        let started = Arc::clone(&started);
        let (tx, rx) = oneshot::channel();
        let handle = tokio::spawn(async move {
            started.lock().await.push(label);
            let _ = rx.await;
        });
        RunningBot::new(tx, handle)
    }
}

fn fake_bot(reference: &str, token: &str, owner: &str) -> crate::telegram::TelegramBot {
    crate::telegram::TelegramBot::new(
        Secret::new(token.to_string()),
        Some(vec![owner.to_string()]),
        vec![reference.to_string()],
    )
}

/// #8190 finding 4 (MEDIUM): bindings were scanned once, so a binding enabled
/// after startup was never picked up. The supervisor re-scans and starts the
/// new bot's poller without a restart.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_starts_a_poller_for_each_new_bot() {
    let started = Arc::new(Mutex::new(Vec::new()));
    let (tick_tx, mut tick_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let round = Arc::new(AtomicUsize::new(0));
    let task = {
        let started = Arc::clone(&started);
        tokio::spawn(async move {
            supervise(
                move || {
                    let round = round.fetch_add(1, Ordering::SeqCst);
                    let tick = tick_tx.clone();
                    async move {
                        let _ = tick.send(());
                        let mut bots = vec![fake_bot("telegram/izzie", "111:a", "izzie")];
                        if round >= 1 {
                            // The operator enabled cto-assistant's binding.
                            bots.push(fake_bot("telegram/cto", "222:b", "cto-assistant"));
                        }
                        GatewayDecision::Poll(bots)
                    }
                },
                fake_start(started),
                shutdown_rx,
            )
            .await;
        })
    };
    for _ in 0..3 {
        tick_rx.recv().await.expect("three scans");
    }
    let _ = shutdown_tx.send(());
    task.await.expect("supervisor joins");
    let seen = started.lock().await;
    assert_eq!(
        seen.len(),
        2,
        "one poller per bot, started once each: {seen:?}"
    );
    assert!(seen.contains(&"telegram/izzie".to_string()), "{seen:?}");
    assert!(seen.contains(&"telegram/cto".to_string()), "{seen:?}");
}

/// A bot whose binding went away stops polling, releasing its lock.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_stops_a_poller_whose_binding_went_away() {
    let started = Arc::new(Mutex::new(Vec::new()));
    let (tick_tx, mut tick_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let round = Arc::new(AtomicUsize::new(0));
    let task = {
        let started = Arc::clone(&started);
        tokio::spawn(async move {
            supervise(
                move || {
                    let round = round.fetch_add(1, Ordering::SeqCst);
                    let tick = tick_tx.clone();
                    async move {
                        let _ = tick.send(());
                        let bots = if round == 0 {
                            vec![
                                fake_bot("telegram/izzie", "111:a", "izzie"),
                                fake_bot("telegram/cto", "222:b", "cto-assistant"),
                            ]
                        } else {
                            vec![fake_bot("telegram/izzie", "111:a", "izzie")]
                        };
                        GatewayDecision::Poll(bots)
                    }
                },
                fake_start(started),
                shutdown_rx,
            )
            .await;
        })
    };
    for _ in 0..3 {
        tick_rx.recv().await.expect("three scans");
    }
    let _ = shutdown_tx.send(());
    task.await.expect("supervisor joins");
    let seen = started.lock().await;
    assert_eq!(
        seen.iter().filter(|s| *s == "telegram/izzie").count(),
        1,
        "the surviving bot is never restarted: {seen:?}"
    );
    assert_eq!(
        seen.iter().filter(|s| *s == "telegram/cto").count(),
        1,
        "the departed bot started once and was stopped, not restarted: {seen:?}"
    );
}

/// #8190 finding 1 (HIGH), outer half: a `Skip` is a pause, not an exit. A host
/// that had nothing bound at boot must start polling when a binding appears.
#[tokio::test(start_paused = true)]
async fn telegram_gateway_skip_is_not_terminal() {
    let started = Arc::new(Mutex::new(Vec::new()));
    let (tick_tx, mut tick_rx) = mpsc::unbounded_channel();
    let (shutdown_tx, shutdown_rx) = oneshot::channel();
    let round = Arc::new(AtomicUsize::new(0));
    let task = {
        let started = Arc::clone(&started);
        tokio::spawn(async move {
            supervise(
                move || {
                    let round = round.fetch_add(1, Ordering::SeqCst);
                    let tick = tick_tx.clone();
                    async move {
                        let _ = tick.send(());
                        if round < 2 {
                            return GatewayDecision::Skip("nothing bound yet".into());
                        }
                        GatewayDecision::Poll(vec![fake_bot("telegram", "111:a", "izzie")])
                    }
                },
                fake_start(started),
                shutdown_rx,
            )
            .await;
        })
    };
    for _ in 0..3 {
        tick_rx.recv().await.expect("three scans");
    }
    // The third scan polls; give its start a turn to record.
    tokio::task::yield_now().await;
    let _ = shutdown_tx.send(());
    task.await.expect("supervisor joins");
    let seen = started.lock().await;
    assert_eq!(
        *seen,
        vec!["telegram".to_string()],
        "the gateway must start after two skipped scans: {seen:?}"
    );
}
