//! The `dbatg` write-ownership check of the `tm serve --stdio` bridge (#10153).
//!
//! Why: DOC-82 LDG-3.6 — a PM writes only its own Ledger store. The `dbatg`
//! MCP tool runs in the shared daemon, whose environment names no caller, so
//! a daemon-side check resolves the daemon's session, not the caller's. The
//! bridge is the process Claude Code spawned inside the PM's pane, so the check
//! runs here, beside `chat_gate_refusal`, with the rule `tm dbatg` applies (#9758).
//! What: [`BridgeOwner::from_env`] snapshots the bridge's own session
//! ([`dbatg::own_session`]) and launch directory (its cwd through
//! [`resolve_project`]) once. [`dbatg_owner_refusal`] answers a `dbatg` call
//! whose `op` is not `show` with an MCP tool error, and no socket call, unless
//! it names this bridge's own session and project. An own fact the bridge
//! cannot determine proves no store is its own, so the write is refused.
//! Reads and every other request pass untouched (LDG-3.3).
//! Test: `dbatg_owner_*` below; end to end in `dbatg_bridge_owner_10153`.

use std::path::{Path, PathBuf};

use serde_json::{Value, json};
use trusty_architect::ledger::LedgerError;
use trusty_architect::ledger::store::resolve_project;
use trusty_mpm::core::dbatg;

/// The tool this check governs.
const DBATG_TOOL: &str = "dbatg";

/// The bridge's own Ledger identity; `None` where it cannot be determined.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct BridgeOwner {
    /// This PM's session: `$DBATG_SESSION`, else the tmux session of `$TMUX_PANE`.
    pub session: Option<String>,
    /// The launch directory of the bridge's working directory.
    pub launch: Option<PathBuf>,
}

impl BridgeOwner {
    /// Reads this process's session and launch directory, as `tm dbatg` does.
    pub(super) fn from_env() -> Self {
        let launch = std::env::current_dir()
            .ok()
            .and_then(|cwd| resolve_project(&cwd).ok());
        Self {
            session: dbatg::own_session(),
            launch,
        }
    }
}

/// The local refusal for a `dbatg` write to a store not provably this bridge's.
///
/// Why: LDG-3.6; see the module doc for why the bridge, not the daemon, checks.
/// What: `Some(MCP tool error)` (isError true, the ledger message ending in its
/// exit code, as the daemon renders one) when `req` is a `tools/call` for
/// `dbatg` whose `op` is not `show` and its target — `project`/`session` for
/// `init`, `store.project`/`store.session` for every other op — is not
/// `owner`'s. `None` for a read or any other request.
/// Test: `dbatg_owner_allows_its_own_writes_and_every_read`,
/// `dbatg_owner_refuses_a_foreign_session_or_project`,
/// `dbatg_owner_refuses_every_write_when_its_own_facts_are_unknown`.
pub(super) fn dbatg_owner_refusal(req: &Value, owner: &BridgeOwner) -> Option<Value> {
    if req.get("method").and_then(Value::as_str) != Some("tools/call")
        || req.pointer("/params/name").and_then(Value::as_str) != Some(DBATG_TOOL)
    {
        return None;
    }
    let args = req.pointer("/params/arguments");
    let field = |p: &str| args.and_then(|a| a.pointer(p)).and_then(Value::as_str);
    let (project, session) = match field("/op") {
        Some("show") => return None,
        Some("init") => (field("/project"), field("/session")),
        // #10153: fail closed — an op the daemon may add later is a write.
        _ => (field("/store/project"), field("/store/session")),
    };
    let refusal = own_write(project, session, owner).err()?;
    Some(json!({
        "content": [{"type": "text", "text": refusal}],
        "isError": true,
    }))
}

/// `Ok` when `project` and `session` name `owner`'s own store.
fn own_write(
    project: Option<&str>,
    session: Option<&str>,
    owner: &BridgeOwner,
) -> Result<(), String> {
    let (Some(project), Some(session)) = (project, session) else {
        return Err(render(&LedgerError::Invalid(
            "a write must name its store's project and session".into(),
        )));
    };
    let launch = resolve_project(Path::new(project)).map_err(|e| render(&e))?;
    let Some(own_launch) = &owner.launch else {
        return Err(render(&LedgerError::Invalid(
            "a PM writes only its own store (LDG-3.6): this bridge's launch \
             directory is unknown, so no store is provably its own"
                .into(),
        )));
    };
    if *own_launch == launch && owner.session.as_deref() == Some(session) {
        return Ok(());
    }
    Err(render(&dbatg::foreign_store(
        &launch,
        session,
        owner.session.as_deref(),
    )))
}

