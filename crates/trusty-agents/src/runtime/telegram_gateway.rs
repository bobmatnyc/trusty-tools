//! The Telegram long-poll gateway, supervised inside the `--api` host (#8190).
//!
//! Why: `tagent --api` is the production host (izzie runs `--api --port 8765`),
//! and it spawned every OTHER channel receiver — the eventstream listeners, the
//! channel migration sweep — but never the Telegram gateway. The gateway
//! started only under the standalone `--telegram` flag or the interactive REPL,
//! so a `[[channels]]` Telegram binding on an API host could never receive: no
//! process polled `getUpdates` at all.
//!
//! What: [`start_for_api_host`] decides whether this host should poll, and
//! spawns [`supervise`] when it should. The decision is [`decide`] — a
//! configured token AND an enabled receiving Telegram channel AND no other
//! poller holding the machine-wide lock. Telegram terminates the older poller
//! when a second `getUpdates` starts, so a second poller is a live outage and
//! the lock is a refusal, never a warning. [`supervise`] restarts a poller that
//! failed or returned, with exponential backoff, and stops the moment
//! [`ApiGateway::shutdown`] fires — which drops the in-flight poll future and
//! with it the PID guard, releasing the lock.
//!
//! Inbound routing is unchanged: a bound chat reaches
//! `agent_channels::inbound::receive_inbound` through `telegram::inbound::route`,
//! the same dispatch Slack uses (#7427).
//!
//! Test: `runtime::telegram_gateway_tests`.

use std::future::Future;
use std::path::PathBuf;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

/// Backoff before the first restart of a failed poller.
const FIRST_BACKOFF: Duration = Duration::from_secs(5);

/// Ceiling on the restart backoff.
///
/// Why: a revoked token never recovers on its own, so an unbounded doubling
/// would leave the host effectively dead once an operator fixes it. Five
/// minutes keeps retrying cheap and bounds recovery latency after a fix.
const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// How long [`ApiGateway::shutdown`] waits for the supervisor to unwind.
const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// Whether this host starts a Telegram poller, and why not when it does not.
///
/// Why (#8190): "the gateway is off" and "the gateway is off BECAUSE" are
/// different operator experiences. Every skip carries the one line the startup
/// log prints, so a host that does not poll says so instead of being silent.
/// Test: `telegram_gateway_starts_with_token_and_enabled_binding`,
/// `telegram_gateway_skips_without_an_enabled_binding`,
/// `telegram_gateway_skips_without_a_resolvable_token`,
/// `telegram_gateway_refuses_when_another_poller_holds_the_lock`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GatewayDecision {
    /// Poll: a token resolves, a binding wants updates, the lock is free.
    Start,
    /// Do not poll. The string is the reason, logged verbatim at startup.
    Skip(String),
}

/// Why the supervisor stopped.
///
/// Why: the supervisor is the only thing that can decide to stop polling, and
/// the two reasons are operationally different — a requested shutdown is
/// routine, a lock taken by another process means this host deliberately
/// stepped aside.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum GatewayExit {
    /// [`ApiGateway::shutdown`] fired; the poll future was dropped.
    ShutdownRequested,
    /// Another live process holds the gateway lock; we never started a second.
    LockTaken(i32),
}

/// Delay before restart attempt number `consecutive_failures`.
///
/// Why: a failing `getUpdates` (revoked token, DNS down) retried in a tight
/// loop is a rate-limit generator. Doubling from [`FIRST_BACKOFF`] and capping
/// at [`MAX_BACKOFF`] keeps a transient fault recovering in seconds and a
/// permanent one cheap.
/// What: `5s, 10s, 20s, …, 300s`. `consecutive_failures` is 1-based; 0 is
/// treated as 1 so a caller cannot ask for a zero delay.
/// Test: `telegram_gateway_backoff_doubles_then_caps`.
pub(super) fn backoff_for(consecutive_failures: u32) -> Duration {
    let steps = consecutive_failures.saturating_sub(1).min(16);
    FIRST_BACKOFF.saturating_mul(1u32 << steps).min(MAX_BACKOFF)
}

