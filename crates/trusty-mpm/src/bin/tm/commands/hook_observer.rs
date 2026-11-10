//! `tm hook` compression-opportunity observer (#10973 steps 1–2).
//!
//! Why: the savings ledger recorded only the Bash calls `tm hook` rewrote, so
//! subagent results, MCP output, Read/Grep/WebFetch and every unwrapped Bash
//! call were invisible — and so was the October drop in rewrites. The owner's
//! ask: every tool call and every subagent response is looked at for
//! compression opportunities, and tracked. This observer measures; it changes
//! no tool output.
//! What: on `PostToolUse` and `SubagentStop`, [`opportunity_row`] reads the
//! result text out of the payload, dry-runs the existing compressor over at
//! most [`OBSERVER_SCAN_CAP`] bytes of it, and [`observe`] appends one
//! [`OpportunityRow`] to the savings ledger the statusline folds — which skips
//! the technique, so the `💸` figure is unchanged (#7867).
//! Test: `a_subagent_result_and_an_mcp_result_each_append_an_opportunity_row`
//! (end to end), `a_bash_result_is_measured_under_its_derived_tool_name`,
//! `a_huge_result_is_scanned_only_up_to_the_cap`,
//! `a_subagent_stop_without_a_message_writes_no_row`,
//! `an_unwritable_ledger_does_not_fail_the_hook`.

use std::path::Path;

use serde_json::Value;
use trusty_agents_common::compress::compress_tool_output;
use trusty_mpm::core::savings::savings_log_in;
use trusty_mpm::core::savings_opportunity::{OpportunityRow, append_opportunity, tool_type_of};

use super::hook_rewrite::effective_tool_name;

/// The most result bytes one hook invocation hands the compressor's dry run.
///
/// Why: `SubagentStop` runs synchronously inside a 5 s hook budget and a tool
/// result can be many megabytes; the dry run is the only super-linear-risk
/// step. 256 KiB is far above the compressor's size gate yet keeps the dry run
/// to milliseconds. Bytes past the cap are counted, not scanned, and count as
/// uncompressed, so the cap can only understate an opportunity.
/// Test: `a_huge_result_is_scanned_only_up_to_the_cap`.
pub(crate) const OBSERVER_SCAN_CAP: usize = 256 * 1024;

/// Append this event's opportunity row to the operator's ledger, if it has one.
///
/// Why: see the module doc.
/// What: resolves the framework root the statusline reads from (the same
/// `--root`/`TRUSTY_MPM_ROOT`/config chain `tm compress` uses), then
/// [`observe_in`].
/// Test: `a_subagent_result_and_an_mcp_result_each_append_an_opportunity_row`.
pub(crate) fn observe(event: &str, payload: Option<&Value>, session_id: &str) {
    match super::managed_root::resolve_managed_paths(None) {
        Ok(paths) => observe_in(&paths.root, event, payload, session_id),
        Err(e) => {
            eprintln!("trusty-mpm: #10973 opportunity row not written: no framework root: {e}")
        }
    }
}

/// [`observe`] against an explicit framework root.
///
/// FAIL-OPEN, deliberately: a ledger write failure is printed to stderr and
/// the hook continues. The row is telemetry about a tool result that has
/// already reached the model; failing the hook would turn a full disk into a
/// blocked `SubagentStop` delivery (#6556) while recovering nothing.
/// Test: `an_unwritable_ledger_does_not_fail_the_hook`.
pub(crate) fn observe_in(root: &Path, event: &str, payload: Option<&Value>, session_id: &str) {
    let Some(row) = payload.and_then(|p| opportunity_row(event, p, session_id)) else {
        return;
    };
    let ledger = savings_log_in(root);
    if let Err(e) = append_opportunity(&ledger, &row) {
        eprintln!(
            "trusty-mpm: #10973 opportunity row not written to {}: {e}",
            ledger.display()
        );
    }
}

/// The opportunity row an event's payload describes, or `None`.
///
/// What: `PostToolUse` → the `tool_response`, under [`tool_type_of`] the
/// `tool_name`; a Bash result is dry-run under its [`effective_tool_name`], the
/// name `tm compress` itself would use. `SubagentStop` → its
/// `last_assistant_message`, as a `subagent`; `None` when the payload carries
/// none. Any other event, or a result with no text at all, → `None`.
/// Test: `a_bash_result_is_measured_under_its_derived_tool_name`,
/// `a_subagent_stop_without_a_message_writes_no_row`.
pub(crate) fn opportunity_row(
    event: &str,
    payload: &Value,
    session_id: &str,
) -> Option<OpportunityRow> {
    let (tool_type, dispatch, result) = match event {
        "PostToolUse" => {
            let tool = payload.get("tool_name")?.as_str()?;
            let dispatch = match payload["tool_input"]["command"].as_str() {
                Some(cmd) if tool == "Bash" => effective_tool_name(cmd.trim()),
                _ => tool.to_string(),
            };
            (tool_type_of(tool), dispatch, payload.get("tool_response")?)
        }
        "SubagentStop" => {
            let message = payload.get("last_assistant_message")?;
            (
                tool_type_of("SubagentStop"),
                "SubagentStop".to_string(),
                message,
            )
        }
        _ => return None,
    };
    let mut text = String::new();
    let returned = collect_text(result, &mut text, OBSERVER_SCAN_CAP);
    if returned == 0 {
        return None;
    }
    let scanned = text.len() as u64;
    let compressed = compress_tool_output(&dispatch, &text).len() as u64;
    Some(OpportunityRow::new(
        event,
        session_id,
        &tool_type,
        &dispatch,
        returned,
        scanned,
        compressed + (returned - scanned),
    ))
}

