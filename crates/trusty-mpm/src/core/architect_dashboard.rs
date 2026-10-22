//! The host half of the `mpm.architect.dashboard.*` bodies (#10063 PR 3).
//!
//! Why: the daemon method, the MCP tool and `tm architect dashboard` must
//! run the same `trusty_architect::dashboard::verbs` bodies with the same
//! secret scan and clock (DOC-82 LDG-3.3, LDG-2.3.5); this crate owns the
//! credential scanner, so it builds the [`Env`] once.
//! What: [`env`] is the system clock and [`scan_for_credentials`]; [`call`]
//! decodes a method's JSON params (unknown keys refused) and runs the verb
//! named by its `mpm.architect.dashboard.*` suffix, returning the reply as
//! JSON.
//! Test: `architect_dashboard_methods_round_trip_over_the_daemon_socket`,
//! `dispatch_architect_dashboard_tool_writes_and_conflicts`.
//!
//! # Spec References
//! - [`SPEC-LEDGER-03~draft`](docs/specs/DOC-82-portfolio-intent-ledger-dashboard.md#SPEC-LEDGER-03~draft)

use std::sync::Arc;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use trusty_architect::SystemClock;
use trusty_architect::dashboard::DashError;
use trusty_architect::dashboard::verbs::{self, Env};

use crate::core::session_ref_publish::scan_for_credentials;

/// The verbs' host facts: the system clock and this crate's credential scanner.
pub fn env() -> Env {
    Env {
        scan: scan_for_credentials,
        clock: Arc::new(SystemClock),
    }
}

fn run<Req, Resp>(
    params: Value,
    body: fn(&Env, &Req) -> Result<Resp, DashError>,
) -> Result<Value, DashError>
where
    Req: DeserializeOwned,
    Resp: Serialize,
{
    let req: Req = serde_json::from_value(params).map_err(|e| DashError::Invalid(e.to_string()))?;
    let resp = body(&env(), &req)?;
    serde_json::to_value(resp).map_err(|e| DashError::Invalid(format!("reply: {e}")))
}

/// Runs the verb `verb` (`get`, `list`, …) on `params`.
///
/// # Errors
/// [`DashError::Invalid`] for an unknown verb or params that do not decode;
/// the verb's own error.
pub fn call(verb: &str, params: Value) -> Result<Value, DashError> {
    match verb {
        "get" => run(params, verbs::get),
        "list" => run(params, verbs::list),
        "set" => run(params, verbs::set),
        "update" => run(params, verbs::update),
        "delete" => run(params, verbs::delete),
        "batch" => run(params, verbs::batch),
        "pruned" => run(params, verbs::pruned),
        "validate" => run(params, verbs::validate),
        "fsck" => run(params, verbs::fsck),
        "tally" => run(params, verbs::tally_verb),
        "footer" => run(params, verbs::footer_verb),
        "ids" => run(params, verbs::ids),
        other => Err(DashError::Invalid(format!(
            "unknown dashboard verb {other:?}: use get, list, set, update, delete, batch, \
             pruned, validate, fsck, tally, footer or ids"
        ))),
    }
}
