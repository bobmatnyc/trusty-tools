//! The swap half of a graceful recycle: start the replacement, swap it in,
//! drain the old sidecar, stop it.
//!
//! Why (#9828): split from `supervisor_recycle.rs` for the 500-SLOC cap; the
//! decision half (when to recycle) stays there.
//! What: [`perform_recycle`] and its drain.
//! Test: `supervisor_recycle_tests.rs`.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicU32, Ordering};
use std::time::Duration;

use tokio::process::Child;
use tokio::sync::{RwLock, watch};

use super::{RecycleHooks, RecycleTrigger, TrackedClient};
use crate::embedder_client::EmbedderClient;
use crate::embedder_client::supervisor::SupervisorConfig;
use crate::embedder_client::supervisor::supervisor_hard_limit::SpawnKind;

/// Drain poll cadence.
const DRAIN_POLL: Duration = Duration::from_millis(20);
/// Slack over the per-call timeout before a stalled drain gives up.
const DRAIN_MARGIN: Duration = Duration::from_secs(5);

/// Everything the supervision loop threads into a recycle.
pub(crate) struct RecycleCtx<'a> {
    pub binary_path: &'a Path,
    pub config: &'a SupervisorConfig,
    pub hooks: &'a RecycleHooks,
    pub child_slot: &'a tokio::sync::Mutex<Option<Child>>,
    pub client_slot: &'a RwLock<Arc<dyn EmbedderClient>>,
    pub child_pid_slot: &'a AtomicU32,
    pub shutdown_rx: &'a watch::Receiver<bool>,
}

/// How a recycle attempt ended.
pub(crate) enum RecycleOutcome {
    /// The replacement serves; the old sidecar was drained and stopped.
    Swapped(Arc<TrackedClient>),
    /// The replacement failed to start; the old sidecar keeps serving.
    SpawnFailed(Arc<TrackedClient>),
    /// #9828: the old sidecar exited while the replacement was starting; its
    /// exit is still in `child_slot` for the crash path to observe.
    OldExited(Arc<TrackedClient>),
    /// #10207: the old sidecar crossed the hard footprint limit while the
    /// replacement was starting; the replacement was abandoned and the
    /// caller kills the old sidecar through the crash path.
    HardLimit(Arc<TrackedClient>),
}

