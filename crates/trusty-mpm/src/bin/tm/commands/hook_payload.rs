//! `tm hook` → daemon payload construction (#2864 S1).
//!
//! Why: the hook handler forwarded only `cwd`, `tool`, `input`, and the
//! idle-parking flags to `POST /hooks`. That is enough to audit a tool call but
//! not enough to *correlate* one: a `PreToolUse` dispatching a subagent and the
//! `SubagentStop` that ends it were indistinguishable from any other pair of
//! events, so the daemon could never tell which of several concurrent subagents
//! had finished. The correlation keys Claude Code already ships in the hook
//! payload — `tool_use_id` on the tool events and `agent_id` on `SubagentStop` —
//! were simply being dropped on the floor. This module forwards them.
//!
//! Extracting the builder out of `commands::misc` is deliberate: `misc.rs` sits
//! ~10 SLOC under the 500-line production cap, and a pure function is unit
//! testable without spawning the binary or standing up an HTTP server.
//!
//! What: [`build_hook_payload`] returns the JSON object sent as `payload`. Every
//! field it adds beyond the pre-#2864 set is additive and emitted only when the
//! source field is present, so existing daemon consumers are unaffected.
//! [`compact_tool_response`] bounds what a `PostToolUse` may carry — see its own
//! note on why the raw `tool_response` must never be forwarded wholesale.
//!
//! Field names are those Claude Code actually emits, confirmed empirically
//! against Claude Code 2.1.220 by capturing live hook stdin (see the #2864
//! Step-0 probe): `PreToolUse`/`PostToolUse` carry `tool_use_id` and
//! `transcript_path`; `SubagentStop` carries `agent_id`, `agent_type`, and
//! `agent_transcript_path` (the *subagent's* transcript — distinct from
//! `transcript_path`, which on that event is the *parent's*).
//!
//! Test: the `#[cfg(test)]` suite below, plus the end-to-end
//! `tm_hook_delegation_payload` integration test which asserts these fields
//! survive the real binary's stdin → HTTP POST path.

use serde_json::Value;
use trusty_mpm::core::agent::TOOL_RESPONSE_KEYS;

/// Fields copied verbatim from the stdin hook payload when present.
///
/// Why: all five are short, bounded scalars that the daemon's delegation
/// tracker needs and that no existing consumer reads, so copying them is cheap
/// and cannot regress anything. `tool_response` is deliberately NOT in this list
/// — see [`compact_tool_response`].
/// What: `transcript_path` and `tool_use_id` (tool events), `agent_id`,
/// `agent_type`, `agent_transcript_path` (`SubagentStop`).
/// Test: `forwards_pre_tool_use_correlation_keys`,
/// `forwards_subagent_stop_correlation_keys`.
const PASSTHROUGH_FIELDS: &[&str] = &[
    "transcript_path",
    "tool_use_id",
    "agent_id",
    "agent_type",
    "agent_transcript_path",
];

/// Reduce a subagent-dispatch `tool_response` to its correlation fields.
///
/// Why: a raw `tool_response` carries the subagent's entire result payload —
/// unbounded text that would be POSTed to the daemon on every dispatch and then
/// held in the ring buffer. The daemon needs five small scalars from it and
/// nothing else, so forwarding the whole object would buy nothing and cost an
/// arbitrary amount of memory and bandwidth. Projecting to a fixed key set makes
/// the forwarded size constant regardless of how much the subagent wrote.
/// What: returns an object holding whichever of [`TOOL_RESPONSE_KEYS`] are
/// present, or `None` when `response` is not an object or holds none of them.
/// Test: `compacts_tool_response_to_correlation_keys`,
/// `compact_tool_response_drops_bulk_output`.
fn compact_tool_response(response: &Value) -> Option<Value> {
    let obj = response.as_object()?;
    let mut out = serde_json::Map::new();
    for key in TOOL_RESPONSE_KEYS {
        if let Some(v) = obj.get(*key) {
            out.insert((*key).to_string(), v.clone());
        }
    }
    (!out.is_empty()).then_some(Value::Object(out))
}

