//! Routing for the `dbatg` MCP tool (#9758).
//!
//! Why: the tool must run the same bodies as the `mpm.dbatg.*` socket
//! methods (DOC-82 LDG-3.3), so a PM without shell writes exactly as
//! `tm dbatg` does.
//! What: [`try_dispatch`] answers `Some` for `dbatg` only: it removes `op`,
//! decodes the rest as that method's request (unknown keys refused) and runs
//! the [`crate::core::dbatg`] body on the blocking pool. An error is the
//! ledger's message with its exit code.
//! Test: `dbatg_tool_writes_and_reads_a_store`.

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use trusty_architect::ledger::LedgerError;

use crate::core::dbatg;

/// Route a `dbatg` tool call; `None` for any other tool name.
pub async fn try_dispatch(name: &str, args: &Value) -> Option<Result<Value, String>> {
    if name != "dbatg" {
        return None;
    }
    let mut params = args.clone();
    let op = params
        .as_object_mut()
        .and_then(|m| m.remove("op"))
        .and_then(|v| v.as_str().map(str::to_owned));
    // #10153: MCP writes not session-bound; check belongs in the serve --stdio bridge
    Some(match op.as_deref() {
        Some("init") => run(params, |r| dbatg::init(&r)).await,
        Some("batch") => run(params, |r| dbatg::batch(&r)).await,
        Some("show") => run(params, |r| dbatg::show(&r)).await,
        _ => Err("dbatg: `op` must be init, batch or show (exit 2)".into()),
    })
}

async fn run<Req, Resp>(
    params: Value,
    body: fn(Req) -> Result<Resp, LedgerError>,
) -> Result<Value, String>
where
    Req: DeserializeOwned + Send + 'static,
    Resp: Serialize + Send + 'static,
{
    let req: Req = serde_json::from_value(params).map_err(|e| format!("dbatg: {e} (exit 2)"))?;
    let resp = tokio::task::spawn_blocking(move || body(req))
        .await
        .map_err(|e| format!("dbatg: {e} (exit 3)"))?
        .map_err(|e| format!("dbatg: {e} (exit {})", e.code().exit_code()))?;
    serde_json::to_value(resp).map_err(|e| format!("dbatg: {e} (exit 3)"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trusty_architect::ledger::init_store;

    use super::try_dispatch;

    /// A PM with no shell adds, replays and reads through the tool.
    #[tokio::test]
    async fn dbatg_tool_writes_and_reads_a_store() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let root = std::fs::canonicalize(tmp.path()).expect("canonical");
        init_store(
            &root.join("codes.json"),
            &root,
            "pm-a",
            "th",
            "2026-10-21T09:00Z",
            &|_| true,
        )
        .expect("init");
        let store = json!({"project": root, "session": "pm-a"});
        let add = json!({"op": "batch", "store": store, "request_id": "r1",
            "ops": [{"op": "add", "kind": "G", "title": "lorem"}]});
        let first = try_dispatch("dbatg", &add)
            .await
            .expect("routed")
            .expect("written");
        assert_eq!(first["events"][0]["id"], "th-G01");
        let again = try_dispatch("dbatg", &add)
            .await
            .expect("routed")
            .expect("replayed");
        assert_eq!(again, first);
        let show = json!({"op": "show", "store": store, "brief": true});
        let text = try_dispatch("dbatg", &show)
            .await
            .expect("routed")
            .expect("read");
        assert_eq!(text["text"], "TOP th-G01 lorem");
        let bad = json!({"op": "batch", "store": store, "request_id": "r2",
            "ops": [{"op": "add", "kind": "T", "title": "t", "goal": "th-G09"}]});
        let err = try_dispatch("dbatg", &bad)
            .await
            .expect("routed")
            .expect_err("refused");
        assert!(err.ends_with("(exit 2)"), "{err}");
        assert!(try_dispatch("session_list", &json!({})).await.is_none());
    }
}