/// Decide whether this host starts the Telegram gateway.
///
/// Why: split from [`start_for_api_host`] so all four outcomes are testable
/// without a credential store, an assistant roster, or a PID file.
/// What: a host with no enabled receiving Telegram channel has nothing to
/// deliver to; a host whose token will not resolve would poll into a 401 loop
/// that reads like a network fault; a live lock holder means another poller
/// already owns `getUpdates` and starting a second would terminate it.
/// Test: `telegram_gateway_starts_with_token_and_enabled_binding`,
/// `telegram_gateway_skips_without_an_enabled_binding`,
/// `telegram_gateway_skips_without_a_resolvable_token`,
/// `telegram_gateway_refuses_when_another_poller_holds_the_lock`.
pub(super) fn decide(
    has_receiving_channel: bool,
    token_error: Option<String>,
    lock_holder: Option<i32>,
) -> GatewayDecision {
    if !has_receiving_channel {
        return GatewayDecision::Skip(
            "telegram gateway: off — no enabled Telegram channel on this host wants updates. \
             Add a telegram binding with receive enabled to an assistant, or a global \
             [[channels]] telegram entry with route_to (#8190)"
                .into(),
        );
    }
    if let Some(error) = token_error {
        return GatewayDecision::Skip(format!(
            "telegram gateway: off — the bot token did not resolve ({error}). Set \
             TELEGRAM_BOT_TOKEN in .env.local, or point the binding's credential reference at a \
             stored Telegram credential (#8190)"
        ));
    }
    if let Some(pid) = lock_holder {
        return GatewayDecision::Skip(format!(
            "telegram gateway: refusing to start a second poller — PID {pid} already holds the \
             gateway lock on this machine. Telegram terminates the older getUpdates when a \
             second one starts, so this host leaves polling to that process (#8190)"
        ));
    }
    GatewayDecision::Start
}

/// Whether any enabled channel on this host wants Telegram updates.
///
/// Why: the gateway exists to feed channel bindings, so a host with none must
/// not hold the machine-wide lock or poll Telegram at all.
/// What: mirrors the two ways `receive_inbound` can claim a Telegram event
/// (#7609) — an assistant binding with `enabled` and `receive_enabled` set, or
/// a harness-wide `[[channels]]` telegram entry that both receives and names a
/// `route_to` assistant. An assistant whose channel file will not load is
/// skipped, exactly as the inbound scan skips it.
/// Test: `telegram_gateway_counts_an_enabled_receiving_binding`,
/// `telegram_gateway_ignores_a_disabled_or_send_only_binding`,
/// `telegram_gateway_counts_a_routed_global_channel`.
async fn has_receiving_telegram_channel() -> bool {
    let globals = crate::mcp::config::GlobalConfig::load().await.channels;
    if globals.iter().any(global_channel_receives_telegram) {
        return true;
    }
    let dirs = crate::agents::agents_dir_candidates();
    for name in roster_or_warn(crate::listeners::wake::candidate_agent_names().await) {
        let loaded = crate::api::server::agent_channels::load_at(&dirs, &name).await;
        if bindings_or_warn(&name, loaded)
            .iter()
            .any(binding_receives_telegram)
        {
            return true;
        }
    }
    false
}

/// The assistant roster, or an empty scan with a warning.
///
/// Why (#8190, fail-open check): a roster this host cannot enumerate is not
/// "no assistant wants Telegram" — it is an unanswered question, and answering
/// it `false` silently leaves the gateway off with nothing in the log saying
/// why. The scan still proceeds as if empty, because starting a poller on a
/// host whose bindings we cannot read would hold the machine-wide lock for a
/// gateway that can deliver to nobody.
/// Test: `telegram_gateway_roster_failure_warns_and_scans_nothing`.
fn roster_or_warn(roster: Result<Vec<String>>) -> Vec<String> {
    roster.unwrap_or_else(|e| {
        warn!(
            error = %format!("{e:#}"),
            "telegram gateway: the assistant roster could not be read; the gateway start scan \
             sees no Telegram channels and the gateway stays off (#8190)"
        );
        Vec::new()
    })
}

/// One assistant's bindings, or none with a warning.
///
/// Why (#8190, fail-open check): a single unreadable channel file must not
/// decide the gateway for every OTHER assistant, and must not vanish silently
/// either — the same rule `receive_inbound` applies to the identical read.
/// Test: `telegram_gateway_an_unreadable_channel_file_warns_and_is_skipped`.
fn bindings_or_warn<P, R, B, E: std::fmt::Debug>(
    name: &str,
    loaded: Result<(P, R, Vec<B>), E>,
) -> Vec<B> {
    loaded.map(|(_, _, bindings)| bindings).unwrap_or_else(|e| {
        warn!(
            assistant = %name,
            error = ?e,
            "telegram gateway: this assistant's channels could not be read; it is skipped by the \
             gateway start scan (#8190)"
        );
        Vec::new()
    })
}

