//! The Telegram long-poll gateway, supervised inside the `--api` host (#8190).
//!
//! Why: `tagent --api` is the production host (izzie runs `--api --port 8765`),
//! and it spawned every OTHER channel receiver — the eventstream listeners, the
//! channel migration sweep — but never the Telegram gateway. A `[[channels]]`
//! Telegram binding on an API host could therefore never receive: no process
//! polled `getUpdates` at all.
//!
//! Owner ruling 2026-09-16: each assistant has its own Telegram bot, carried on
//! the binding's own `credential_ref` exactly as a Slack binding carries its
//! workspace token. So this host runs one poller per DISTINCT bound token, each
//! delivering only to the assistants that own that bot, with the gateway lock
//! and the pairing state keyed per bot. A single machine-wide
//! `TELEGRAM_BOT_TOKEN` routed by chat pairing is explicitly not the target.
//!
//! What: [`start_for_api_host`] always spawns [`supervisor::supervise`], which
//! re-runs [`scan::scan`] every [`supervisor::RESCAN_INTERVAL`] and reconciles
//! the running pollers against what it found. Nothing here can fail API
//! startup, and [`ApiGateway::shutdown`] stops every poller and releases every
//! lock.
//!
//! Inbound routing reaches `agent_channels::inbound::receive_inbound` through
//! `telegram::inbound::route` (#7427), now with this bot's owners as the
//! `allowed_personas` filter — which is what keeps a message on izzie's bot
//! from waking an assistant bound only to cto-assistant's.
//!
//! Test: `tests`.

mod scan;
mod supervisor;

use std::path::PathBuf;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{info, warn};

use crate::system_status::telegram_gateway as status;
use crate::telegram::TelegramBot;

use supervisor::{GatewayDecision, RunningBot, SHUTDOWN_GRACE};

/// The API host's handle on its Telegram gateway.
///
/// Why: the pollers hold per-bot locks, so the host that started them must be
/// able to stop them — a lock leaked past shutdown makes the next start wait.
/// Test: `telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock`.
pub(super) struct ApiGateway {
    shutdown: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

impl ApiGateway {
    /// Stop every poller and wait for the locks to be released.
    ///
    /// Why (#8190): dropping a `JoinHandle` detaches the task rather than
    /// cancelling it, so a plain drop at process exit would leave the
    /// supervisor running until the runtime tore down — and the lock guards
    /// might never drop. Signalling and then awaiting is what makes the release
    /// observable.
    /// What: sends on the shutdown channel, then awaits the supervisor for
    /// [`SHUTDOWN_GRACE`]; a supervisor that has not unwound by then is aborted
    /// so shutdown can never hang the host.
    /// Test: `telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock`,
    /// `telegram_gateway_shutdown_aborts_a_stuck_supervisor`.
    pub(super) async fn shutdown(self) {
        let _ = self.shutdown.send(());
        let abort = self.handle.abort_handle();
        match tokio::time::timeout(SHUTDOWN_GRACE, self.handle).await {
            Ok(Ok(())) => info!("telegram gateway: stopped"),
            Ok(Err(e)) => warn!(error = %e, "telegram gateway: supervisor task ended abnormally"),
            Err(_) => {
                // #8190: each poller holds a per-bot lock released only when
                // its guard drops. Aborting is what drops them when the
                // supervisor will not unwind on its own.
                abort.abort();
                warn!(
                    "telegram gateway: supervisor did not stop within the shutdown grace period; \
                     aborted it to release the gateway locks"
                );
            }
        }
        status::clear();
    }
}

/// One scan, as a decision plus the status rows it produces.
///
/// Why: the scan's three outputs — the bots to poll, the bindings it had to
/// skip, and its own read failures — all belong in the status surface, and
/// publishing them is the same step as deciding. Keeping that in one function
/// is what stops a future rescan from updating one and not the other.
/// What: records the scan, then returns the decision. Each scanned bot's row
/// carries the [`status::STARTING`] placeholder, which
/// [`status::record_scan`] replaces with the live state the previous scan's row
/// held — a rescan must not report a healthy poller as perpetually starting
/// (#8190).
/// Test: `telegram_gateway_status_records_a_skipped_binding`,
/// `telegram_gateway_status_a_rescan_preserves_a_polling_row`.
async fn decide_and_publish() -> GatewayDecision {
    let found = scan::scan().await;
    let mut rows: Vec<status::TelegramBotStatus> = found
        .bots
        .iter()
        .map(|bot| status::TelegramBotStatus {
            credential_refs: bot.label(),
            assistants: bot.owners().unwrap_or_default().to_vec(),
            state: status::STARTING.into(),
            detail: None,
        })
        .collect();
    rows.extend(found.skipped.iter().map(|s| status::TelegramBotStatus {
        credential_refs: s.binding_id.clone(),
        assistants: vec![s.owner.clone()],
        state: "skipped".into(),
        detail: Some(s.reason.clone()),
    }));
    status::record_scan(rows, found.warnings.clone());
    supervisor::decide(found.bots, &found.skipped)
}

/// Spawn one poller task for `bot`.
///
/// Why: the only place a real `getUpdates` loop, a real lock path, and the
/// supervisor's closures meet. Split out so [`supervisor::supervise`] itself
/// never mentions teloxide and stays testable.
/// What: each attempt gets a fresh duplicate of the bot (the poller consumes
/// it) and a fresh pending-pairs map — the API host has no REPL to issue
/// `/telegram pair` codes, and a bound chat needs none: the binding IS the
/// grant (#7427).
/// Test: `telegram_gateway_starts_a_poller_for_each_new_bot` covers the
/// supervisor half; the teloxide half is exercised only live.
fn spawn_poller(bot: &TelegramBot, project_path: PathBuf) -> RunningBot {
    let label = bot.label();
    let poller = bot.duplicate();
    let lock_path = crate::telegram::telegram_pid_file_path_for(bot.key());
    let (tx, rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        supervisor::supervise_bot(
            label,
            || {
                crate::telegram::run_telegram_bot_for(
                    poller.duplicate(),
                    project_path.clone(),
                    crate::telegram::new_pending_pairs(),
                )
            },
            || crate::telegram::gateway_lock_holder_at(&lock_path),
            rx,
        )
        .await;
    });
    RunningBot::new(tx, handle)
}

/// Start the Telegram gateway for this `--api` host.
///
/// Why (#8190): the one call the `--api` branch makes. It ALWAYS spawns — the
/// decision of whether to poll belongs to the supervisor, which re-asks every
/// minute, because a host that decided "no" once at startup is exactly the
/// permanent outage code-critic finding 1 names.
/// What: returns a handle that stops the supervisor and every poller under it.
/// Never returns an error: no Telegram misconfiguration may fail API startup.
/// Test: `telegram_gateway_skip_is_not_terminal`,
/// `telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock`.
pub(super) fn start_for_api_host() -> ApiGateway {
    let project_path = crate::ctrl::detect_self_project()
        .or_else(|| std::env::current_dir().ok())
        .unwrap_or_else(|| PathBuf::from("."));
    info!(
        project = %project_path.display(),
        "telegram gateway: supervising in-process on the API host (#8190)"
    );
    let (tx, rx) = oneshot::channel();
    let handle = tokio::spawn(async move {
        supervisor::supervise(
            decide_and_publish,
            |bot| spawn_poller(bot, project_path.clone()),
            rx,
        )
        .await;
    });
    ApiGateway {
        shutdown: tx,
        handle,
    }
}

#[cfg(test)]
mod tests;
