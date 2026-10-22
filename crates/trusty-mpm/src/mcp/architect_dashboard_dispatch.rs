//! Routing for the `architect_dashboard` MCP tool (#10063 PR 3).
//!
//! Why: the tool must run the same bodies as the
//! `mpm.architect.dashboard.*` socket methods (DOC-82 LDG-3.3).
//! What: [`try_dispatch`] answers `Some` for `architect_dashboard` only: it
//! removes `op`, runs [`crate::core::architect_dashboard::call`] with the
//! rest on the blocking pool, and returns the reply as JSON; an error is the
//! store's message with its exit code.
//! Test: `dispatch_architect_dashboard_tool_writes_and_conflicts`.

use serde_json::Value;

use crate::core::architect_dashboard;

/// Route an `architect_dashboard` tool call; `None` for any other tool name.
pub async fn try_dispatch(name: &str, args: &Value) -> Option<Result<Value, String>> {
    if name != "architect_dashboard" {
        return None;
    }
    let mut params = args.clone();
    let op = params
        .as_object_mut()
        .and_then(|m| m.remove("op"))
        .and_then(|v| v.as_str().map(str::to_owned));
    let Some(op) = op else {
        return Some(Err("architect_dashboard: `op` is required (exit 2)".into()));
    };
    // #10153: MCP calls are not bound to the calling session (LDG-3.6, gap G17).
    let joined = tokio::task::spawn_blocking(move || architect_dashboard::call(&op, params)).await;
    Some(match joined {
        Ok(Ok(v)) => Ok(v),
        Ok(Err(e)) => Err(format!(
            "architect_dashboard: {e} (exit {})",
            e.code().exit_code()
        )),
        Err(e) => Err(format!("architect_dashboard: {e} (exit 3)")),
    })
}
