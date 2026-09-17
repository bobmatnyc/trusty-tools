//! Keeping one poller alive per bound bot, for as long as the API host serves
//! (#8190).
//!
//! Why (code-critic HIGH 1 and MEDIUM 3/4): the first attempt decided once at
//! startup and never asked again. A lock holder still exiting from the previous
//! app launch made the host stand down PERMANENTLY, so Telegram stayed dead
//! until someone restarted it; a binding enabled later was never picked up; and
//! the restart backoff never reset, so a poller that ran healthily for an hour
//! restarted at the 5-minute ceiling after one blip. Every one of those is the
//! same defect — a decision taken once — so the supervisor now re-asks.
//!
//! What: [`supervise`] re-runs the whole scan every [`RESCAN_INTERVAL`] and
//! reconciles the running set, starting, keeping, and stopping one poller per
//! distinct token. [`supervise_bot`] is one bot's own loop: a live lock holder
//! is a WAIT, not an exit, and a healthy run resets the backoff. Both take
//! their I/O as closures, so every arm below is reachable from a test with no
//! network, no credential store, and no real `getUpdates`.
//!
//! Test: `super::tests`.

use std::collections::BTreeMap;
use std::future::Future;
use std::time::Duration;

use anyhow::Result;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;
use tracing::{error, info, warn};

use crate::system_status::telegram_gateway as status;
use crate::telegram::{BotKey, LockHolder, TelegramBot};

use super::scan::SkippedBinding;

/// Backoff before the first restart of a failed poller.
pub(super) const FIRST_BACKOFF: Duration = Duration::from_secs(5);

/// Ceiling on the restart backoff, and the "this run was healthy" threshold.
///
/// Why: a revoked token never recovers on its own, so an unbounded doubling
/// would leave the host effectively dead once an operator fixes it. Five
/// minutes keeps retrying cheap and bounds recovery latency after a fix. The
/// same value doubles as the healthy-run threshold (see [`supervise_bot`]): a
/// poller that lasted at least as long as the worst backoff was working.
pub(super) const MAX_BACKOFF: Duration = Duration::from_secs(300);

/// How long between full rescans of this host's Telegram bindings.
///
/// Why (#8190 findings 1 and 4): both "a lock holder is still exiting" and "a
/// binding was enabled after startup" resolve themselves within about a minute
/// if anyone looks again. Sixty seconds is cheap — the scan is two config reads
/// and a credential lookup — and bounds how long either costs.
pub(super) const RESCAN_INTERVAL: Duration = Duration::from_secs(60);

/// What this host should do with Telegram right now.
///
/// Why (#8190): "the gateway is off" and "the gateway is off BECAUSE" are
/// different operator experiences. Every skip carries the one line the log
/// prints, so a host that does not poll says so instead of being silent.
/// Test: `telegram_gateway_polls_a_scanned_bot`,
/// `telegram_gateway_skips_without_an_enabled_binding`,
/// `telegram_gateway_skips_when_no_binding_resolves_a_token`.
#[derive(Debug)]
pub(super) enum GatewayDecision {
    /// Poll these bots — one poller each.
    Poll(Vec<TelegramBot>),
    /// Poll nothing. The string is the reason, logged verbatim.
    Skip(String),
}

