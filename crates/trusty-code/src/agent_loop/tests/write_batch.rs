//! Regression guards for batched file writes (#2681 — decoupling agent
//! turn-count from file-count).
//!
//! Why: On every bake-off run the number of files a solution needed equalled the
//! number of write-turns 1:1 (an 8-file scaffold cost 8 turns), the single
//! biggest driver of tcode's high agent-turn count. Two levers fix this: (1) the
//! loop's `dispatch_all` executes N `write_file` calls emitted in ONE turn, and
//! (2) the dedicated `write_files` tool writes an array of files in a single
//! call. These tests pin BOTH so a future loop/tool change cannot silently
//! reintroduce one-file-per-turn behaviour. Split into this focused child module
//! (from `agent_loop::tests`) to keep the parent file under its SLOC cap while
//! reusing its scripted-LLM harness verbatim via `use super::*`.
//! What: Reuses the parent module's `ScriptedLlm`, `stop_response`, and
//! `make_loop` helpers; registers the real `WriteFileTool`/`WriteFilesTool` over
//! a tempdir and asserts a single turn writes every file.
//! Test: this module is itself the test surface.

use super::*;
use crate::tools::{WriteFileTool, WriteFilesTool};

/// Build a response carrying MULTIPLE `write_file` tool calls in one assistant
/// choice.
///
/// Why: Proves a single turn emitting N `write_file` calls writes all N files —
/// the batching lever's first form.
/// What: Emits one assistant message whose `tool_calls` array names `write_file`
/// once per `(call_id, path, content)` triple, with `finish_reason ==
/// "tool_calls"`.
/// Test: `batched_write_file_calls_write_all_files_in_one_turn`.
fn multi_write_file_response(files: &[(&str, &str, &str)]) -> Value {
    let tool_calls: Vec<Value> = files
        .iter()
        .map(|(id, path, content)| {
            let arguments = json!({ "path": path, "content": content }).to_string();
            json!({
                "id": id,
                "type": "function",
                "function": { "name": "write_file", "arguments": arguments }
            })
        })
        .collect();
    json!({
        "id": "gen-multi-write",
        "choices": [{
            "message": { "role": "assistant", "content": null, "tool_calls": tool_calls },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 20, "completion_tokens": 15, "total_tokens": 35 }
    })
}

/// Build a response carrying ONE `write_files` batch tool call.
///
/// Why: Proves the dedicated batch tool writes an array of files in a single
/// call — the batching lever's second, structural form.
/// What: Emits one assistant message whose single `write_files` tool call
/// carries a `files` array built from the `(path, content)` pairs.
/// Test: `write_files_tool_writes_all_files_in_one_turn`.
fn write_files_call_response(call_id: &str, files: &[(&str, &str)]) -> Value {
    let arr: Vec<Value> = files
        .iter()
        .map(|(path, content)| json!({ "path": path, "content": content }))
        .collect();
    let arguments = json!({ "files": arr }).to_string();
    json!({
        "id": "gen-write-files",
        "choices": [{
            "message": {
                "role": "assistant",
                "content": null,
                "tool_calls": [{
                    "id": call_id,
                    "type": "function",
                    "function": { "name": "write_files", "arguments": arguments }
                }]
            },
            "finish_reason": "tool_calls"
        }],
        "usage": { "prompt_tokens": 20, "completion_tokens": 15, "total_tokens": 35 }
    })
}

/// A single turn emitting N `write_file` calls writes ALL N files, in one turn.
///
/// Why: This is lever 1 of the turn-count/file-count decoupling — the model may
/// batch several `write_file` calls into one turn and the loop must execute
/// every one. Regression guard against a return to one-file-per-turn.
/// What: Registers the real `WriteFileTool` over a tempdir, scripts ONE response
/// with three `write_file` calls, then a stop. Asserts exactly two chat calls
/// (one batch turn + one stop) and that all three files exist on disk with the
/// right content.
/// Test: this test.
#[tokio::test]
async fn batched_write_file_calls_write_all_files_in_one_turn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(WriteFileTool::new(tmp.path())));
    let registry = Arc::new(reg);

    let llm = Arc::new(ScriptedLlm::from_json(&[
        multi_write_file_response(&[
            ("w1", "a.py", "# a"),
            ("w2", "pkg/b.py", "# b"),
            ("w3", "src/main.rs", "fn main() {}"),
        ]),
        stop_response("scaffolded three files in one turn"),
    ]));
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "scaffold three files")
        .await
        .expect("batched write turn should complete");

    assert_eq!(out.content, "scaffolded three files in one turn");
    assert_eq!(
        llm.calls(),
        2,
        "all three writes must happen in ONE turn, then one stop — not three turns"
    );

    assert_eq!(
        std::fs::read_to_string(tmp.path().join("a.py")).unwrap(),
        "# a"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("pkg/b.py")).unwrap(),
        "# b"
    );
    assert_eq!(
        std::fs::read_to_string(tmp.path().join("src/main.rs")).unwrap(),
        "fn main() {}"
    );
}

/// A single `write_files` call writes ALL files in the array, in one turn.
///
/// Why: This is lever 2 — the dedicated batch tool structurally decouples
/// turn-count from file-count even when the model does not batch separate calls.
/// What: Registers the real `WriteFilesTool` over a tempdir, scripts ONE
/// response with a single `write_files` call carrying four files, then a stop.
/// Asserts exactly two chat calls and that all four files exist on disk.
/// Test: this test.
#[tokio::test]
async fn write_files_tool_writes_all_files_in_one_turn() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let mut reg = ToolRegistry::new();
    reg.register(Arc::new(WriteFilesTool::new(tmp.path())));
    let registry = Arc::new(reg);

    let llm = Arc::new(ScriptedLlm::from_json(&[
        write_files_call_response(
            "wf1",
            &[
                ("app/__init__.py", ""),
                ("app/main.py", "print('hi')"),
                ("app/models.py", "# models"),
                ("tests/test_main.py", "def test(): pass"),
            ],
        ),
        stop_response("wrote the whole scaffold in one call"),
    ]));
    let agent = make_loop(llm.clone(), registry, AgentLoopConfig::default());

    let out = agent
        .run("system", "scaffold the app")
        .await
        .expect("write_files turn should complete");

    assert_eq!(out.content, "wrote the whole scaffold in one call");
    assert_eq!(
        llm.calls(),
        2,
        "one write_files call writes every file in ONE turn, then one stop"
    );

    for (path, content) in [
        ("app/__init__.py", ""),
        ("app/main.py", "print('hi')"),
        ("app/models.py", "# models"),
        ("tests/test_main.py", "def test(): pass"),
    ] {
        assert_eq!(
            std::fs::read_to_string(tmp.path().join(path)).unwrap(),
            content,
            "file {path} must be written by the single write_files call"
        );
    }
}
