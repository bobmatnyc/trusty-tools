//! The managed-session lifecycle routes as JSON-RPC methods (#6288 slice 4).
//!
//! Why: `/api/v1/sessions/managed*` is what `tm launch`, the picker, the TUI,
//! Telegram, and Slack all drive. ADR-0032 moves its transport onto the socket;
//! nothing here re-implements a route — every method calls the same `*_core`
//! body the axum handler calls.
//!
//! What: what an HTTP route reads from a path segment or a query string becomes
//! a field of the request object, since a socket request has no URL.
//!
//! | Method | Route |
//! |---|---|
//! | `mpm.managed.spawn` | `POST /api/v1/sessions/managed` |
//! | `mpm.managed.list` | `GET /api/v1/sessions/managed` |
//! | `mpm.managed.adopt` | `POST /api/v1/sessions/managed/adopt` |
//! | `mpm.managed.prune` | `POST /api/v1/sessions/managed/prune` |
//! | `mpm.managed.decommission_ephemeral` | `POST /api/v1/sessions/managed/decommission-ephemeral` |
//! | `mpm.managed.prune_worktrees` | `POST /api/v1/sessions/managed/prune-worktrees` |
//! | `mpm.managed.reconcile_worktrees` | `GET /api/v1/sessions/managed/reconcile-worktrees` |
//! | `mpm.managed.fleet` | `GET /api/v1/sessions/managed/fleet` |
//! | `mpm.managed.get` | `GET /api/v1/sessions/managed/{id}` |
//! | `mpm.managed.stop` | `DELETE /api/v1/sessions/managed/{id}` |
//! | `mpm.managed.rename` | `PATCH /api/v1/sessions/managed/{id}` |
//! | `mpm.managed.send` | `POST /api/v1/sessions/managed/{id}/send` |
//! | `mpm.managed.provision_status` | `GET /api/v1/sessions/managed/{id}/provision-status` |
//! | `mpm.managed.sync_assets` | `POST /api/v1/sessions/managed/{id}/sync-assets` |
//! | `mpm.managed.sync_assets_all` | `POST /api/v1/sessions/managed/sync-assets` |
//! | `mpm.managed.answer` | `POST /api/v1/sessions/managed/{id}/answer` |
//! | `mpm.managed.attach_cmd` | `GET /api/v1/sessions/managed/{id}/attach-cmd` |
//! | `mpm.managed.activity` | `GET /api/v1/sessions/managed/{id}/activity` |
//! | `mpm.managed.runtime_stop` | `POST /api/v1/sessions/managed/{id}/runtime-stop` |
//! | `mpm.managed.resume` | `POST /api/v1/sessions/managed/{id}/resume` |
//! | `mpm.managed.reactivate` | `POST /api/v1/sessions/managed/{id}/reactivate` |
//! | `mpm.managed.decommission` | `POST /api/v1/sessions/managed/{id}/decommission` |
//! | `mpm.managed.delete` | `POST /api/v1/sessions/managed/{id}/delete` |
//!
//! `mpm.managed.stop` and `mpm.managed.runtime_stop` name the same body, because
//! the `DELETE` route is a legacy alias that delegates to runtime-stop. Both
//! names are registered so a caller porting either HTTP route finds its verb.
//!
//! Test: `managed_*_parity` in `daemon::rpc::managed_tests`.

use std::sync::Arc;

use serde::Deserialize;
use trusty_common::uds::server::RpcRouter;

use crate::daemon::managed_routes::prune::{PruneRequest, PruneWorktreesRequest};
use crate::daemon::managed_routes::{
    AdoptExistingRequest, AnswerRequest, ReactivateQuery, RenameRequest, SendInputRequest,
    SpawnRequest, activity, cores, delete, fleet, provision_status, prune, reactivate, reconcile,
    rename, sync_assets,
};
use crate::daemon::state::DaemonState;

/// A request naming a managed session by id, with nothing else.
#[derive(Debug, Deserialize)]
pub struct SessionParams {
    /// The managed-session id the HTTP route reads from its path.
    pub id: String,
}

/// `mpm.managed.list` parameters — the HTTP route's two query flags.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct ListParams {
    /// Return only rows whose last-known `source_id` matches exactly.
    pub source_id: Option<String>,
    /// Skip the per-session `stale_assets` probe (#4335). Only a caller that
    /// never reads that flag may pass it — every `stale_assets` then reports
    /// its `false` default, meaning "not computed".
    pub slim: bool,
}

