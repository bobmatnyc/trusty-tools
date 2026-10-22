//! Routing for the `dbatg` MCP tool (#9758).
//!
//! Why: the tool must run the same bodies as the `mpm.dbatg.*` socket
//! methods (DOC-82 LDG-3.3), so a PM without shell writes exactly as
//! `tm dbatg` does.
//! What: [`try_dispatch`] answers `Some` for `dbatg` only: it removes `op`,
//! decodes the rest as that method's request (unknown keys refused) and runs
//! the [`crate::core::dbatg`] body on the blocking pool. A write (`init`,
//! `batch`) naming a session other than the bridge process's own is refused
//! first (LDG-3.6); a read (`show`) is not. An error is the ledger's message
//! with its exit code.
//! Test: `dbatg_tool_writes_and_reads_a_store`,
//! `a_pm_writing_another_sessions_store_is_refused_mcp`.

use std::sync::OnceLock;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::Value;
use trusty_architect::ledger::LedgerError;
use trusty_architect::ledger::rpc::{BatchRequest, InitRequest, ShowRequest};

use crate::core::dbatg;

/// Route a `dbatg` tool call; `None` for any other tool name.
pub async fn try_dispatch(name: &str, args: &Value) -> Option<Result<Value, String>> {
    // #9758: the bridge's session is fixed for its lifetime; resolve it once.
    static OWN: OnceLock<Option<String>> = OnceLock::new();
    let own = OWN.get_or_init(dbatg::own_session).as_deref();
    try_dispatch_as(name, args, own).await
}

/// [`try_dispatch`] for a bridge whose own session is `own`.
pub async fn try_dispatch_as(
    name: &str,
    args: &Value,
    own: Option<&str>,
) -> Option<Result<Value, String>> {
    if name != "dbatg" {
        return None;
    }
    let mut params = args.clone();
    let op = params
        .as_object_mut()
        .and_then(|m| m.remove("op"))
        .and_then(|v| v.as_str().map(str::to_owned));
    // #9758: LDG-3.6 — a write names the bridge's own session or is refused.
    let mine = |project: &std::path::Path, session: &str| match own {
        Some(o) if o == session => Ok(()),
        _ => Err(dbatg::foreign_store(project, session, own)),
    };
    Some(match op.as_deref() {
        Some("init") => {
            let check = |r: &InitRequest| mine(&r.project, &r.session);
            run(params, check, |r| dbatg::init(&r)).await
        }
        Some("batch") => {
            let check = |r: &BatchRequest| mine(&r.store.project, &r.store.session);
            run(params, check, |r| dbatg::batch(&r)).await
        }
        Some("show") => run(params, |_: &ShowRequest| Ok(()), |r| dbatg::show(&r)).await,
        _ => Err("dbatg: `op` must be init, batch or show (exit 2)".into()),
    })
}

async fn run<Req, Resp>(
    params: Value,
    check: impl FnOnce(&Req) -> Result<(), LedgerError>,
    body: fn(Req) -> Result<Resp, LedgerError>,
) -> Result<Value, String>
where
    Req: DeserializeOwned + Send + 'static,
    Resp: Serialize + Send + 'static,
{
    let ledger = |e: LedgerError| format!("dbatg: {e} (exit {})", e.code().exit_code());
    let req: Req = serde_json::from_value(params).map_err(|e| format!("dbatg: {e} (exit 2)"))?;
    check(&req).map_err(ledger)?;
    let resp = tokio::task::spawn_blocking(move || body(req))
        .await
        .map_err(|e| format!("dbatg: {e} (exit 3)"))?
        .map_err(ledger)?;
    serde_json::to_value(resp).map_err(|e| format!("dbatg: {e} (exit 3)"))
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use trusty_architect::ledger::init_store;

    use super::{try_dispatch, try_dispatch_as};

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
        let first = try_dispatch_as("dbatg", &add, Some("pm-a"))
            .await
            .expect("routed")
            .expect("written");
        assert_eq!(first["events"][0]["id"], "th-G01");
        let again = try_dispatch_as("dbatg", &add, Some("pm-a"))
            .await
            .expect("routed")
            .expect("replayed");
        assert_eq!(again, first);
        let show = json!({"op": "show", "store": store, "brief": true});
        let text = try_dispatch_as("dbatg", &show, Some("pm-a"))
            .await
            .expect("routed")
            .expect("read");
        assert_eq!(text["text"], "TOP th-G01 lorem");
        let bad = json!({"op": "batch", "store": store, "request_id": "r2",
            "ops": [{"op": "add", "kind": "T", "title": "t", "goal": "th-G09"}]});
        let err = try_dispatch_as("dbatg", &bad, Some("pm-a"))
            .await
            .expect("routed")
            .expect_err("refused");
        assert!(err.ends_with("(exit 2)"), "{err}");
        assert!(try_dispatch("session_list", &json!({})).await.is_none());
    }

    /// LDG-3.6 check (#9758): a PM writing another session's store is
    /// refused by the bridge — a `batch` or `init` naming a session that is
    /// not the bridge's own, or with its own unknown — and nothing is
    /// written; a read of that store still answers.
    #[tokio::test]
    async fn a_pm_writing_another_sessions_store_is_refused_mcp() {
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
        let state = root.join(".trusty-mpm/dbatg/pm-a/state.json");
        let before = std::fs::read(&state).expect("state");
        let store = json!({"project": root, "session": "pm-a"});
        let add = json!({"op": "batch", "store": store, "request_id": "r1",
            "ops": [{"op": "add", "kind": "G", "title": "lorem"}]});
        let init = json!({"op": "init", "project": root, "session": "pm-c", "code": "tc"});
        for own in [Some("pm-b"), None] {
            for call in [&add, &init] {
                let err = try_dispatch_as("dbatg", call, own)
                    .await
                    .expect("routed")
                    .expect_err("the bridge wrote another session's store");
                assert!(err.contains("own store"), "{err}");
                assert!(err.ends_with("(exit 2)"), "{err}");
            }
        }
        assert_eq!(std::fs::read(&state).expect("state"), before);
        assert!(!root.join(".trusty-mpm/dbatg/pm-c").exists());
        let show = json!({"op": "show", "store": store, "brief": true});
        let text = try_dispatch_as("dbatg", &show, Some("pm-b"))
            .await
            .expect("routed")
            .expect("read");
        assert_eq!(text["text"], "no open items");
    }
}