/// Turn one scan into a decision.
///
/// Why: split from the scan so all three outcomes are testable without a
/// credential store, an assistant roster, or a lock file.
/// What: any resolvable bot means poll. Otherwise the reason distinguishes "no
/// Telegram channel wants updates" from "every one of them names a token that
/// will not resolve" — two different operator fixes.
/// Test: `telegram_gateway_polls_a_scanned_bot`,
/// `telegram_gateway_skips_without_an_enabled_binding`,
/// `telegram_gateway_skips_when_no_binding_resolves_a_token`.
pub(super) fn decide(bots: Vec<TelegramBot>, skipped: &[SkippedBinding]) -> GatewayDecision {
    if !bots.is_empty() {
        return GatewayDecision::Poll(bots);
    }
    if let Some(first) = skipped.first() {
        return GatewayDecision::Skip(format!(
            "telegram gateway: off — every Telegram binding on this host names a bot token that \
             will not resolve (`{}` on `{}`: {}). Store each assistant's bot token under the \
             credential reference its binding names (#8190)",
            first.binding_id, first.owner, first.reason
        ));
    }
    GatewayDecision::Skip(
        "telegram gateway: off — no enabled Telegram channel on this host wants updates. Add a \
         telegram binding with receive enabled to an assistant, or a global [[channels]] telegram \
         entry with route_to (#8190)"
            .into(),
    )
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

/// A poller task and the channel that stops it.
pub(super) struct RunningBot {
    shutdown: oneshot::Sender<()>,
    handle: JoinHandle<()>,
}

impl RunningBot {
    /// Build a handle over an already-spawned poller task.
    pub(super) fn new(shutdown: oneshot::Sender<()>, handle: JoinHandle<()>) -> Self {
        Self { shutdown, handle }
    }

    /// Signal the poller and wait for it to release its lock.
    ///
    /// Why (#8190): dropping a `JoinHandle` DETACHES the task rather than
    /// cancelling it, so a plain drop would leave the poller running and its
    /// `flock` held. Signalling and then awaiting is what makes the release
    /// observable; a poller that will not unwind inside [`SHUTDOWN_GRACE`] is
    /// aborted, which drops the guard for it.
    /// Test: `telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock`,
    /// `telegram_gateway_shutdown_aborts_a_stuck_supervisor`.
    pub(super) async fn stop(self) {
        let _ = self.shutdown.send(());
        let abort = self.handle.abort_handle();
        match tokio::time::timeout(SHUTDOWN_GRACE, self.handle).await {
            Ok(Ok(())) => {}
            Ok(Err(e)) => warn!(error = %e, "telegram gateway: poller task ended abnormally"),
            Err(_) => {
                abort.abort();
                warn!(
                    "telegram gateway: a poller did not stop within the shutdown grace period; \
                     aborted it to release the gateway lock"
                );
            }
        }
    }
}

/// How long a stop waits for one poller to unwind.
pub(super) const SHUTDOWN_GRACE: Duration = Duration::from_secs(5);

/// One running poller and the owners it was spawned with.
///
/// Why (#8190 round-2 finding 1, HIGH): a poller's `allowed_personas` are
/// FROZEN at spawn — `run_telegram_bot_for` clones them once and every dispatch
/// reads that clone. Reconciling the token SET alone therefore kept an old
/// poller alive across an owner change, so removing or disabling assistant B's
/// binding on a bot assistant A still binds left B in the live poller's filter
/// until the host restarted, and T-4 revocation never took effect. Remembering
/// what each running poller was started with is what makes the change
/// detectable.
/// What: `owners` is the scanned owner list the poller carries, already sorted
/// by [`super::scan::group_by_token`], so equality is a plain comparison.
/// Test: `telegram_gateway_restarts_a_poller_whose_owners_changed`.
struct RunningEntry {
    owners: Option<Vec<String>>,
    bot: RunningBot,
}

/// Run ONE bot's poller, restarting it until shutdown.
///
/// Why (#8190 finding 1): a live lock holder used to end this loop for good, so
/// an app relaunch racing the previous process's exit left Telegram dead until
/// someone noticed. Standing down is now a WAIT: the loop re-probes every
/// [`RESCAN_INTERVAL`] and starts the moment the lock frees.
/// What: before every attempt it probes `lock_holder`, logging once per
/// distinct holder rather than once per probe. An attempt that lasted at least
/// [`MAX_BACKOFF`] resets the failure count (#8190 finding 3), so a poller that
/// ran healthily for an hour restarts in five seconds, not five minutes. A poll
/// future that returns `Ok` is ALSO restarted — while the host serves, a
/// returned poller means updates stopped arriving. `shutdown` is checked before
/// the attempt, during the backoff sleep, and during the lock wait; the
/// `select!` drops the in-flight poll future, which drops the lock guard.
/// Test: `telegram_gateway_waits_out_a_lock_holder_then_polls`,
/// `telegram_gateway_retries_a_failed_poller_with_backoff`,
/// `telegram_gateway_restarts_a_poller_that_returned_ok`,
/// `telegram_gateway_resets_the_backoff_after_a_healthy_run`,
/// `telegram_gateway_shutdown_stops_the_poller_and_releases_the_lock`.
pub(super) async fn supervise_bot<A, AFut, L>(
    label: String,
    mut attempt: A,
    mut lock_holder: L,
    mut shutdown: oneshot::Receiver<()>,
) where
    A: FnMut() -> AFut,
    AFut: Future<Output = Result<()>>,
    L: FnMut() -> Option<LockHolder>,
{
    let mut failures: u32 = 0;
    let mut reported_holder: Option<String> = None;
    loop {
        if let Some(holder) = lock_holder() {
            let pid = holder.label();
            if reported_holder.as_deref() != Some(pid.as_str()) {
                warn!(
                    bot = %label, holder_pid = %pid,
                    "telegram gateway: another process is polling this bot; waiting for it to \
                     release the lock (#8190)"
                );
                reported_holder = Some(pid.clone());
            }
            status::record_state(
                &label,
                "waiting-for-lock",
                Some(format!("PID {pid} holds this bot's gateway lock")),
            );
            tokio::select! {
                biased;
                _ = &mut shutdown => return,
                () = tokio::time::sleep(RESCAN_INTERVAL) => continue,
            }
        }
        reported_holder = None;
        status::record_state(&label, "polling", None);
        let started = tokio::time::Instant::now();
        let outcome = tokio::select! {
            biased;
            _ = &mut shutdown => return,
            outcome = attempt() => outcome,
        };
        // #8190 finding 3: a run that outlasted the worst backoff was healthy,
        // so the next failure starts the ladder over rather than resuming at
        // the ceiling a long-dead fault left behind.
        if started.elapsed() >= MAX_BACKOFF {
            failures = 0;
        }
        failures = failures.saturating_add(1);
        let delay = backoff_for(failures);
        let detail = match outcome {
            Ok(()) => {
                warn!(
                    bot = %label, restart_in_secs = delay.as_secs(),
                    "telegram gateway: the long-poll loop returned while the API host is still \
                     serving; restarting it"
                );
                "the long-poll loop returned unexpectedly".to_string()
            }
            Err(e) => {
                let rendered = format!("{e:#}");
                error!(
                    bot = %label, error = %rendered, restart_in_secs = delay.as_secs(),
                    "telegram gateway: the long-poll loop failed; the API host is unaffected and \
                     the poller will be restarted"
                );
                rendered
            }
        };
        status::record_state(
            &label,
            "restarting",
            Some(format!("{detail}; retrying in {}s", delay.as_secs())),
        );
        tokio::select! {
            biased;
            _ = &mut shutdown => return,
            () = tokio::time::sleep(delay) => {}
        }
    }
}

/// Re-scan this host's Telegram bindings on a timer and keep one poller per bot.
///
/// Why (#8190 findings 1 and 4): the set of bots is not a startup constant. A
/// binding enabled later must start polling; a binding disabled or retargeted
/// must stop; a token that would not resolve at boot must be picked up once it
/// does. Re-running the whole decision is the only way all three hold, and it
/// is why `Skip` is no longer terminal.
/// What: `decide_now` yields this iteration's decision and `start` spawns one
/// poller for a bot. A `Skip` stops everything running and logs once per
/// DISTINCT reason, so a permanently-unconfigured host prints one line, not one
/// per minute. On shutdown every poller is stopped and awaited, which releases
/// every lock.
///
/// #8190 round-2 finding 1: reconciliation is per BOT, not per token set. A bot
/// whose scanned owners differ from the running poller's is restarted, because
/// the poller's `allowed_personas` cannot be changed in place — see
/// [`RunningEntry`].
/// Test: `telegram_gateway_starts_a_poller_for_each_new_bot`,
/// `telegram_gateway_stops_a_poller_whose_binding_went_away`,
/// `telegram_gateway_restarts_a_poller_whose_owners_changed`,
/// `telegram_gateway_skip_is_not_terminal`.
pub(super) async fn supervise<D, DFut, S>(
    mut decide_now: D,
    mut start: S,
    mut shutdown: oneshot::Receiver<()>,
) where
    D: FnMut() -> DFut,
    DFut: Future<Output = GatewayDecision>,
    S: FnMut(&TelegramBot) -> RunningBot,
{
    let mut running: BTreeMap<BotKey, RunningEntry> = BTreeMap::new();
    let mut reported_skip: Option<String> = None;
    loop {
        let decision = tokio::select! {
            biased;
            _ = &mut shutdown => break,
            decision = decide_now() => decision,
        };
        match decision {
            GatewayDecision::Skip(reason) => {
                if reported_skip.as_deref() != Some(reason.as_str()) {
                    info!("{reason}");
                    reported_skip = Some(reason);
                }
                stop_all(&mut running).await;
            }
            GatewayDecision::Poll(bots) => {
                reported_skip = None;
                let wanted: Vec<BotKey> = bots.iter().map(|b| b.key().clone()).collect();
                let departed: Vec<BotKey> = running
                    .keys()
                    .filter(|k| !wanted.contains(k))
                    .cloned()
                    .collect();
                for key in departed {
                    if let Some(entry) = running.remove(&key) {
                        info!("telegram gateway: a bot is no longer bound; stopping its poller");
                        entry.bot.stop().await;
                    }
                }
                for bot in &bots {
                    let owners = bot.owners().map(<[String]>::to_vec);
                    match running.get(bot.key()) {
                        Some(entry) if entry.owners == owners => continue,
                        Some(_) => {
                            // #8190: the live poller froze the OLD owner list at
                            // spawn, so a revoked binding stays in its filter
                            // until it is replaced.
                            if let Some(entry) = running.remove(bot.key()) {
                                info!(
                                    bot = %bot.label(),
                                    "telegram gateway: this bot's owning assistants changed; \
                                     restarting its poller so the new set takes effect (#8190)"
                                );
                                entry.bot.stop().await;
                            }
                        }
                        None => {}
                    }
                    info!(bot = %bot.label(), "telegram gateway: starting a poller (#8190)");
                    running.insert(
                        bot.key().clone(),
                        RunningEntry {
                            owners,
                            bot: start(bot),
                        },
                    );
                }
            }
        }
        tokio::select! {
            biased;
            _ = &mut shutdown => break,
            () = tokio::time::sleep(RESCAN_INTERVAL) => {}
        }
    }
    stop_all(&mut running).await;
}

/// Stop and await every running poller, releasing every lock.
async fn stop_all(running: &mut BTreeMap<BotKey, RunningEntry>) {
    for (_, entry) in std::mem::take(running) {
        entry.bot.stop().await;
    }
}
