//! The PR watch loop's production ports and its spawn (#11673).
//!
//! Why: the loop's rules are tested against fakes; this file is the one place
//! they meet the bus, the ledger and the session manager.
//! What: [`BusEmitter`] buffers the event on the daemon's `ActionPublisher`,
//! whose flush delivers it and whose stop spools what the bus did not take
//! (#10412); with no publisher attached it writes the event straight to that
//! spool, which the next start replays. [`DbatgLedger`] replaces this PR's
//! `ci` row in the owner's `waiting_on` through the `mpm.dbatg.batch` body.
//! [`PaneWaker`] sends one line with `SessionManager::send_input`, whose pane
//! gate refuses a pane that is provisioning or shows a modal.
//! [`spawn_if_enabled`] wires them and starts the loop.
//! Test: `ledger_rows_replace_only_this_prs_ci_row`,
//! `a_publisherless_emit_lands_in_the_spool`.

use std::path::PathBuf;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;
use tracing::{info, warn};
use trusty_architect::ledger::model::{State, WaitingOn};
use trusty_architect::ledger::rpc::BatchRequest;
use trusty_architect::ledger::store::StorePaths;
use trusty_common::control_bus::HarnessEvent;

use super::pr_watch_loop::{self, Emitter, LedgerPort, Ports, PrWatchLoop, Transition, Waker};
use crate::core::pr_watch::gh::{GhPrReader, IdentityResolver};
use crate::core::pr_watch::table::{Table, WatchRow};
use crate::core::pr_watch::verdict::Kind;
use crate::daemon::DaemonState;
use crate::daemon::action_events::{ActionPublisher, spool};
use crate::session_manager::worktree_reclaim_gh::resolve_strict_gh_env_in;

/// Per-call `gh` budget for the loop's reads.
const GH_CALL_BUDGET: Duration = Duration::from_secs(30);

/// The most `waiting_on` rows the ledger accepts (LDG limits).
const WAITING_ON_MAX: usize = 5;

/// The daemon's bus producer, else the spool directory.
pub struct BusEmitter {
    /// The attached publisher, if the bus is allowed.
    pub publisher: Option<Arc<ActionPublisher>>,
    /// The action-event spool directory, if the data dir resolved.
    pub spool_dir: Option<PathBuf>,
}

#[async_trait::async_trait]
impl Emitter for BusEmitter {
    async fn emit(&self, event: HarnessEvent) -> anyhow::Result<()> {
        if let Some(publisher) = &self.publisher {
            publisher.publish_event(event);
            return Ok(());
        }
        let dir = self
            .spool_dir
            .clone()
            .ok_or_else(|| anyhow::anyhow!("no bus publisher and no spool directory"))?;
        let written = tokio::task::spawn_blocking(move || spool::persist(&dir, &[event]))
            .await
            .map_err(|e| anyhow::anyhow!("spool task failed: {e}"))??;
        anyhow::ensure!(written == 1, "spool wrote {written} events, wanted 1");
        Ok(())
    }
}

/// The owner's `waiting_on` with this PR's `ci` row set for `t`.
///
/// What: replaces the `ci` row whose `what` starts `<repo>#<pr> `, else
/// appends one. `None` when the list is full and holds no such row, so the
/// write is skipped rather than refused forever.
/// Test: `ledger_rows_replace_only_this_prs_ci_row`.
pub fn ci_rows(rows: &[WaitingOn], row: &WatchRow, t: &Transition) -> Option<Vec<WaitingOn>> {
    let prefix = format!("{}#{} ", row.repo, row.pr);
    let head: String = t.head.chars().take(10).collect();
    let verdict = match t.kind {
        Kind::Rerun => "pending (rerun)",
        _ => t.verdict.as_str(),
    };
    let mut what = format!("{prefix}CI {verdict} @{head}");
    what.truncate(120);
    let mine = |r: &WaitingOn| r.who == "ci" && r.what.starts_with(&prefix);
    let mut out: Vec<WaitingOn> = rows.to_vec();
    let fresh = WaitingOn {
        who: "ci".into(),
        what,
        since: String::new(),
    };
    match out.iter().position(mine) {
        Some(at) => out[at] = fresh,
        None if out.len() < WAITING_ON_MAX => out.push(fresh),
        None => return None,
    }
    Some(out)
}

/// The ledger port over `core::dbatg::batch`.
pub struct DbatgLedger;

#[async_trait::async_trait]
impl LedgerPort for DbatgLedger {
    async fn set_ci_wait(&self, row: &WatchRow, t: &Transition) -> anyhow::Result<()> {
        let (row, t) = (row.clone(), t.clone());
        tokio::task::spawn_blocking(move || write_ci_wait(&row, &t))
            .await
            .map_err(|e| anyhow::anyhow!("ledger task failed: {e}"))?
    }
}

