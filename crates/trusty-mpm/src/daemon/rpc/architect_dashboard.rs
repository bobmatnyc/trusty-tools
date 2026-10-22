//! The Architect dashboard methods on the daemon socket:
//! `mpm.architect.dashboard.*` (#10063 PR 3, owner ruling D34).
//!
//! Why: the daemon hosts the dashboard store (owner ruling Dbc); the MCP
//! tool and `tm architect dashboard` run the same bodies (DOC-82 LDG-3.3).
//! What: [`register`] mounts each of
//! `trusty_architect::dashboard::rpc::METHODS`; a call decodes its params
//! itself (so an unknown key is a refusal with exit 2, not a bare
//! invalid-params frame) and runs [`crate::core::architect_dashboard::call`]
//! on the blocking pool (the store lock and file I/O block). An error frame
//! keeps the exit class: [`CODE_CONFLICT`] (exit 1), [`CODE_UNAVAILABLE`]
//! (exit 3), [`CODE_UNPROCESSABLE`] (exit 2), with `data.exit`.
//! Test: `architect_dashboard_methods_round_trip_over_the_daemon_socket`.

use serde_json::{Value, json};
use trusty_architect::dashboard::DashError;
use trusty_architect::dashboard::rpc::METHODS;
use trusty_architect::ledger::rpc::RpcErrorCode;
use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::core::architect_dashboard;
use crate::daemon::error::{CODE_CONFLICT, CODE_UNAVAILABLE, CODE_UNPROCESSABLE};

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

/// Mount every `mpm.architect.dashboard.*` method on `router`.
pub fn register(router: RpcRouter) -> RpcRouter {
    METHODS
        .into_iter()
        .fold(router, |router, method: &'static str| {
            let verb = method.rsplit('.').next().unwrap_or_default();
            router.typed(method, move |params: Value| async move {
                match tokio::task::spawn_blocking(move || architect_dashboard::call(verb, params))
                    .await
                {
                    Ok(result) => result.map_err(|e| to_rpc(&e)),
                    Err(e) => Err(RpcError::internal(format!("{method}: {e}"))),
                }
            })
        })
}