/// Build the `payload` object for one `POST /hooks` relay.
///
/// Why: one place that decides what the daemon is told about a hook event, so
/// the forwarded shape is auditable and testable rather than smeared across the
/// handler. See the module note for why the correlation keys matter.
/// What: always emits `cwd` (issue #1744 — the daemon correlates a
/// `SessionStart` to a managed session by workspace path). Adds
/// `idle_parking`/`idle_parking_phrase` when `idle_parking_phrase` is `Some`
/// (#2610); `tool`/`input` from `tool_name`/`tool_input` (#1956); each present
/// member of [`PASSTHROUGH_FIELDS`]; and, only for a
/// [`is_subagent_dispatch_tool`](trusty_mpm::core::agent::is_subagent_dispatch_tool)
/// tool, a [`compact_tool_response`] under `tool_response` (#2864).
///
/// This function performs no I/O and writes nothing to stdout — it is called on
/// the `PreToolUse` path, which has a hard stdout-purity contract (the hook's
/// stdout must contain only the rewrite JSON object, if any).
/// Test: the `#[cfg(test)]` suite below.
pub(crate) fn build_hook_payload(
    cwd: &str,
    stdin_payload: Option<&Value>,
    idle_parking_phrase: Option<&str>,
) -> Value {
    let mut payload = serde_json::json!({ "cwd": cwd });

    // #2610: forward the idle-parking signal so the daemon can surface it and
    // auto-nudge.
    if let Some(phrase) = idle_parking_phrase {
        payload["idle_parking"] = Value::Bool(true);
        payload["idle_parking_phrase"] = Value::String(phrase.to_string());
    }

    let Some(stdin) = stdin_payload else {
        return payload;
    };

    // #1956: the daemon's hook_service reads `payload["tool"]`/`["input"]`.
    let tool_name = stdin.get("tool_name").and_then(Value::as_str);
    if let Some(name) = tool_name {
        payload["tool"] = Value::String(name.to_string());
    }
    if let Some(input) = stdin.get("tool_input") {
        payload["input"] = input.clone();
    }

    // #2864: correlation keys. Copied only when present — a hook event that
    // does not carry a field simply omits it.
    for field in PASSTHROUGH_FIELDS {
        if let Some(v) = stdin.get(*field)
            && !v.is_null()
        {
            payload[*field] = v.clone();
        }
    }

    // #2864: the dispatch response, compacted, and only for a subagent-dispatch
    // tool — this is where `agentId` comes from.
    if tool_name.is_some_and(trusty_mpm::core::agent::is_subagent_dispatch_tool)
        && let Some(response) = stdin.get("tool_response")
        && let Some(compact) = compact_tool_response(response)
    {
        payload["tool_response"] = compact;
    }

    payload
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `PreToolUse` payload as Claude Code 2.1.220 actually emits it for a
    /// subagent dispatch (verbatim shape from the #2864 Step-0 capture).
    fn pre_tool_use_agent() -> Value {
        serde_json::json!({
            "session_id": "d6066d66-8c7f-41f1-8914-8d0710d563aa",
            "transcript_path": "/tmp/proj/d6066d66.jsonl",
            "cwd": "/tmp/proj",
            "prompt_id": "31fd3d1b-d586-412f-9772-2baf48d56acf",
            "permission_mode": "bypassPermissions",
            "hook_event_name": "PreToolUse",
            "tool_name": "Agent",
            "tool_input": {
                "description": "probe alpha",
                "prompt": "Reply with ALPHA.",
                "subagent_type": "general-purpose"
            },
            "tool_use_id": "toolu_01DAvdgnCx8jYZQmn8NYpUge"
        })
    }

    #[test]
    fn forwards_pre_tool_use_correlation_keys() {
        let p = build_hook_payload("/tmp/proj", Some(&pre_tool_use_agent()), None);
        assert_eq!(p["cwd"], "/tmp/proj");
        assert_eq!(p["tool"], "Agent");
        assert_eq!(p["input"]["subagent_type"], "general-purpose");
        assert_eq!(p["input"]["description"], "probe alpha");
        // The #2864 additions.
        assert_eq!(p["tool_use_id"], "toolu_01DAvdgnCx8jYZQmn8NYpUge");
        assert_eq!(p["transcript_path"], "/tmp/proj/d6066d66.jsonl");
    }

    #[test]
    fn forwards_subagent_stop_correlation_keys() {
        // SubagentStop carries `agent_id` (the correlation key) plus BOTH the
        // parent transcript (`transcript_path`) and the subagent's own
        // (`agent_transcript_path`) — they are different files.
        let stdin = serde_json::json!({
            "hook_event_name": "SubagentStop",
            "session_id": "d6066d66-8c7f-41f1-8914-8d0710d563aa",
            "transcript_path": "/tmp/proj/d6066d66.jsonl",
            "agent_id": "a403cdbc078b5c474",
            "agent_type": "general-purpose",
            "agent_transcript_path": "/tmp/proj/d6066d66/subagents/agent-a403cdbc078b5c474.jsonl",
            "last_assistant_message": "ALPHA"
        });
        let p = build_hook_payload("/tmp/proj", Some(&stdin), None);
        assert_eq!(p["agent_id"], "a403cdbc078b5c474");
        assert_eq!(p["agent_type"], "general-purpose");
        assert_eq!(
            p["agent_transcript_path"],
            "/tmp/proj/d6066d66/subagents/agent-a403cdbc078b5c474.jsonl"
        );
        assert_eq!(p["transcript_path"], "/tmp/proj/d6066d66.jsonl");
        // No tool on a stop event.
        assert!(p.get("tool").is_none());
    }

    #[test]
    fn compacts_tool_response_to_correlation_keys() {
        let stdin = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Agent",
            "tool_use_id": "toolu_01DAvdgnCx8jYZQmn8NYpUge",
            "tool_response": {
                "isAsync": true,
                "status": "async_launched",
                "agentId": "a403cdbc078b5c474",
                "resolvedModel": "claude-haiku-4-5-20251001",
                "description": "probe alpha",
                "prompt": "Reply with ALPHA.",
                "outputFile": "/tmp/out.txt",
                "canReadOutputFile": true
            }
        });
        let p = build_hook_payload("/tmp/proj", Some(&stdin), None);
        assert_eq!(p["tool_response"]["agentId"], "a403cdbc078b5c474");
        assert_eq!(p["tool_response"]["status"], "async_launched");
        assert_eq!(p["tool_response"]["isAsync"], true);
        assert_eq!(
            p["tool_response"]["resolvedModel"],
            "claude-haiku-4-5-20251001"
        );
        // Bulk / irrelevant fields are dropped.
        assert!(p["tool_response"].get("prompt").is_none());
        assert!(p["tool_response"].get("outputFile").is_none());
    }

    #[test]
    fn compact_tool_response_drops_bulk_output() {
        // A non-dispatch tool's response must never be forwarded at all, no
        // matter how large — this is the bandwidth guard.
        let huge = "x".repeat(200_000);
        let stdin = serde_json::json!({
            "hook_event_name": "PostToolUse",
            "tool_name": "Bash",
            "tool_use_id": "toolu_bash",
            "tool_response": { "stdout": huge, "is_error": false }
        });
        let p = build_hook_payload("/tmp/proj", Some(&stdin), None);
        assert!(
            p.get("tool_response").is_none(),
            "non-dispatch tool_response must not be forwarded"
        );
        assert!(p.to_string().len() < 1000, "payload must stay bounded");
    }

    #[test]
    fn preserves_idle_parking_fields() {
        // #2610 behaviour must be unchanged by the #2864 additions.
        let stdin = serde_json::json!({
            "hook_event_name": "SubagentStop",
            "agent_id": "a403cdbc078b5c474"
        });
        let p = build_hook_payload("/w", Some(&stdin), Some("waiting for the monitor"));
        assert_eq!(p["idle_parking"], true);
        assert_eq!(p["idle_parking_phrase"], "waiting for the monitor");
        assert_eq!(p["agent_id"], "a403cdbc078b5c474");
    }

    #[test]
    fn no_stdin_payload_yields_cwd_only() {
        let p = build_hook_payload("/w", None, None);
        assert_eq!(p["cwd"], "/w");
        assert_eq!(p.as_object().expect("object").len(), 1);
    }

    #[test]
    fn absent_fields_are_omitted_not_nulled() {
        // A minimal Bash PreToolUse has no correlation keys; the builder must
        // omit them rather than emit explicit nulls (which would make the
        // daemon's `get(..).and_then(as_str)` reads ambiguous).
        let stdin = serde_json::json!({
            "hook_event_name": "PreToolUse",
            "tool_name": "Bash",
            "tool_input": { "command": "ls" },
            "agent_id": Value::Null
        });
        let p = build_hook_payload("/w", Some(&stdin), None);
        assert!(p.get("agent_id").is_none());
        assert!(p.get("tool_use_id").is_none());
        assert!(p.get("transcript_path").is_none());
    }
}