/// [`DbatgLedger`]'s blocking body. A row with no project, or an owner with
/// no store, is skipped with `Ok`: the PM keeps no ledger to update.
fn write_ci_wait(row: &WatchRow, t: &Transition) -> anyhow::Result<()> {
    let Some(project) = &row.project else {
        return Ok(());
    };
    let paths = StorePaths::new(project, &row.owner)?;
    let text = match std::fs::read_to_string(&paths.state) {
        Ok(text) => text,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(()),
        Err(e) => return Err(e.into()),
    };
    let state = State::from_json(&text)?;
    let rows = state.session.and_then(|s| s.waiting_on).unwrap_or_default();
    let Some(new) = ci_rows(&rows, row, t) else {
        warn!(
            "pr watch: {}: {}'s waiting_on is full; ledger not updated",
            t.key, row.owner
        );
        return Ok(());
    };
    let new: Vec<_> = new
        .iter()
        .map(|r| {
            let mut v = json!({"who": r.who, "what": r.what});
            if !r.since.is_empty() {
                v["since"] = json!(r.since);
            }
            v
        })
        .collect();
    use sha2::Digest as _;
    let digest = sha2::Sha256::digest(t.key.as_bytes());
    let id: String = digest.iter().take(16).map(|b| format!("{b:02x}")).collect();
    let req: BatchRequest = serde_json::from_value(json!({
        "store": {"project": project, "session": row.owner},
        "request_id": format!("pr-watch-{id}"),
        "if_v": state.v,
        "ops": [{"op": "session", "waiting_on": new}],
    }))?;
    crate::core::dbatg::batch(&req)?;
    Ok(())
}

/// The pane port over the daemon's session manager.
pub struct PaneWaker {
    /// The daemon state the manager comes from.
    pub state: Arc<DaemonState>,
}

#[async_trait::async_trait]
impl Waker for PaneWaker {
    async fn wake(&self, owner: &str, line: &str) -> anyhow::Result<()> {
        let mgr = self.state.session_manager().await;
        let record = mgr
            .list()
            .await
            .into_iter()
            .find(|r| r.tmux_name == owner)
            .ok_or_else(|| anyhow::anyhow!("no managed session named {owner}"))?;
        mgr.send_input(&record.id, line).await?;
        Ok(())
    }
}

/// The daemon's PR reader: each call's identity resolved as worktree reclaim
/// resolves it, against `registry_dir` (#11673).
///
/// Why: launchd hands the daemon neither `GH_TOKEN` nor `GH_CONFIG_DIR`; the
/// overlay `resolve_gh_env(None)` gave was empty, so every read exited 4.
/// What: [`resolve_strict_gh_env_in`] per call — the registry pin, then the
/// static `github:` binding for the repo — with no ambient fallback on a
/// refusal.
/// Test: `a_pinned_repo_reads_through_its_bound_identity`,
/// `a_registry_refusal_spawns_no_gh`.
pub(crate) fn daemon_reader(registry_dir: PathBuf, budget: Duration) -> GhPrReader {
    let identity: IdentityResolver = Arc::new(move |repo: &str| {
        resolve_strict_gh_env_in(repo, &registry_dir).map_err(|f| f.to_string())
    });
    GhPrReader::new(identity, budget)
}

/// Start the loop unless [`pr_watch_loop::ENV_ENABLED`] turns it off.
///
/// What: the table under the data dir, [`daemon_reader`] over the project
/// registry, the daemon's publisher and spool.
pub(crate) fn spawn_if_enabled(
    state: Arc<DaemonState>,
    cancel: tokio_util::sync::CancellationToken,
) {
    let raw = std::env::var(pr_watch_loop::ENV_ENABLED).ok();
    if !pr_watch_loop::parse_enabled(raw.as_deref()) {
        info!("pr watch disabled via {}", pr_watch_loop::ENV_ENABLED);
        return;
    }
    let data_dir = match trusty_common::resolve_data_dir("trusty-mpm") {
        Ok(d) => d,
        Err(e) => {
            warn!("pr watch: no data dir; loop not started: {e:#}");
            return;
        }
    };
    let ports = Ports {
        // #11673: per-repo identity, resolved per call; was `resolve_gh_env(None)`.
        reader: Arc::new(daemon_reader(
            crate::project::registry_data_dir(),
            GH_CALL_BUDGET,
        )),
        emitter: Arc::new(BusEmitter {
            publisher: state.action_events().publisher().cloned(),
            spool_dir: Some(spool::spool_dir(&data_dir)),
        }),
        ledger: Arc::new(DbatgLedger),
        waker: Arc::new(PaneWaker { state }),
    };
    let interval = pr_watch_loop::parse_interval_secs(
        std::env::var(pr_watch_loop::ENV_INTERVAL_SECS)
            .ok()
            .as_deref(),
    );
    let driver = PrWatchLoop::new(Table::at(data_dir), ports, interval);
    tokio::spawn(driver.run_until(cancel));
}

#[cfg(test)]
#[path = "pr_watch_identity_tests.rs"]
mod identity_tests;
