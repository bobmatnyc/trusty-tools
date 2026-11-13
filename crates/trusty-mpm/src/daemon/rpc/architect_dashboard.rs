//! The Architect dashboard methods on the daemon socket:
//! `mpm.architect.dashboard.*` (#10063 PR 3, owner ruling D34).
//!
//! Why: the daemon hosts the dashboard store (owner ruling Dbc); the MCP
//! tool and `tm architect dashboard` run the same bodies (DOC-82 LDG-3.3).
//! What: [`register`](crate::daemon::rpc::architect_dashboard::register) mounts each of
//! `trusty_architect::dashboard::rpc::METHODS`; a call decodes its params
//! itself (so an unknown key is a refusal with exit 2, not a bare
//! invalid-params frame) and runs [`crate::core::architect_dashboard::call`]
//! on the blocking pool (the store lock and file I/O block). An error frame
//! keeps the exit class: [`CODE_CONFLICT`](crate::daemon::error::CODE_CONFLICT) (exit 1), [`CODE_UNAVAILABLE`](crate::daemon::error::CODE_UNAVAILABLE)
//! (exit 3), [`CODE_UNPROCESSABLE`](crate::daemon::error::CODE_UNPROCESSABLE) (exit 2), with `data.exit`.
//! A call with no `store`, or a `store` with no `root`, gets the root
//! [`crate::core::architect_store::resolve_store_root`] picks (#10944); one
//! that names `store.root` is passed on unchanged.
//! Test: `architect_dashboard_methods_round_trip_over_the_daemon_socket`,
//! `omitted_store_lists_and_tallies_the_resolved_store`.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use serde_json::{Value, json};
use trusty_architect::dashboard::DashError;
use trusty_architect::dashboard::rpc::METHODS;
use trusty_architect::ledger::rpc::RpcErrorCode;
use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::core::architect_dashboard;
use crate::core::architect_store::{self, ResolvedRoot};
use crate::daemon::error::{CODE_CONFLICT, CODE_UNAVAILABLE, CODE_UNPROCESSABLE};
use crate::daemon::state::DaemonState;

#[cfg(test)]
#[path = "architect_dashboard_tests.rs"]
mod tests;

/// The error frame for a dashboard error.
pub fn to_rpc(e: &DashError) -> RpcError {
    let class = e.code();
    let code = match class {
        RpcErrorCode::Conflict => CODE_CONFLICT,
        RpcErrorCode::Unavailable => CODE_UNAVAILABLE,
        RpcErrorCode::Invalid => CODE_UNPROCESSABLE,
    };
    RpcError::new(code, e.to_string()).with_data(json!({ "exit": class.exit_code() }))
}

/// The verbs that write the store; a resolved root on one is logged at `info`.
const WRITE_VERBS: [&str; 4] = ["set", "update", "delete", "batch"];

/// The daemon's env lookup for the store-root overrides.
#[cfg(not(test))]
fn process_env(name: &str) -> Option<String> {
    std::env::var(name).ok()
}

/// Test-binary arm: the developer's shell (an Architect session sets
/// `ARCHITECT_PROJECT_DIR`) must not reach a socket test; the env order is
/// covered with an injected lookup in `core/architect_store_tests.rs`.
#[cfg(test)]
fn process_env(_: &str) -> Option<String> {
    None
}

/// The home the daemon's framework root sits in (`<home>/.trusty-mpm`), so a
/// daemon over a temp framework root resolves under that temp home.
fn home_of(framework_root: &Path) -> Option<PathBuf> {
    framework_root.parent().map(Path::to_path_buf)
}

/// `params` with the resolved store root filled in when the caller gave none.
fn resolve_params(verb: &str, params: Value, home: Option<&Path>) -> Result<Value, DashError> {
    let resolve = || architect_store::resolve_store_root(&process_env, home);
    let (params, resolved) = architect_store::normalize_params(params, resolve)?;
    if let Some(ResolvedRoot { path, source }) = resolved {
        // #10944 Q1: a write to a store the caller did not name shows in the log.
        if WRITE_VERBS.contains(&verb) {
            tracing::info!(verb, root = %path.display(), %source, "dashboard store root resolved");
        } else {
            tracing::debug!(verb, root = %path.display(), %source, "dashboard store root resolved");
        }
    }
    Ok(params)
}

/// Mount every `mpm.architect.dashboard.*` method on `router`.
///
/// Why: #10944 — a socket client such as `tm tui` cannot know the store root,
/// so the daemon resolves it (Architect ruling Q2).
/// What: each handler fills an omitted `store.root` from the home above
/// `state`'s framework root ([`resolve_params`]), then runs the verb on the
/// blocking pool; a resolution failure is an exit-3 frame naming the path,
/// its source and the fix.
/// Test: `omitted_store_lists_and_tallies_the_resolved_store`,
/// `unresolvable_root_returns_an_error_frame_with_exit_3`.
pub fn register(router: RpcRouter, state: &Arc<DaemonState>) -> RpcRouter {
    let home: Option<Arc<Path>> = home_of(state.framework_root()).map(Arc::from);
    METHODS
        .into_iter()
        .fold(router, |router, method: &'static str| {
            let verb = method.rsplit('.').next().unwrap_or_default();
            let home = home.clone();
            router.typed(method, move |params: Value| {
                let home = home.clone();
                async move {
                    let run = move || {
                        let params = resolve_params(verb, params, home.as_deref())?;
                        architect_dashboard::call(verb, params)
                    };
                    match tokio::task::spawn_blocking(run).await {
                        Ok(result) => result.map_err(|e| to_rpc(&e)),
                        Err(e) => Err(RpcError::internal(format!("{method}: {e}"))),
                    }
                }
            })
        })
}