/// Whether one saved binding wants Telegram updates delivered to it.
///
/// Why: the gateway's start condition has to be the inbound path's claim
/// condition — `first_telegram_credential_ref` selects on exactly these three
/// flags, so a host that would claim an event is a host that must poll.
/// What: a blank target is excluded — an unresolved overlay addresses no chat.
/// Test: `telegram_gateway_counts_an_enabled_receiving_binding`,
/// `telegram_gateway_ignores_a_disabled_or_send_only_binding`.
pub(super) fn binding_receives_telegram(
    binding: &crate::api::server::agent_channels::Binding,
) -> bool {
    binding.provider == "telegram"
        && binding.enabled
        && binding.receive_enabled
        && !binding.target.is_empty()
}

/// Whether one harness-wide channel delivers Telegram updates to an assistant.
///
/// Test: `telegram_gateway_counts_a_routed_global_channel`.
pub(super) fn global_channel_receives_telegram(channel: &crate::channels::Channel) -> bool {
    channel.provider == "telegram"
        && channel.enabled
        && channel.receive_enabled
        && !channel.route_to.is_empty()
}

/// Run one poller, restarting it until shutdown.
///
/// Why: a gateway that stops on its first failure is the same outage as never
/// starting one, and a gateway that takes the API host down with it is worse.
/// The supervisor owns both halves: every attempt failure is logged and
/// retried with [`backoff_for`], and the whole loop lives in a spawned task so
/// nothing here can fail the HTTP server.
/// What: before every attempt it re-probes `lock_holder`, so a standalone
/// `--telegram` that started during a backoff window is stepped aside for
/// rather than fought with. A poll future that returns `Ok` is ALSO restarted —
/// while the API host is up, a returned poller means updates stopped arriving,
/// which must never be silent. `shutdown` is checked before the attempt and
/// during the backoff sleep, and the `select!` drops the in-flight poll future,
/// which drops the PID guard it holds.
/// Test: `telegram_gateway_retries_a_failed_poller_with_backoff`,
/// `telegram_gateway_restarts_a_poller_that_returned_ok`,
/// `telegram_gateway_stops_when_the_lock_is_taken_during_backoff`,
/// `telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock`.
pub(super) async fn supervise<A, AFut, L>(
    mut attempt: A,
    mut lock_holder: L,
    mut shutdown: oneshot::Receiver<()>,
) -> GatewayExit
where
    A: FnMut() -> AFut,
    AFut: Future<Output = Result<()>>,
    L: FnMut() -> Option<i32>,
{
    let mut failures: u32 = 0;
    loop {
        // #8190: a poller that took the lock while we were backing off owns
        // getUpdates now; starting beside it would terminate one of the two.
        if let Some(pid) = lock_holder() {
            warn!(
                pid,
                "telegram gateway: another poller holds the gateway lock; this host stands down"
            );
            return GatewayExit::LockTaken(pid);
        }
        let outcome = tokio::select! {
            biased;
            _ = &mut shutdown => return GatewayExit::ShutdownRequested,
            outcome = attempt() => outcome,
        };
        failures = failures.saturating_add(1);
        let delay = backoff_for(failures);
        match outcome {
            Ok(()) => warn!(
                restart_in_secs = delay.as_secs(),
                "telegram gateway: the long-poll loop returned while the API host is still \
                 serving; restarting it"
            ),
            Err(e) => error!(
                error = %format!("{e:#}"),
                restart_in_secs = delay.as_secs(),
                "telegram gateway: the long-poll loop failed; the API host is unaffected and the \
                 poller will be restarted"
            ),
        }
        tokio::select! {
            biased;
            _ = &mut shutdown => return GatewayExit::ShutdownRequested,
            _ = tokio::time::sleep(delay) => {}
        }
    }
}

/// The API host's handle on its Telegram gateway.
///
/// Why: the poller holds a machine-wide PID lock, so the host that started it
/// must be able to stop it — a lock leaked past shutdown makes the NEXT start
/// refuse. `None` fields are the inert handle a host that decided not to poll
/// gets, so the `--api` call site is the same either way.
/// Test: `telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock`,
/// `telegram_gateway_inert_handle_shutdown_is_a_no_op`.
pub(super) struct ApiGateway {
    shutdown: Option<oneshot::Sender<()>>,
    handle: Option<JoinHandle<GatewayExit>>,
}

