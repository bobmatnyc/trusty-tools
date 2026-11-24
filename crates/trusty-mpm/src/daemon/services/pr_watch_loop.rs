//! The daemon's PR watch loop: head-aware CI transitions, one wake each
//! (#11673 AC2-AC4).
//!
//! Why: a PM waiting on a PR's required checks stalled until someone relayed
//! the result (#11595). The daemon outlives the PM's turn and has no inbound
//! endpoint, so it polls the tracked-PR table outbound, on a cadence.
//! What: [`PrWatchLoop::tick`] reads the table, takes at most
//! [`MAX_ROWS_PER_TICK`] rows round-robin, and per row reads the PR through
//! [`PrReader`]. A merged or closed PR's row is removed. Otherwise the
//! required contexts are judged on the current head ([`judge`], re-reading
//! the contexts on a new head) and [`detect`] names any transition. A
//! transition is delivered in three steps — the `ci_transition` bus event
//! ([`Emitter`]), the owner's ledger wait ([`LedgerPort`]), the owner's pane
//! line ([`Waker`]) — each marked done on the row under the transition's
//! dedupe key, so a retry resumes at the failed step and a restart or a bus
//! replay repeats nothing. A shim rate-limit refusal ends the tick and pauses
//! the loop until the reset ([`backoff_secs`]).
//!
//! # Fail direction
//!
//! No failed step advances state. A failed emit, ledger write or wake leaves
//! its mark and the row's (head, verdict) unchanged, so the next tick detects
//! the same transition, under the same key, and retries that step. Only a
//! delivered wake records the new (head, verdict).
//! Test: `pr_watch_loop_tests.rs`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tracing::{info, warn};
use trusty_common::control_bus::{EventId, HarnessEvent, HarnessPayload, HarnessSource};

use crate::core::gh_rate_limit::{backoff_secs, classify};
use crate::core::pr_watch::gh::PrReader;
use crate::core::pr_watch::table::{Table, WatchRow};
use crate::core::pr_watch::verdict::{Kind, Verdict, dedupe_key, detect, judge};

/// Rows read per tick; the rest wait for the next tick, round-robin.
///
/// Why: each row costs one GraphQL read (three on a new head), and the loop
/// shares the host's `gh` budget with every PM.
pub const MAX_ROWS_PER_TICK: usize = 8;

/// Environment variable that disables the loop: `0`/`false`/`off`/`no`.
pub const ENV_ENABLED: &str = "TRUSTY_MPM_PR_WATCH";

/// Environment variable overriding the cadence, in seconds.
pub const ENV_INTERVAL_SECS: &str = "TRUSTY_MPM_PR_WATCH_INTERVAL_SECS";

/// Default cadence: one minute.
pub const DEFAULT_INTERVAL_SECS: u64 = 60;

/// The `Hook` kind of the bus event.
pub const EVENT_KIND: &str = "ci_transition";

/// One transition to deliver.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Transition {
    /// What changed.
    pub kind: Kind,
    /// The head it was observed on.
    pub head: String,
    /// The verdict on that head.
    pub verdict: Verdict,
    /// The (repo, PR, head, kind) dedupe key.
    pub key: String,
    /// The required contexts judged on `head`.
    pub required: Vec<String>,
}

/// Publishes the `ci_transition` event.
#[async_trait::async_trait]
pub trait Emitter: Send + Sync {
    /// Hand `event` to the bus, or to its spool when no bus producer runs.
    async fn emit(&self, event: HarnessEvent) -> anyhow::Result<()>;
}

/// Writes the owner's ledger wait.
#[async_trait::async_trait]
pub trait LedgerPort: Send + Sync {
    /// Set the owner's `ci` wait row for this PR; `Ok` also when skipped.
    async fn set_ci_wait(&self, row: &WatchRow, t: &Transition) -> anyhow::Result<()>;
}

/// Types the wake line into the owner's pane.
#[async_trait::async_trait]
pub trait Waker: Send + Sync {
    /// Send `line` to the managed session named `owner`.
    async fn wake(&self, owner: &str, line: &str) -> anyhow::Result<()>;
}

/// The loop's outside world.
#[derive(Clone)]
pub struct Ports {
    /// `gh` reads.
    pub reader: Arc<dyn PrReader>,
    /// The bus.
    pub emitter: Arc<dyn Emitter>,
    /// The ledger.
    pub ledger: Arc<dyn LedgerPort>,
    /// The pane.
    pub waker: Arc<dyn Waker>,
}

/// What one tick did.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct TickReport {
    /// Rows read.
    pub read: usize,
    /// Wakes delivered.
    pub woken: usize,
    /// Rows removed because their PR merged or closed.
    pub removed: usize,
    /// Steps that failed and will be retried.
    pub failed: usize,
    /// Whether a rate-limit refusal ended the tick.
    pub rate_limited: bool,
}

