//! `tm hook` glue for the #11674 subagent task round trip.
//!
//! Why: the daemon knows the bound ledger task; only the hook sees the
//! subagent's `last_assistant_message` and can answer Claude Code. This file
//! joins the two without letting either failure crash or hang the hook.
//! What: [`add_dispatch_key`] adds the dispatch `tool_use_id` (from the
//! subagent's `.meta.json`) to the stop payload before the POST; [`on_stop`]
//! judges the handback against the binding in the daemon's reply, appends
//! one event to the PM's datg `log.jsonl` when that store exists, blocks the
//! stop once for `missing`/`mismatch`, and prints the hook's stdout JSON.
//! Test: `core::subagent_task_roundtrip` tests cover every verdict and the
//! block rule; `subagent_task_roundtrip_meta_tool_use_id` covers the join key.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_architect::ledger::fresh::state_dir;
use trusty_architect::ledger::store::launch_dir;
use trusty_mpm::core::subagent_task_roundtrip::{
    self as rt, DISPATCH_TOOL_USE_ID_KEY, StopBinding,
};

/// Largest `.meta.json` read; a real one is a few hundred bytes.
const MAX_META_BYTES: u64 = 64 * 1024;

/// The dispatch `tool_use_id` recorded beside a subagent transcript.
///
/// What: `<dir>/agent-<id>.jsonl` → `<dir>/agent-<id>.meta.json`, field
/// `toolUseId`. `None` on any read or parse failure.
pub(crate) fn meta_tool_use_id(transcript: &Path) -> Option<String> {
    let name = transcript.file_name()?.to_str()?.strip_suffix(".jsonl")?;
    let meta = transcript.with_file_name(format!("{name}.meta.json"));
    if std::fs::metadata(&meta).ok()?.len() > MAX_META_BYTES {
        return None;
    }
    let v: Value = serde_json::from_str(&std::fs::read_to_string(meta).ok()?).ok()?;
    v.get("toolUseId")?.as_str().map(str::to_owned)
}

/// Add the dispatch `tool_use_id` to an outgoing `SubagentStop` payload.
pub(crate) fn add_dispatch_key(stdin: Option<&Value>, payload: &mut Value) {
    let id = stdin
        .and_then(|v| v.get("agent_transcript_path"))
        .and_then(Value::as_str)
        .and_then(|p| meta_tool_use_id(Path::new(p)));
    if let (Some(id), Some(obj)) = (id, payload.as_object_mut()) {
        obj.insert(DISPATCH_TOOL_USE_ID_KEY.to_owned(), Value::from(id));
    }
}

/// Judge one `SubagentStop`; never fails, prints one JSON object to stdout.
pub(crate) fn on_stop(stdin: Option<&Value>, response: Option<&Value>) {
    let Some(stdin) = stdin else { return };
    let str_of = |k: &str| stdin.get(k).and_then(Value::as_str);
    // Fail closed: an unparseable binding is treated as no binding.
    let binding: Option<StopBinding> = response
        .and_then(|r| r.get("task_binding"))
        .and_then(|b| serde_json::from_value(b.clone()).ok());
    let session = super::il_auto_apply::datg_session().unwrap_or_default();
    let mut event = rt::classify(
        binding.as_ref(),
        str_of("agent_id"),
        str_of("last_assistant_message"),
        chrono::Utc::now().to_rfc3339(),
        &session,
    );
    if event.tool_use_id.is_none() {
        event.tool_use_id =
            str_of("agent_transcript_path").and_then(|p| meta_tool_use_id(Path::new(p)));
    }
    let stop_hook_active = stdin
        .get("stop_hook_active")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let key = event.tool_use_id.clone().or_else(|| event.agent_id.clone());
    event.blocked = rt::wants_block(&event, stop_hook_active)
        && key.is_some_and(|k| rt::claim_block_once(&rt::block_marker_dir(), &k));
    if let Some(log) = ledger_log(str_of("cwd"), &session)
        && let Err(e) = rt::append_ledger_event(&log, &event)
    {
        eprintln!(
            "tm hook: #11674 ledger event not written to {}: {e}",
            log.display()
        );
    }
    eprintln!("tm hook: {}", rt::summary(&event));
    println!("{}", rt::hook_output(&event));
}

/// The PM's datg `log.jsonl`, only when its store exists.
fn ledger_log(cwd: Option<&str>, session: &str) -> Option<PathBuf> {
    let cwd = cwd
        .map(PathBuf::from)
        .or_else(|| std::env::current_dir().ok())?;
    let dir = state_dir(&launch_dir(&cwd), Some(session).filter(|s| !s.is_empty()));
    dir.join("state.json")
        .is_file()
        .then(|| dir.join("log.jsonl"))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn subagent_task_roundtrip_meta_tool_use_id() {
        let dir = tempfile::tempdir().expect("tempdir");
        let t = dir.path().join("agent-a1.jsonl");
        assert_eq!(meta_tool_use_id(&t), None, "no meta file");
        std::fs::write(
            dir.path().join("agent-a1.meta.json"),
            r#"{"agentType":"research","toolUseId":"toolu_01X"}"#,
        )
        .expect("write");
        assert_eq!(meta_tool_use_id(&t).as_deref(), Some("toolu_01X"));
        let stdin = serde_json::json!({ "agent_transcript_path": t });
        let mut payload = serde_json::json!({ "agent_id": "a1" });
        add_dispatch_key(Some(&stdin), &mut payload);
        assert_eq!(payload[DISPATCH_TOOL_USE_ID_KEY], "toolu_01X");
    }
}