/// `mpm.managed.send` parameters: the id plus the HTTP body.
#[derive(Debug, Deserialize)]
pub struct SendParams {
    /// The managed-session id.
    pub id: String,
    /// The text to inject.
    pub text: String,
}

/// `mpm.managed.answer` parameters: the id plus the HTTP body.
#[derive(Debug, Deserialize)]
pub struct AnswerParams {
    /// The managed-session id.
    pub id: String,
    /// The answer to the pending decision.
    pub answer: String,
}

/// `mpm.managed.rename` parameters: the id plus the HTTP body.
#[derive(Debug, Deserialize)]
pub struct RenameParams {
    /// The managed-session id.
    pub id: String,
    /// The new session name.
    pub name: String,
}

/// `mpm.managed.decommission` parameters: the id plus `?record_only=`.
#[derive(Debug, Deserialize)]
pub struct DecommissionParams {
    /// The managed-session id.
    pub id: String,
    /// Tear down the RECORD only, never the filesystem. Absent means false, as
    /// an absent query parameter does.
    #[serde(default)]
    pub record_only: bool,
}

/// `mpm.managed.delete` parameters: the id plus `?force=`.
#[derive(Debug, Deserialize)]
pub struct DeleteParams {
    /// The managed-session id.
    pub id: String,
    /// Bypass the running-session guard. Absent means false.
    #[serde(default)]
    pub force: bool,
}

/// `mpm.managed.decommission_ephemeral` parameters: `?dry_run=`.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct EphemeralParams {
    /// Report what would be torn down without tearing it down.
    pub dry_run: bool,
}

/// `mpm.managed.reactivate` parameters: the id plus the two query flags.
#[derive(Debug, Deserialize)]
pub struct ReactivateParams {
    /// The managed-session id.
    pub id: String,
    /// The caller's own pane, excluded from the liveness probe (#2789).
    #[serde(default)]
    pub caller_pane_id: Option<String>,
    /// The caller's self-asserted claim that it watched the agent exit (#2453).
    #[serde(default)]
    pub pane_confirmed_dead: bool,
}

/// A request carrying nothing — the socket form of a GET with no path or query.
#[derive(Debug, Deserialize, Default)]
#[serde(default)]
pub struct NoParams {}

/// Register every managed-session method on `router`.
///
/// Why one function rather than several: the SLOC cost is in the closures, not
/// the structure, and splitting it would separate a method name from the body
/// it names without making either easier to read.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let router = register_fleet_wide(router, state);
    register_per_session(router, state)
}

/// The methods that address the fleet rather than one session.
fn register_fleet_wide(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    macro_rules! st {
        () => {
            Arc::clone(state)
        };
    }
    let (spawn_s, list_s, adopt_s, prune_s, eph_s) = (st!(), st!(), st!(), st!(), st!());
    let (pw_s, rw_s, fleet_s, sa_s) = (st!(), st!(), st!(), st!());

    router
        .typed("mpm.managed.spawn", move |req: SpawnRequest| {
            let state = Arc::clone(&spawn_s);
            async move { cores::spawn_core(&state, req).await.into_rpc() }
        })
        .typed("mpm.managed.list", move |req: ListParams| {
            let state = Arc::clone(&list_s);
            async move {
                cores::list_core(&state, req.source_id.as_deref(), req.slim)
                    .await
                    .into_rpc()
            }
        })
        .typed("mpm.managed.adopt", move |req: AdoptExistingRequest| {
            let state = Arc::clone(&adopt_s);
            async move { cores::adopt_core(&state, req).await.into_rpc() }
        })
        .typed("mpm.managed.prune", move |req: PruneRequest| {
            let state = Arc::clone(&prune_s);
            async move { prune::prune_managed_core(&state, req).await.into_rpc() }
        })
        .typed(
            "mpm.managed.decommission_ephemeral",
            move |req: EphemeralParams| {
                let state = Arc::clone(&eph_s);
                async move {
                    prune::decommission_ephemeral_core(&state, req.dry_run)
                        .await
                        .into_rpc()
                }
            },
        )
        .typed(
            "mpm.managed.prune_worktrees",
            move |req: PruneWorktreesRequest| {
                let state = Arc::clone(&pw_s);
                async move { prune::prune_worktrees_core(&state, req).await.into_rpc() }
            },
        )
        .typed("mpm.managed.reconcile_worktrees", move |_: NoParams| {
            let state = Arc::clone(&rw_s);
            async move { reconcile::reconcile_worktrees_core(&state).await.into_rpc() }
        })
        .typed("mpm.managed.fleet", move |_: NoParams| {
            let state = Arc::clone(&fleet_s);
            async move { fleet::fleet_core(&state).await.into_rpc() }
        })
        .typed("mpm.managed.sync_assets_all", move |_: NoParams| {
            let state = Arc::clone(&sa_s);
            async move {
                sync_assets::sync_all_session_assets_core(&state)
                    .await
                    .into_rpc()
            }
        })
}