/// Total bytes of every string leaf in `value`; the first `cap` of them into `out`.
///
/// Why: Bash returns `{stdout, stderr}`, an MCP tool an array of content blocks,
/// `Agent` a `{content: […]}` object. Counting string leaves measures the text
/// the model reads under every one of those shapes without a per-tool parser.
/// What: depth-first over arrays and object values; `out` grows to at most
/// `cap` bytes, cut on a char boundary. Numbers, booleans and keys are ignored.
/// Test: `a_huge_result_is_scanned_only_up_to_the_cap`.
fn collect_text(value: &Value, out: &mut String, cap: usize) -> u64 {
    match value {
        Value::String(s) => {
            let room = cap.saturating_sub(out.len());
            if room > 0 {
                let mut end = room.min(s.len());
                while !s.is_char_boundary(end) {
                    end -= 1;
                }
                out.push_str(&s[..end]);
            }
            s.len() as u64
        }
        Value::Array(items) => items.iter().map(|v| collect_text(v, out, cap)).sum(),
        Value::Object(map) => map.values().map(|v| collect_text(v, out, cap)).sum(),
        _ => 0,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn cargo_output() -> String {
        let mut s = String::new();
        for i in 0..400 {
            s.push_str(&format!("test mod::t{i} ... ok\n"));
        }
        s.push_str("test result: ok. 400 passed; 0 failed\n");
        s
    }

    #[test]
    fn a_bash_result_is_measured_under_its_derived_tool_name() {
        let out = cargo_output();
        let payload = json!({
            "tool_name": "Bash",
            "tool_input": {"command": "cargo test -p x"},
            "tool_response": {"stdout": out, "stderr": "", "interrupted": false},
        });
        let row = opportunity_row("PostToolUse", &payload, "s1").expect("a row");
        assert_eq!(row.tool_type, "bash");
        assert_eq!(row.tool_name, "cargo test");
        assert_eq!(row.session_id, "s1");
        assert_eq!(row.bytes_returned, out.len() as u64);
        assert!(row.would_compress_bytes < row.bytes_returned, "{row:?}");
    }

    #[test]
    fn a_huge_result_is_scanned_only_up_to_the_cap() {
        let huge = "é".repeat(10 * 1024 * 1024); // 20 MiB, two bytes a char
        let payload = json!({"tool_name": "Read", "tool_response": huge});
        let started = std::time::Instant::now();
        let row = opportunity_row("PostToolUse", &payload, "s1").expect("a row");
        let elapsed = started.elapsed();
        assert_eq!(row.bytes_returned, huge.len() as u64);
        assert!(
            row.bytes_scanned <= OBSERVER_SCAN_CAP as u64,
            "{}",
            row.bytes_scanned
        );
        assert!(row.would_compress_bytes <= row.bytes_returned);
        assert!(elapsed < std::time::Duration::from_secs(2), "{elapsed:?}");
    }

    #[test]
    fn a_subagent_stop_without_a_message_writes_no_row() {
        assert!(opportunity_row("SubagentStop", &json!({"agent_id": "a"}), "s1").is_none());
        assert!(opportunity_row("PreToolUse", &json!({"tool_name": "Read"}), "s1").is_none());
        let stop = json!({"last_assistant_message": "done: 3 files changed"});
        let row = opportunity_row("SubagentStop", &stop, "s1").expect("a row");
        assert_eq!(row.tool_type, "subagent");
        assert_eq!(row.event, "SubagentStop");
    }

    /// #10973 critic finding 1: the statusline reads `savings.jsonl` whole on
    /// every refresh, so opportunity volume must never reach that file.
    #[test]
    fn a_large_opportunity_volume_leaves_the_savings_ledger_untouched() {
        use trusty_mpm::core::savings::{fold_session_per_call, savings_log_in};
        let dir = tempfile::tempdir().expect("tempdir");
        let ledger = savings_log_in(dir.path());
        std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
        let compress_row = r#"{"ts":"2026-11-10T00:00:00Z","session_id":"s1","technique":"compress","tokens_saved":900,"tokens_before":1000,"cost_saved_usd":0.01,"basis":"b","model_source":""}"#;
        std::fs::write(&ledger, format!("{compress_row}\n")).expect("seed");
        let before = std::fs::metadata(&ledger).expect("stat").len();
        let fold_before = fold_session_per_call(&ledger, "s1");
        let payload = json!({"tool_name": "Read", "tool_response": "x".repeat(500)});
        for _ in 0..2_000 {
            observe_in(dir.path(), "PostToolUse", Some(&payload), "s1");
        }
        assert_eq!(std::fs::metadata(&ledger).expect("stat").len(), before);
        assert_eq!(fold_session_per_call(&ledger, "s1"), fold_before);
    }

    #[test]
    fn an_unwritable_ledger_does_not_fail_the_hook() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join("usage"), "not a directory").expect("blocker");
        let payload = json!({"tool_name": "Read", "tool_response": "x".repeat(500)});
        observe_in(dir.path(), "PostToolUse", Some(&payload), "s1");
        assert!(
            dir.path().join("usage").is_file(),
            "the blocker is untouched"
        );
    }
}