impl ApiGateway {
    /// A handle for a host that is not polling.
    pub(super) fn inert() -> Self {
        Self {
            shutdown: None,
            handle: None,
        }
    }

    /// Stop the poller and wait for the PID lock to be released.
    ///
    /// Why (#8190): dropping a `JoinHandle` detaches the task rather than
    /// cancelling it, so a plain drop at process exit would leave the poller
    /// running until the runtime tore down — and the PID guard's `Drop` might
    /// never run. Signalling and then awaiting the task is what makes the
    /// release observable.
    /// What: sends on the shutdown channel, then awaits the supervisor for
    /// [`SHUTDOWN_GRACE`]; a supervisor that has not unwound by then is
    /// aborted so shutdown can never hang the host.
    /// Test: `telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock`,
    /// `telegram_gateway_shutdown_aborts_a_stuck_supervisor`,
    /// `telegram_gateway_shutdown_reports_a_supervisor_that_panicked`.
    pub(super) async fn shutdown(mut self) {
        let Some(handle) = self.handle.take() else {
            return;
        };
        if let Some(tx) = self.shutdown.take() {
            let _ = tx.send(());
        }
        let abort = handle.abort_handle();
        match tokio::time::timeout(SHUTDOWN_GRACE, handle).await {
            Ok(Ok(exit)) => info!(?exit, "telegram gateway: stopped"),
            Ok(Err(e)) => warn!(error = %e, "telegram gateway: supervisor task ended abnormally"),
            Err(_) => {
                // #8190: the poller holds the machine-wide PID lock, and the
                // guard only releases when its future is dropped. Aborting is
                // what drops it when the supervisor will not unwind on its own.
                abort.abort();
                warn!(
                    "telegram gateway: supervisor did not stop within the shutdown grace period; \
                     aborted it to release the gateway lock"
                );
            }
        }
    }
}

/// Start the Telegram gateway for this `--api` host, when it should run.
///
/// Why (#8190): the one call the `--api` branch makes. Everything it needs to
/// decide — the channel scan, the credential resolution, the lock probe — is
/// read here so the call site stays a two-line hook and this module owns the
/// policy.
/// What: returns an inert [`ApiGateway`] after logging exactly one reason line
/// when the host should not poll; otherwise spawns [`supervise`] over
/// `telegram::run_telegram_bot` and returns a handle that stops it. Never
/// returns an error: no Telegram misconfiguration may fail API startup.
/// Test: `telegram_gateway_starts_with_token_and_enabled_binding` and the other
/// `decide` cases pin the policy; the spawn itself is exercised by
/// `telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock` over the
/// same supervisor.
pub(super) async fn start_for_api_host() -> ApiGateway {
    let has_channel = has_receiving_telegram_channel().await;
    // Resolved through the credential authority under the reference a
    // receiving binding names — the same one the poller itself resolves, so
    // this pre-check cannot disagree with it (#7427).
    let credential_ref = crate::api::server::agent_channels::telegram_poll_credential_ref().await;
    let token_error = crate::channels::telegram_poll_token(credential_ref.as_deref())
        .err()
        .map(|e| e.to_string());
    match decide(
        has_channel,
        token_error,
        crate::telegram::gateway_lock_holder(),
    ) {
        GatewayDecision::Skip(reason) => {
            info!("{reason}");
            ApiGateway::inert()
        }
        GatewayDecision::Start => {
            let project_path = crate::ctrl::detect_self_project()
                .or_else(|| std::env::current_dir().ok())
                .unwrap_or_else(|| PathBuf::from("."));
            info!(
                project = %project_path.display(),
                "telegram gateway: starting in-process on the API host (#8190)"
            );
            let (tx, rx) = oneshot::channel();
            let handle = tokio::spawn(async move {
                supervise(
                    || {
                        // A fresh pending-pairs map per attempt: the API host
                        // has no REPL to issue `/telegram pair` codes, and a
                        // bound chat needs none — the binding IS the grant
                        // (#7427).
                        crate::telegram::run_telegram_bot(
                            project_path.clone(),
                            crate::telegram::new_pending_pairs(),
                        )
                    },
                    crate::telegram::gateway_lock_holder,
                    rx,
                )
                .await
            });
            ApiGateway {
                shutdown: Some(tx),
                handle: Some(handle),
            }
        }
    }
}

#[cfg(test)]
#[path = "telegram_gateway_tests.rs"]
mod telegram_gateway_tests;