/// The loop state: the table, the ports, the round-robin cursor and backoff.
pub struct PrWatchLoop {
    table: Table,
    ports: Ports,
    interval_s: u64,
    cursor: usize,
    paused_until: u64,
    streak: u32,
}

impl PrWatchLoop {
    /// A loop over `table` with `ports`, ticking every `interval_s`.
    pub fn new(table: Table, ports: Ports, interval_s: u64) -> Self {
        Self {
            table,
            ports,
            interval_s,
            cursor: 0,
            paused_until: 0,
            streak: 0,
        }
    }

    /// Run one tick at unix time `now`; see the module doc.
    ///
    /// Test: `a_green_head_wakes_the_owner_once_across_ticks_and_restart`,
    /// `a_merged_pr_row_is_removed`, `each_tick_reads_at_most_the_bound`,
    /// `a_rate_limited_read_pauses_the_watch_until_the_reset`.
    pub async fn tick(&mut self, now: u64) -> TickReport {
        let mut report = TickReport::default();
        if now < self.paused_until {
            report.rate_limited = true;
            return report;
        }
        let table = self.table.clone();
        let rows = match blocking(move || table.list()).await {
            Ok(rows) => rows,
            Err(e) => {
                warn!("pr watch: cannot read the table: {e:#}");
                return report;
            }
        };
        if rows.is_empty() {
            return report;
        }
        let start = self.cursor % rows.len();
        let take = rows.len().min(MAX_ROWS_PER_TICK);
        self.cursor = (start + take) % rows.len();
        for i in 0..take {
            let row = rows[(start + i) % rows.len()].clone();
            report.read += 1;
            match self.check_row(&row, &mut report).await {
                Ok(()) => self.streak = 0,
                Err(e) => match classify(&e) {
                    Some(hit) => {
                        self.streak += 1;
                        let wait = backoff_secs(hit, now, self.interval_s, self.streak);
                        self.paused_until = now.saturating_add(wait);
                        warn!("pr watch: gh rate limit; pausing {wait}s: {e:#}");
                        report.rate_limited = true;
                        return report;
                    }
                    None => {
                        report.failed += 1;
                        warn!("pr watch: {}#{}: read failed: {e:#}", row.repo, row.pr);
                    }
                },
            }
        }
        report
    }

    /// Read one row's PR and deliver any transition. `Err` is a read error.
    async fn check_row(&self, row: &WatchRow, report: &mut TickReport) -> anyhow::Result<()> {
        let reader = Arc::clone(&self.ports.reader);
        let (repo, pr) = (row.repo.clone(), row.pr);
        let snap = blocking(move || reader.view(&repo, pr)).await?;
        if snap.is_done() {
            let table = self.table.clone();
            let (repo, pr) = (row.repo.clone(), row.pr);
            match blocking(move || table.remove(&repo, pr)).await {
                Ok(_) => {
                    info!(
                        "pr watch: {}#{} is {}; row removed",
                        row.repo, row.pr, snap.state
                    );
                    report.removed += 1;
                }
                Err(e) => warn!("pr watch: cannot remove {}#{}: {e:#}", row.repo, row.pr),
            }
            return Ok(());
        }
        let new_head = !snap.head.eq_ignore_ascii_case(&row.head);
        let required = if new_head {
            // #11673: the required contexts are re-read on each new head.
            let reader = Arc::clone(&self.ports.reader);
            let (repo, base) = (row.repo.clone(), snap.base.clone());
            blocking(move || reader.required(&repo, &base))
                .await?
                .unwrap_or_default()
        } else {
            row.required.clone()
        };
        let verdict = judge(&snap.rollup, &required);
        let Some(kind) = detect(&row.head, row.verdict, &snap.head, verdict) else {
            if row.verdict != Some(verdict) {
                self.record(row, move |r| r.verdict = Some(verdict)).await;
            }
            return Ok(());
        };
        let t = Transition {
            kind,
            key: dedupe_key(&row.repo, row.pr, &snap.head, kind),
            head: snap.head,
            verdict,
            required,
        };
        if self.deliver(row, &t).await {
            report.woken += 1;
        } else {
            report.failed += 1;
        }
        Ok(())
    }

