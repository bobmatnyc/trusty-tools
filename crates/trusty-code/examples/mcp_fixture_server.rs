//! A minimal stdio MCP server, for `tests/mcp_loader_e2e.rs` (#5428).
//!
//! Why: the loader's contract is "spawn a real subprocess, speak JSON-RPC to
//! it, register what it advertises". Proving that against a mock client would
//! prove nothing about the transport, and pointing the test at a real
//! trusty-* binary would make it depend on what happens to be installed. This
//! is an EXAMPLE rather than a `[[bin]]` on purpose: `cargo test` builds it,
//! `cargo install` does not ship it.
//!
//! What: newline-delimited JSON-RPC 2.0 on stdin/stdout. Answers `initialize`,
//! `tools/list` (two tools) and `tools/call`; ignores notifications, which
//! carry no `id`. `echo` returns its `message` argument; `echo_env` returns
//! `$TCODE_FIXTURE_TOKEN`, which is how the e2e test proves a configured `env`
//! actually reaches the child. With [`HANG_VAR`] set it reads its input and
//! answers NOTHING, which is the wedged server the concurrency test needs —
//! a server that fails fast would prove nothing about the per-server bound.
//!
//! #7951: an explicit `cargo test --test mcp_loader_e2e` selector builds that
//! target alone and NOT the examples, so the suite used to run against
//! whatever copy of this file was compiled last. `fixture_binary()` in that
//! test now builds this example itself before using it, so either invocation
//! reflects an edit here.
//!
//! Test: `crates/trusty-code/tests/mcp_loader_e2e.rs`.

use std::io::{BufRead, Write};

use serde_json::{Value, json};

/// The env var `echo_env` reports, so the test can prove `env` overlay works.
const TOKEN_VAR: &str = "TCODE_FIXTURE_TOKEN";

/// Set this to make the fixture accept input and never reply.
const HANG_VAR: &str = "TCODE_FIXTURE_HANG";

fn main() {
    if std::env::var_os(HANG_VAR).is_some() {
        // Stay alive and silent until the parent kills us: `is_alive()` must
        // stay true, so exiting would exercise the respawn path instead.
        for line in std::io::stdin().lock().lines() {
            if line.is_err() {
                break;
            }
        }
        std::thread::sleep(std::time::Duration::from_secs(300));
        return;
    }
    let stdin = std::io::stdin();
    let mut stdout = std::io::stdout();
    for line in stdin.lock().lines() {
        let Ok(line) = line else { break };
        let Ok(request) = serde_json::from_str::<Value>(&line) else {
            continue;
        };
        // A notification carries no `id` and must not be answered.
        let Some(id) = request.get("id").cloned() else {
            continue;
        };
        let method = request
            .get("method")
            .and_then(Value::as_str)
            .unwrap_or_default();
        let Some(result) = respond(method, request.get("params")) else {
            continue;
        };
        let envelope = json!({"jsonrpc": "2.0", "id": id, "result": result});
        if writeln!(stdout, "{envelope}").is_err() || stdout.flush().is_err() {
            break;
        }
    }
}

/// The `result` for one request, or `None` for a method this fixture ignores.
fn respond(method: &str, params: Option<&Value>) -> Option<Value> {
    match method {
        "initialize" => Some(json!({
            "protocolVersion": "2024-11-05",
            "capabilities": {"tools": {}},
            "serverInfo": {"name": "tcode-mcp-fixture", "version": "0.1.0"},
        })),
        "tools/list" => Some(json!({
            "tools": [
                {
                    "name": "echo",
                    "description": "Echoes its message back.",
                    "inputSchema": {
                        "type": "object",
                        "properties": {"message": {"type": "string"}},
                        "required": ["message"],
                    },
                },
                {
                    "name": "echo_env",
                    "description": "Reports the fixture token from the environment.",
                    "inputSchema": {"type": "object", "properties": {}},
                },
            ]
        })),
        "tools/call" => Some(call(params)),
        "ping" => Some(json!({})),
        _ => None,
    }
}

/// Dispatch one `tools/call`, in the MCP content-frame shape.
fn call(params: Option<&Value>) -> Value {
    let name = params
        .and_then(|p| p.get("name"))
        .and_then(Value::as_str)
        .unwrap_or_default();
    let text = match name {
        "echo" => params
            .and_then(|p| p.get("arguments"))
            .and_then(|a| a.get("message"))
            .and_then(Value::as_str)
            .unwrap_or("")
            .to_string(),
        "echo_env" => std::env::var(TOKEN_VAR).unwrap_or_else(|_| "<unset>".to_string()),
        other => {
            return json!({
                "isError": true,
                "content": [{"type": "text", "text": format!("no such tool: {other}")}],
            });
        }
    };
    json!({"content": [{"type": "text", "text": text}]})
}