/// A ledger error as the daemon's `dbatg` tool renders it.
fn render(e: &LedgerError) -> String {
    format!("dbatg: {e} (exit {})", e.code().exit_code())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A launch directory (it holds a `.git` directory) under a tempdir.
    fn project(root: &Path, name: &str) -> PathBuf {
        let dir = root.join(name);
        std::fs::create_dir_all(dir.join(".git")).expect("project dir");
        std::fs::canonicalize(dir).expect("canonical")
    }

    fn call(args: Value) -> Value {
        json!({"jsonrpc": "2.0", "id": 1, "method": "tools/call",
               "params": {"name": "dbatg", "arguments": args}})
    }

    fn batch(project: &Path, session: &str) -> Value {
        call(json!({"op": "batch", "request_id": "r1", "ops": [],
                    "store": {"project": project, "session": session}}))
    }

    fn init(project: &Path, session: &str) -> Value {
        call(json!({"op": "init", "project": project, "session": session, "code": "th"}))
    }

    fn refusal_text(v: Option<Value>) -> String {
        let v = v.expect("refused");
        assert_eq!(v["isError"], true, "{v}");
        v["content"][0]["text"].as_str().expect("text").to_owned()
    }

    /// LDG-3.3: own-session writes and every read pass to the daemon.
    #[test]
    fn dbatg_owner_allows_its_own_writes_and_every_read() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let own = project(tmp.path(), "own");
        let other = project(tmp.path(), "other");
        let owner = BridgeOwner {
            session: Some("pm-a".into()),
            launch: Some(own.clone()),
        };
        assert_eq!(dbatg_owner_refusal(&batch(&own, "pm-a"), &owner), None);
        assert_eq!(dbatg_owner_refusal(&init(&own, "pm-a"), &owner), None);
        // A worktree subdirectory maps to the same launch directory.
        let sub = own.join("src");
        std::fs::create_dir_all(&sub).expect("subdir");
        assert_eq!(dbatg_owner_refusal(&batch(&sub, "pm-a"), &owner), None);
        let show = call(json!({"op": "show", "store": {"project": other, "session": "pm-b"}}));
        assert_eq!(dbatg_owner_refusal(&show, &owner), None);
        let list = json!({"jsonrpc": "2.0", "id": 1, "method": "tools/list"});
        assert_eq!(dbatg_owner_refusal(&list, &owner), None);
    }

    /// LDG-3.6: another session's store, or another project's, is refused.
    #[test]
    fn dbatg_owner_refuses_a_foreign_session_or_project() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let own = project(tmp.path(), "own");
        let other = project(tmp.path(), "other");
        let owner = BridgeOwner {
            session: Some("pm-a".into()),
            launch: Some(own.clone()),
        };
        for req in [
            batch(&own, "pm-b"),
            init(&own, "pm-b"),
            batch(&other, "pm-a"),
            init(&other, "pm-a"),
            call(json!({"op": "digest", "store": {"project": other, "session": "pm-a"}})),
        ] {
            let text = refusal_text(dbatg_owner_refusal(&req, &owner));
            assert!(
                text.contains("LDG-3.6") && text.ends_with("(exit 2)"),
                "{text}"
            );
        }
        let missing = call(json!({"op": "batch", "request_id": "r", "ops": []}));
        let text = refusal_text(dbatg_owner_refusal(&missing, &owner));
        assert!(text.ends_with("(exit 2)"), "{text}");
    }

    /// Fail-open check (#10153): an unknown own session or launch directory
    /// proves no store is the bridge's, so every write is refused.
    #[test]
    fn dbatg_owner_refuses_every_write_when_its_own_facts_are_unknown() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let own = project(tmp.path(), "own");
        let no_session = BridgeOwner {
            session: None,
            launch: Some(own.clone()),
        };
        let text = refusal_text(dbatg_owner_refusal(&batch(&own, "pm-a"), &no_session));
        assert!(text.contains("this session is unknown"), "{text}");
        let no_launch = BridgeOwner {
            session: Some("pm-a".into()),
            launch: None,
        };
        let text = refusal_text(dbatg_owner_refusal(&init(&own, "pm-a"), &no_launch));
        assert!(text.contains("launch directory is unknown"), "{text}");
    }
}