    /// Deliver `t` in its three steps; `true` once the wake is recorded.
    ///
    /// Test: `a_failed_wake_retries_next_tick_and_never_re_emits`,
    /// `a_failed_emit_advances_nothing`.
    async fn deliver(&self, row: &WatchRow, t: &Transition) -> bool {
        let key = Some(t.key.clone());
        if row.emitted_key != key {
            if let Err(e) = self.ports.emitter.emit(transition_event(row, t)).await {
                warn!(
                    "pr watch: {}: emit failed; retrying next tick: {e:#}",
                    t.key
                );
                return false;
            }
            let k = key.clone();
            if !self.record(row, move |r| r.emitted_key = k).await {
                return false;
            }
        }
        if row.ledger_key != key {
            if let Err(e) = self.ports.ledger.set_ci_wait(row, t).await {
                warn!(
                    "pr watch: {}: ledger update failed; retrying next tick: {e:#}",
                    t.key
                );
                return false;
            }
            let k = key.clone();
            if !self.record(row, move |r| r.ledger_key = k).await {
                return false;
            }
        }
        if row.last_woken_key != key
            && let Err(e) = self.ports.waker.wake(&row.owner, &wake_line(row, t)).await
        {
            warn!(
                "pr watch: {}: wake refused; retrying next tick: {e:#}",
                t.key
            );
            return false;
        }
        let t = t.clone();
        self.record(row, move |r| {
            r.last_woken_key = Some(t.key);
            r.head = t.head;
            r.required = t.required;
            // #11673: a new head starts unjudged, so its first verdict is a transition.
            r.verdict = (t.kind != Kind::NewHead).then_some(t.verdict);
        })
        .await
    }

    /// Apply `change` to the row on disk; `false` when it failed or is gone.
    async fn record(
        &self,
        row: &WatchRow,
        change: impl FnOnce(&mut WatchRow) + Send + 'static,
    ) -> bool {
        let table = self.table.clone();
        let (repo, pr) = (row.repo.clone(), row.pr);
        match blocking(move || table.update(&repo, pr, change)).await {
            Ok(found) => found,
            Err(e) => {
                warn!("pr watch: cannot update {}#{}: {e:#}", row.repo, row.pr);
                false
            }
        }
    }

    /// Run until `cancel` fires; the first tick is immediate.
    pub async fn run_until(mut self, cancel: tokio_util::sync::CancellationToken) {
        info!(
            interval_secs = self.interval_s,
            "pr watch: daemon loop starting (#11673)"
        );
        let mut timer = tokio::time::interval(Duration::from_secs(self.interval_s));
        timer.set_missed_tick_behavior(tokio::time::MissedTickBehavior::Delay);
        loop {
            tokio::select! {
                biased;
                () = cancel.cancelled() => return,
                _ = timer.tick() => {
                    let now = chrono::Utc::now().timestamp().max(0).unsigned_abs();
                    self.tick(now).await;
                }
            }
        }
    }
}

/// Run blocking `f` on the blocking pool; a panic is an error.
async fn blocking<T: Send + 'static>(
    f: impl FnOnce() -> anyhow::Result<T> + Send + 'static,
) -> anyhow::Result<T> {
    tokio::task::spawn_blocking(f)
        .await
        .map_err(|e| anyhow::anyhow!("blocking task failed: {e}"))?
}

/// The `Hook{kind:"ci_transition"}` event for `t` (AC3).
///
/// Test: `a_green_head_wakes_the_owner_once_across_ticks_and_restart`.
pub fn transition_event(row: &WatchRow, t: &Transition) -> HarnessEvent {
    HarnessEvent {
        source: HarnessSource::Mpm,
        session: None,
        seq: 0,
        at: chrono::Utc::now(),
        payload: HarnessPayload::Hook {
            kind: EVENT_KIND.to_string(),
            data: json!({
                "key": t.key, "repo": row.repo, "pr": row.pr, "head": t.head,
                "transition": t.kind.as_str(), "verdict": t.verdict.as_str(),
                "required": t.required, "owner": row.owner,
            }),
        },
        id: EventId::new(),
        parent_id: None,
        project: None,
    }
}

/// The one line typed into the owner's pane.
///
/// Test: `a_green_head_wakes_the_owner_once_across_ticks_and_restart`.
pub fn wake_line(row: &WatchRow, t: &Transition) -> String {
    let head: String = t.head.chars().take(10).collect();
    let what = match t.kind {
        Kind::Green => "required checks are green".to_string(),
        Kind::Red => "required checks are red".to_string(),
        Kind::NewHead => "new head pushed; CI restarted".to_string(),
        Kind::Rerun => "red checks are re-running".to_string(),
    };
    format!(
        "[tm pr watch] {}#{} @ {head}: {what}. Check it with `gh pr checks {} --repo {}`.",
        row.repo, row.pr, row.pr, row.repo
    )
}

/// Pure parse of [`ENV_ENABLED`]: on unless `0`/`false`/`off`/`no`.
pub fn parse_enabled(raw: Option<&str>) -> bool {
    crate::daemon::services::merged_pr_reclaim::parse_enabled(raw)
}

/// Pure parse of [`ENV_INTERVAL_SECS`]; junk or zero is the default.
///
/// Test: `parse_interval_defaults_on_junk_and_zero`.
pub fn parse_interval_secs(raw: Option<&str>) -> u64 {
    raw.and_then(|v| v.trim().parse::<u64>().ok())
        .filter(|n| *n > 0)
        .unwrap_or(DEFAULT_INTERVAL_SECS)
}

#[cfg(test)]
#[path = "pr_watch_loop_tests.rs"]
mod tests;
