//! `mpm.managed.prune_worktrees_stream`: the prune-worktrees pass as a frame
//! stream (#7619 PR 2).
//!
//! Why: the unary method answers once, minutes later, and its socket dispatch
//! never notices a departed client. So `tm session prune-worktrees` printed
//! nothing about which worktree the pass was on, and a killed CLI left the pass
//! deleting to the end of its list.
//! What: [`open_prune_stream`] runs the same admission and the same pass as
//! the unary method, and streams four item kinds:
//!
//! | `kind` | Carries |
//! |---|---|
//! | `progress` | `line`: one phase or candidate, e.g. `orphan sweep: removing 2/5 <path>` |
//! | `keepalive` | `elapsed_s`, every [`KEEPALIVE`] while the pass runs |
//! | `detached` | first item of a `detach: true` request: the pass ignores a departure |
//! | `result` | `body`: the unary method's reply, verbatim, as the last item |
//!
//! A refusal (bad caller id, bad scope, busy lane) and a failed pass are the
//! stream's terminal error frame, with the unary method's code and message.
//! Cancel: `trusty_common`'s stream writer drops the item receiver when the
//! peer departs, so a closed channel IS the cancel signal. Both loops read it
//! before each candidate; a removal in flight finishes. `detach: true` opts out.
//! Test: `worktree_7619_the_stream_reports_every_worktree_before_its_result`,
//! `worktree_7619_a_closed_stream_before_the_pass_removes_nothing`,
//! `worktree_7619_a_dropped_stream_stops_removing_and_frees_the_lane`,
//! `worktree_7619_a_detached_stream_runs_the_pass_to_the_end`,
//! `worktree_7619_a_refused_stream_request_is_one_terminal_error`,
//! `worktree_7619_the_stream_beats_while_the_pass_is_quiet`.

use std::sync::Arc;
use std::time::Duration;

use serde_json::{Value, json};
use tokio::sync::mpsc;
use trusty_common::uds::server::{RpcError, RpcStreamItems};

use super::prune::{PruneWorktreesRequest, admit_prune, prune_worktrees_pass};
use crate::daemon::rpc::managed::outcome::RouteOutcome;
use crate::daemon::state::DaemonState;
use crate::session_manager::prune_progress::PruneCancel;

/// The streaming method's name; the CLI falls back to the unary one when a
/// daemon does not serve it.
pub const PRUNE_STREAM_METHOD: &str = "mpm.managed.prune_worktrees_stream";

/// How often a running pass proves it is alive (#7619, Architect ruling Q4).
///
/// One slow `gh` call can take 90 s, which under the client's 120 s idle
/// budget would read as a stall without a beat in between.
pub const KEEPALIVE: Duration = Duration::from_secs(15);

/// Items buffered toward the socket writer. Progress beyond this queues in
/// the unbounded feed instead of blocking the loops.
const ITEM_CHANNEL: usize = 64;

/// Open the stream for one request, on the daemon's maintenance lane.
pub(crate) async fn open_prune_stream(
    state: Arc<DaemonState>,
    req: PruneWorktreesRequest,
) -> Result<RpcStreamItems, RpcError> {
    let lane = crate::daemon::services::sweep_status::lane();
    open_prune_stream_in(state, req, lane, KEEPALIVE)
}

/// [`open_prune_stream`] with the lane and the keepalive period as parameters.
///
/// Why: a test holds its own lane and cannot wait 15 s for a beat.
/// What: admission runs first, so a refusal opens no pass. Then three tasks:
/// the pass (owning the permit, #8994), a beat loop that ends with the pass,
/// and a forwarder from the unbounded feed into the bounded item channel. The
/// result is the feed's last item, so it follows every progress line.
/// Test: see the module header.
pub(crate) fn open_prune_stream_in(
    state: Arc<DaemonState>,
    req: PruneWorktreesRequest,
    lane: &'static tokio::sync::Semaphore,
    keepalive: Duration,
) -> Result<RpcStreamItems, RpcError> {
    let (scope, permit) = admit_prune(&req, lane).map_err(refusal)?;
    let (items_tx, items) = mpsc::channel(ITEM_CHANNEL);
    let (feed, mut queue) = mpsc::unbounded_channel::<Result<Value, RpcError>>();
    let sink = feed.clone();
    let cancel = stream_cancel(items_tx.clone(), req.detach).with_progress(move |line| {
        // A send fails only once the forwarder is gone, which the cancel
        // probe already reads as stop.
        let _ = sink.send(Ok(json!({"kind": "progress", "line": line})));
    });
    if req.detach {
        let _ = feed.send(Ok(json!({"kind": "detached"})));
    }
    tokio::spawn(async move {
        while let Some(item) = queue.recv().await {
            if items_tx.send(item).await.is_err() {
                // The peer left: nothing more can be delivered.
                break;
            }
        }
    });
    tokio::spawn(async move {
        let started = tokio::time::Instant::now();
        let mut pass = tokio::spawn(async move {
            let _permit = permit;
            prune_worktrees_pass(&state, req, scope, cancel).await
        });
        let mut beats = tokio::time::interval_at(started + keepalive, keepalive);
        let joined = loop {
            tokio::select! {
                joined = &mut pass => break joined,
                _ = beats.tick() => {
                    let elapsed_s = started.elapsed().as_secs();
                    let _ = feed.send(Ok(json!({"kind": "keepalive", "elapsed_s": elapsed_s})));
                }
            }
        };
        let terminal = match joined {
            Ok(outcome) => outcome
                .into_rpc()
                .map(|body| json!({"kind": "result", "body": body})),
            Err(e) => {
                tracing::warn!("prune-worktrees stream: the prune task failed: {e}");
                Err(RpcError::internal(format!("the prune task failed: {e}")))
            }
        };
        let _ = feed.send(terminal);
    });
    Ok(items)
}

/// The stop signal for a streamed pass (#7619).
///
/// Why: Fail-Open Check (plan test d1). The closed item channel must read as
/// "stop", never as "no signal, keep going".
/// What: attached, the pass stops once the receiver is gone, including when
/// it is gone before the first candidate. Detached, it never stops early.
/// Test: `worktree_7619_a_closed_stream_before_the_pass_removes_nothing`,
/// `worktree_7619_a_detached_stream_runs_the_pass_to_the_end`.
fn stream_cancel(items: mpsc::Sender<Result<Value, RpcError>>, detach: bool) -> PruneCancel {
    if detach {
        return PruneCancel::never();
    }
    PruneCancel::from_probe(move || Some(items.is_closed()))
}

/// The RPC error a pre-pass refusal becomes: the unary method's code and text.
fn refusal(outcome: RouteOutcome) -> RpcError {
    match outcome.into_rpc() {
        Err(error) => error,
        // `admit_prune` returns only refusals; a success here is a bug.
        Ok(body) => RpcError::internal(format!("prune admission refused with a success: {body}")),
    }
}

#[cfg(test)]
#[path = "prune_stream_tests.rs"]
mod prune_stream_tests;