/// The methods that address one session by id.
fn register_per_session(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    macro_rules! st {
        () => {
            Arc::clone(state)
        };
    }
    let (get_s, stop_s, rstop_s, rename_s, send_s) = (st!(), st!(), st!(), st!(), st!());
    let (ps_s, sa_s, ans_s, ac_s, act_s) = (st!(), st!(), st!(), st!(), st!());
    let (res_s, react_s, dec_s, del_s) = (st!(), st!(), st!(), st!());

    router
        .typed("mpm.managed.get", move |req: SessionParams| {
            let state = Arc::clone(&get_s);
            async move { cores::get_core(&state, &req.id).await.into_rpc() }
        })
        // The DELETE alias and runtime-stop are ONE body; both names resolve to
        // it so a caller porting either HTTP route finds its verb.
        .typed("mpm.managed.stop", move |req: SessionParams| {
            let state = Arc::clone(&stop_s);
            async move { cores::runtime_stop_core(&state, &req.id).await.into_rpc() }
        })
        .typed("mpm.managed.runtime_stop", move |req: SessionParams| {
            let state = Arc::clone(&rstop_s);
            async move { cores::runtime_stop_core(&state, &req.id).await.into_rpc() }
        })
        .typed("mpm.managed.rename", move |req: RenameParams| {
            let state = Arc::clone(&rename_s);
            async move {
                rename::rename_core(&state, &req.id, RenameRequest { name: req.name })
                    .await
                    .into_rpc()
            }
        })
        .typed("mpm.managed.send", move |req: SendParams| {
            let state = Arc::clone(&send_s);
            async move {
                cores::send_core(&state, &req.id, SendInputRequest { text: req.text })
                    .await
                    .into_rpc()
            }
        })
        .typed("mpm.managed.provision_status", move |req: SessionParams| {
            let state = Arc::clone(&ps_s);
            async move {
                provision_status::provision_status_core(&state, &req.id)
                    .await
                    .into_rpc()
            }
        })
        .typed("mpm.managed.sync_assets", move |req: SessionParams| {
            let state = Arc::clone(&sa_s);
            async move {
                sync_assets::sync_session_assets_core(&state, &req.id)
                    .await
                    .into_rpc()
            }
        })
        .typed("mpm.managed.answer", move |req: AnswerParams| {
            let state = Arc::clone(&ans_s);
            async move {
                cores::answer_core(&state, &req.id, AnswerRequest { answer: req.answer })
                    .await
                    .into_rpc()
            }
        })
        .typed("mpm.managed.attach_cmd", move |req: SessionParams| {
            let state = Arc::clone(&ac_s);
            async move { cores::attach_cmd_core(&state, &req.id).await.into_rpc() }
        })
        .typed("mpm.managed.activity", move |req: SessionParams| {
            let state = Arc::clone(&act_s);
            async move { activity::activity_core(&state, &req.id).await.into_rpc() }
        })
        .typed("mpm.managed.resume", move |req: SessionParams| {
            let state = Arc::clone(&res_s);
            async move { cores::resume_core(&state, &req.id).await.into_rpc() }
        })
        .typed("mpm.managed.reactivate", move |req: ReactivateParams| {
            let state = Arc::clone(&react_s);
            async move {
                let query = ReactivateQuery {
                    caller_pane_id: req.caller_pane_id,
                    pane_confirmed_dead: req.pane_confirmed_dead,
                };
                let outcome = reactivate::reactivate_core(&state, &req.id, &query).await;
                outcome.into_rpc()
            }
        })
        .typed(
            "mpm.managed.decommission",
            move |req: DecommissionParams| {
                let state = Arc::clone(&dec_s);
                async move {
                    cores::decommission_core(&state, &req.id, req.record_only)
                        .await
                        .into_rpc()
                }
            },
        )
        .typed("mpm.managed.delete", move |req: DeleteParams| {
            let state = Arc::clone(&del_s);
            async move {
                delete::delete_core(&state, &req.id, req.force)
                    .await
                    .into_rpc()
            }
        })
}
