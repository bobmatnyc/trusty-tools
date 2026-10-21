//! `palace_migrate` — the daemon's half of `trusty-memory palace migrate` (#10094).
//!
//! Why: while the daemon runs it holds every palace it serves, so the CLI's
//! in-process path cannot open them; the CLI calls this tool instead, as
//! `palace backup` calls `palace_backup`.
//! What: [`handle_palace_migrate`] parses the request and runs the CLI's own
//! `run_request` on a blocking thread against the daemon's palace root. The
//! report's `ok` is false when a check failed; the CLI then exits non-zero.
//! Test: `palace_migrate_tool_dispatches_and_rejects_a_bad_mode`,
//! `tool_definitions_lists_all_tools`, `mcp_schema_matches_snapshot`.

use anyhow::{Context, Result, anyhow};
use serde_json::Value;

use crate::AppState;
use crate::commands::palace_migrate::{MigrateMode, MigrateRequest, run_request};

/// `palace_migrate {palace?, all?, mode?, yes?}` (#10094).
///
/// What: `mode` is `migrate` (default), `dry_run` or `verify`. Without `all`,
/// `palace` falls back to the server's `--palace` default, as the other
/// palace tools do. A malformed argument is an error before anything opens.
/// Test: `palace_migrate_tool_dispatches_and_rejects_a_bad_mode`.
pub(crate) async fn handle_palace_migrate(state: &AppState, args: Value) -> Result<Value> {
    let req = parse_request(state, &args)?;
    let root = state.data_root.clone();
    tokio::task::spawn_blocking(move || run_request(&root, &req))
        .await
        .context("join palace_migrate")?
}

/// The request `args` describe.
fn parse_request(state: &AppState, args: &Value) -> Result<MigrateRequest> {
    let flag = |name: &str| -> Result<bool> {
        match args.get(name) {
            None | Some(Value::Null) => Ok(false),
            Some(Value::Bool(b)) => Ok(*b),
            Some(other) => Err(anyhow!(
                "palace_migrate: '{name}' must be a boolean, got {other}"
            )),
        }
    };
    let mode = match args.get("mode") {
        None | Some(Value::Null) => MigrateMode::Migrate,
        Some(v) => serde_json::from_value(v.clone()).map_err(|_| {
            anyhow!(
                "palace_migrate: 'mode' must be \"migrate\", \"dry_run\" or \"verify\", got {v}"
            )
        })?,
    };
    let all = flag("all")?;
    let palace = match (args.get("palace").and_then(Value::as_str), all) {
        (Some(p), _) => Some(p.to_string()),
        (None, true) => None,
        (None, false) => Some(super::helpers::resolve_palace(
            state,
            args,
            "palace_migrate",
        )?),
    };
    Ok(MigrateRequest {
        palace,
        all,
        mode,
        yes: flag("yes")?,
    })
}
