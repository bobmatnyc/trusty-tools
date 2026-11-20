//! The stdio JSON-RPC loop of `trusty-browser-mcp`, as FastMCP 1.30 answers.
//!
//! Why: an MCP client sees Python's exact lines today; the port must give
//! the same `initialize`, `tools/list`, `tools/call` and error lines so no
//! client notices the switch (plan section 3, item 16).
//! What: [`Server::handle_line`] maps one input line to at most one output line.
//! Results are written with [`Json::dumps`] (`ensure_ascii=False`, Python
//! float repr), which is how pydantic serialises them. The `-32602` error
//! envelope is a [`trusty_mcp::Response`]; its field order matches FastMCP's.
//! A line that is not a JSON-RPC 2.0 message gets FastMCP's
//! `Internal Server Error` log notification; an id that is neither an integer
//! nor a string gets no reply.
//! Test: `tools_list_and_initialize_are_byte_identical` and the three
//! exchange replays in `tests/parity_mcp.rs`.

use std::io::{BufRead, Write};

use trusty_mcp::{Response, error_codes};

use crate::mcp_tools::{CONNECT_TOOLS, Mode, Server, WORKFLOW_TOOLS};
use crate::pyjson::Json;

/// `mcp.types.LATEST_PROTOCOL_VERSION` in mcp 1.30.0.
pub const LATEST_PROTOCOL: &str = "2025-11-25";
/// `SUPPORTED_PROTOCOL_VERSIONS` in mcp 1.30.0.
pub const SUPPORTED_PROTOCOLS: &[&str] = &["2024-11-05", "2025-03-26", "2025-06-18", "2025-11-25"];
/// FastMCP reports the `mcp` package version as the server version.
pub const SERVER_VERSION: &str = "1.30.0";
const INTERNAL_ERROR: &str = r#"{"method":"notifications/message","params":{"level":"error","logger":"mcp.server.exception_handler","data":"Internal Server Error"},"jsonrpc":"2.0"}"#;

fn id_value(id: &Json) -> serde_json::Value {
    match id {
        Json::Int(i) => serde_json::Value::from(*i),
        Json::UInt(u) => serde_json::Value::from(*u),
        Json::Str(s) => serde_json::Value::from(s.as_str()),
        _ => serde_json::Value::Null,
    }
}

fn invalid_params(id: &Json) -> String {
    let mut r = Response::err(
        Some(id_value(id)),
        error_codes::INVALID_PARAMS,
        "Invalid request parameters",
    );
    if let Some(e) = r.error.as_mut() {
        e.data = Some(serde_json::Value::from(""));
    }
    serde_json::to_string(&r).unwrap_or_default()
}

fn result_line(id: &Json, result_text: &str) -> String {
    let id = id.dumps(false).unwrap_or_default();
    format!(r#"{{"jsonrpc":"2.0","id":{id},"result":{result_text}}}"#)
}

fn result(id: &Json, result: &Json) -> String {
    result_line(id, &result.dumps(false).unwrap_or_default())
}

fn initialize(id: &Json, params: Option<&Json>) -> String {
    let Some(params @ Json::Object(_)) = params else {
        return invalid_params(id);
    };
    let version = params.get("protocolVersion").and_then(Json::as_str);
    let client = params.get("clientInfo");
    let shaped = version.is_some()
        && matches!(params.get("capabilities"), Some(Json::Object(_)))
        && client
            .and_then(|c| c.get("name"))
            .and_then(Json::as_str)
            .is_some()
        && client
            .and_then(|c| c.get("version"))
            .and_then(Json::as_str)
            .is_some();
    let Some(version) = version.filter(|_| shaped) else {
        return invalid_params(id);
    };
    let version = if SUPPORTED_PROTOCOLS.contains(&version) {
        version
    } else {
        LATEST_PROTOCOL
    };
    let text = format!(
        r#"{{"protocolVersion":{},"capabilities":{{"experimental":{{}},"prompts":{{"listChanged":false}},"resources":{{"subscribe":false,"listChanged":false}},"tools":{{"listChanged":false}}}},"serverInfo":{{"name":"trusty-browser","version":"{SERVER_VERSION}"}}}}"#,
        Json::from(version).dumps(false).unwrap_or_default()
    );
    result_line(id, &text)
}

fn call_result(outcome: Result<Json, String>) -> Json {
    let text = |t: String| Json::object([("type", Json::from("text")), ("text", Json::Str(t))]);
    match outcome {
        Ok(reply) => Json::object([
            (
                "content",
                Json::Array(vec![text(reply.dumps_indent2().unwrap_or_default())]),
            ),
            ("structuredContent", reply),
            ("isError", Json::Bool(false)),
        ]),
        Err(message) => Json::object([
            ("content", Json::Array(vec![text(message)])),
            ("isError", Json::Bool(true)),
        ]),
    }
}

impl Server {
    /// Answer one stdin line; `None` writes nothing.
    ///
    /// Test: `connect_exchange_replays_byte_identically`.
    pub fn handle_line(&mut self, line: &str) -> Option<String> {
        let msg = match Json::parse(line.as_bytes()) {
            Ok(m @ Json::Object(_)) if m.get("jsonrpc").and_then(Json::as_str) == Some("2.0") => m,
            _ => return Some(INTERNAL_ERROR.to_owned()),
        };
        let Some(method) = msg.get("method").and_then(Json::as_str) else {
            return None; // a client response or result: nothing to answer
        };
        let id = msg.get("id")?;
        if !matches!(id, Json::Int(_) | Json::UInt(_) | Json::Str(_)) {
            return None;
        }
        let params = msg.get("params").filter(|p| **p != Json::Null);
        Some(match method {
            "initialize" => initialize(id, params),
            "ping" => result_line(id, "{}"),
            "tools/list" => {
                let tools = match self.mode {
                    Mode::Connect(..) => CONNECT_TOOLS,
                    Mode::Workflow(_) => WORKFLOW_TOOLS,
                };
                result_line(id, tools.trim_end())
            }
            "resources/list" => result_line(id, r#"{"resources":[]}"#),
            "resources/templates/list" => result_line(id, r#"{"resourceTemplates":[]}"#),
            "prompts/list" => result_line(id, r#"{"prompts":[]}"#),
            "tools/call" => {
                let name = params.and_then(|p| p.get("name")).and_then(Json::as_str);
                let args = params.and_then(|p| p.get("arguments"));
                let args = match args {
                    None | Some(Json::Null) => Json::Object(Vec::new()),
                    Some(a @ Json::Object(_)) => a.clone(),
                    Some(_) => return Some(invalid_params(id)),
                };
                match name {
                    Some(name) => {
                        let name = name.to_owned();
                        result(id, &call_result(self.call_tool(&name, &args)))
                    }
                    None => invalid_params(id),
                }
            }
            _ => invalid_params(id),
        })
    }

    /// Serve stdin to stdout until EOF.
    pub fn run(&mut self, input: impl BufRead, mut output: impl Write) -> std::io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            if let Some(reply) = self.handle_line(&line) {
                output.write_all(reply.as_bytes())?;
                output.write_all(b"\n")?;
                output.flush()?;
            }
        }
        Ok(())
    }
}
