//! The PM ledger methods on the daemon socket: `mpm.dbatg.init`, `batch`
//! and `show` (#9758).
//!
//! Why: the daemon is the preferred ledger writer (#9756 plan section 2);
//! `tm dbatg` and the `dbatg` MCP tool reach the same bodies.
//! What: each method runs its [`crate::core::dbatg`] body on the blocking
//! pool (the store lock and file I/O block). A ledger error answers an error
//! frame whose code keeps the prototype's exit class — [`CODE_CONFLICT`]
//! (exit 1), [`CODE_UNAVAILABLE`] (exit 3), [`CODE_UNPROCESSABLE`] (exit 2) —
//! and whose `data.exit` carries the exit code.
//! Test: `dbatg_methods_round_trip_over_the_daemon_socket`.

use serde::Serialize;
use serde::de::DeserializeOwned;
use trusty_architect::ledger::LedgerError;
use trusty_architect::ledger::rpc::{BatchRequest, InitRequest, RpcErrorCode, ShowRequest};
use trusty_common::uds::server::{RpcError, RpcRouter};

use crate::core::dbatg;
use crate::daemon::error::{CODE_CONFLICT, CODE_UNAVAILABLE, CODE_UNPROCESSABLE};

#[cfg(test)]
#[path = "dbatg_tests.rs"]
mod tests;

/// The error frame for a ledger error.
pub fn to_rpc(e: &LedgerError) -> RpcError {
    let class = e.code();
    let code = match class {
        RpcErrorCode::Conflict => CODE_CONFLICT,
        RpcErrorCode::Unavailable => CODE_UNAVAILABLE,
        RpcErrorCode::Invalid => CODE_UNPROCESSABLE,
    };
    RpcError::new(code, e.to_string()).with_data(serde_json::json!({ "exit": class.exit_code() }))
}

/// Mount `mpm.dbatg.init`, `batch` and `show` on `router`.
///
/// Test: `dbatg_methods_round_trip_over_the_daemon_socket`.
pub fn register(router: RpcRouter) -> RpcRouter {
    let router = mount(router, "mpm.dbatg.init", |r: InitRequest| dbatg::init(&r));
    let router = mount(router, "mpm.dbatg.batch", |r: BatchRequest| {
        dbatg::batch(&r)
    });
    mount(router, "mpm.dbatg.show", |r: ShowRequest| dbatg::show(&r))
}

fn mount<Req, Resp>(
    router: RpcRouter,
    method: &'static str,
    body: fn(Req) -> Result<Resp, LedgerError>,
) -> RpcRouter
where
    Req: DeserializeOwned + Send + 'static,
    Resp: Serialize + Send + 'static,
{
    router.typed(method, move |req: Req| async move {
        match tokio::task::spawn_blocking(move || body(req)).await {
            Ok(result) => result.map_err(|e| to_rpc(&e)),
            Err(e) => Err(RpcError::internal(format!("{method}: {e}"))),
        }
    })
}