/// Replace the live sidecar with a fresh one, then drain and stop the old.
///
/// Why (#9828): a recycle must never fail or drop a caller's call and must
/// never count as a crash. Spawning first means there is never a moment with
/// no sidecar; draining before the kill means no in-flight call is lost.
/// What: spawns and probes the replacement while it watches the old child; if
/// the old child exits first, abandons the spawn (`OldExited`); if the old
/// child crosses the hard footprint limit first (#10207), abandons the spawn
/// (`HardLimit`) so the caller can kill it. On a spawn
/// failure logs WARN, counts a `spawn_failure`, and leaves the old sidecar
/// serving (`SpawnFailed`). On success swaps `client_slot`, retires the old
/// client so later calls forward to the new one, moves the new child into
/// `child_slot`, publishes the new pid, records the recycle, then drains:
/// it waits until no call is in flight on the old sidecar, cut short only by
/// a shutdown request or by no call finishing for `max(drain_secs, per-call
/// timeout + 5 s)`; then kills and reaps the old child.
/// Test: `recycle_on_footprint_ceiling_keeps_inflight_call_and_counts_no_failure`,
/// `recycle_drain_waits_for_calls_slower_than_drain_secs`,
/// `old_sidecar_crash_during_a_blocked_respawn_is_handled`,
/// `hard_limit_abandons_a_hanging_replacement_spawn`.
pub(crate) async fn perform_recycle(
    ctx: RecycleCtx<'_>,
    trigger: RecycleTrigger,
    old: Arc<TrackedClient>,
) -> RecycleOutcome {
    let old_pid = ctx.child_pid_slot.load(Ordering::Acquire);
    let mut child_guard = ctx.child_slot.lock().await;
    let Some(old_child) = child_guard.as_mut() else {
        return RecycleOutcome::OldExited(old);
    };
    // #9828: a crash of the old sidecar is not left unseen behind a slow or
    // failing replacement spawn.
    let spawned = tokio::select! {
        r = super::super::spawn_child(ctx.binary_path, ctx.config, SpawnKind::Respawn) => r,
        mb = hard_breach(&ctx, old_pid) => {
            tracing::warn!(
                trigger = trigger.as_str(),
                old_pid,
                footprint_mb = mb,
                "EmbedderSupervisor: sidecar crossed its hard footprint limit while its \
                 recycle replacement was starting — abandoning the replacement"
            );
            return RecycleOutcome::HardLimit(old);
        }
        _ = old_child.wait() => {
            tracing::warn!(
                trigger = trigger.as_str(),
                old_pid,
                "EmbedderSupervisor: sidecar exited while its recycle replacement was \
                 starting — handing it to the crash path"
            );
            return RecycleOutcome::OldExited(old);
        }
    };
    let (new_child, new_client) = match spawned {
        Ok(pair) => pair,
        Err(e) => {
            ctx.hooks
                .stats
                .spawn_failures
                .fetch_add(1, Ordering::Relaxed);
            tracing::warn!(
                trigger = trigger.as_str(),
                old_pid,
                "EmbedderSupervisor: recycle replacement failed to start — keeping the \
                 current sidecar: {e:#}"
            );
            return RecycleOutcome::SpawnFailed(old);
        }
    };
    let new_pid = new_child.id().unwrap_or(0);
    let tracked = TrackedClient::new(new_client);
    *ctx.client_slot.write().await = Arc::clone(&tracked) as Arc<dyn EmbedderClient>;
    old.retire(Arc::clone(&tracked) as Arc<dyn EmbedderClient>);
    let old_child = child_guard.replace(new_child);
    drop(child_guard);
    ctx.child_pid_slot.store(new_pid, Ordering::Release);
    tracing::info!(
        trigger = trigger.as_str(),
        old_pid,
        new_pid,
        "EmbedderSupervisor: recycling sidecar (old pid={old_pid}, new pid={new_pid}, \
         trigger={})",
        trigger.as_str()
    );

    // Counted once the new sidecar serves; the drain below only tidies up.
    ctx.hooks.stats.record(trigger, old_pid, new_pid);
    let stall = Duration::from_secs(ctx.config.recycle.drain_secs)
        .max(crate::embedder_client::stdio::embed_call_timeout() + DRAIN_MARGIN);
    drain(&old, stall, ctx.shutdown_rx).await;
    drop(old);
    if let Some(mut child) = old_child {
        if let Err(e) = child.start_kill() {
            tracing::debug!("EmbedderSupervisor: recycled sidecar already exited: {e}");
        }
        let _ = child.wait().await;
    }
    RecycleOutcome::Swapped(tracked)
}

/// #10207: resolve with the footprint once `pid` reads over the hard limit,
/// checked every check interval; never resolves when the limit is off.
async fn hard_breach(ctx: &RecycleCtx<'_>, pid: u32) -> u64 {
    let limit = ctx.config.recycle.effective_hard_footprint_mb();
    let Some(limit) = limit.filter(|_| pid != 0) else {
        return std::future::pending().await;
    };
    let mut tick = tokio::time::interval(ctx.config.recycle.effective_check_interval());
    loop {
        tick.tick().await;
        if let Some(mb) = (ctx.hooks.sampler)(pid)
            && mb > limit
        {
            return mb;
        }
    }
}

/// Wait until no call is in flight on the retired `old` client.
///
/// #9828: `old` is retired, so its in-flight count only falls. Every call on
/// it ends by itself (a reply, or its own per-call timeout), so the wait ends
/// unless calls stop finishing; `stall` (never below the per-call timeout plus
/// a margin) bounds only that case, measured from the last call to finish.
async fn drain(old: &TrackedClient, stall: Duration, shutdown_rx: &watch::Receiver<bool>) {
    let mut finished = old.finished.load(Ordering::Acquire);
    let mut progress_at = tokio::time::Instant::now();
    loop {
        if old.in_flight.load(Ordering::SeqCst) == 0 || *shutdown_rx.borrow() {
            return;
        }
        let now = tokio::time::Instant::now();
        let done = old.finished.load(Ordering::Acquire);
        if done != finished {
            finished = done;
            progress_at = now;
        }
        if now.duration_since(progress_at) >= stall {
            tracing::warn!(
                in_flight = old.in_flight.load(Ordering::SeqCst),
                "EmbedderSupervisor: no call on the recycled sidecar finished in {stall:?} — \
                 stopping it"
            );
            return;
        }
        tokio::time::sleep(DRAIN_POLL).await;
    }
}
