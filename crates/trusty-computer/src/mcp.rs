//! The `trusty-computer-mcp` server: 30 static `computer_*` tools over stdio JSON-RPC.
//!
//! Why: agents speak MCP; the broker speaks newline JSON on `agent.sock`.
//! This is a thin translator with a fixed tool list, so the exposed surface
//! is exactly plan section 5 and nothing a broker change could widen.
//! What: [`tools`] is the static schema (`mcp_tools.json`). [`McpServer`]
//! answers `initialize`, `ping`, `tools/list` and `tools/call`; it keeps one
//! `agent.sock` connection (the broker binds a redeemed code to it) and the
//! grant id `computer_connect` returned. Each call carries a `request_id`,
//! a uuid4 when the caller gave none. A broker refusal is an MCP tool error
//! (`isError: true`) whose text and `structuredContent` carry its `code`.
//! `computer_screenshot` (#11694) answers an MCP `image` part plus its metadata.
//! `computer_invoke_menu` (#11695) maps to the driver's `invoke_menu`.
//! `computer_click_at` and `computer_drag` (#11696) map to the broker's
//! window-bounded coordinate tools. `computer_focus`, `computer_move_window`
//! and `computer_resize_window` (#11696 Phase B) map to the broker's window tools;
//! `computer_clipboard_read` and `computer_clipboard_write` (#11696 Phase C) to
//! its clipboard tools; `computer_quit` (#11696 Phase D) to its `quit_app`.
//! Test: `tools_list_is_the_plan_table`, `mcp_smoke_against_a_fake_broker`,
//! `screenshot_result_is_an_image_part`.

use std::io::{self, BufRead, BufReader, Write};
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::sync::OnceLock;
use std::time::Duration;

use serde_json::{Map, Value, json};

use crate::consent;

/// The MCP protocol version answered when the client names none.
pub const PROTOCOL_VERSION: &str = "2025-06-18";

const TOOLS_JSON: &str = include_str!("mcp_tools.json");

/// The static tool list (empty only if the embedded JSON were malformed,
/// which `tools_list_is_the_plan_table` rules out).
pub fn tools() -> &'static [Value] {
    static TOOLS: OnceLock<Vec<Value>> = OnceLock::new();
    TOOLS.get_or_init(|| serde_json::from_str(TOOLS_JSON).unwrap_or_default())
}

/// The cua-driver tool a `computer_*` call tool maps to; `None` for broker ops.
pub fn driver_tool(name: &str) -> Option<&'static str> {
    Some(match name {
        "computer_apps" => "list_apps",
        "computer_windows" => "list_windows",
        "computer_read" => "get_window_state",
        "computer_verify" => "verify_state",
        "computer_screenshot" => crate::screenshot::SCREENSHOT_TOOL,
        "computer_click" => "click",
        "computer_double_click" => "double_click",
        "computer_right_click" => "right_click",
        "computer_scroll" => "scroll",
        "computer_type" => "type_text",
        "computer_set_value" => "set_value",
        "computer_key" => "press_key",
        "computer_hotkey" => "hotkey",
        "computer_launch" => "launch_app",
        "computer_invoke_menu" => "invoke_menu",
        "computer_click_at" => crate::pointer::CLICK_AT_TOOL,
        "computer_drag" => crate::pointer::DRAG_TOOL,
        "computer_focus" => crate::window_mgmt::FOCUS_TOOL,
        "computer_move_window" => crate::window_mgmt::MOVE_TOOL,
        "computer_resize_window" => crate::window_mgmt::RESIZE_TOOL,
        "computer_clipboard_read" => crate::clipboard::READ_TOOL,
        "computer_clipboard_write" => crate::clipboard::WRITE_TOOL,
        "computer_quit" => crate::app_quit::QUIT_TOOL,
        _ => return None,
    })
}

/// The argument names `name`'s schema declares; `None` for an unknown tool.
///
/// Why: an argument outside the schema (`snapshot_id`, `x`, `modifiers`)
/// must never reach the broker, so the schema is the allow list (#11460).
/// Test: `undeclared_arguments_are_refused`.
pub fn declared_args(name: &str) -> Option<Vec<&'static str>> {
    let tool = tools().iter().find(|t| t["name"] == name)?;
    let props = tool.pointer("/inputSchema/properties")?.as_object()?;
    Some(props.keys().map(String::as_str).collect())
}

/// The broker op of a non-call tool.
fn broker_op(name: &str) -> Option<&'static str> {
    Some(match name {
        "computer_connect" => "connect",
        "computer_status" => "status",
        "computer_release" => "release",
        "computer_disconnect" => "disconnect",
        "computer_grants" => "grants",
        "computer_wait" => "wait",
        "computer_refresh" => "refresh",
        _ => return None,
    })
}

fn tool_error(code: &str, detail: &str) -> Value {
    json!({"content": [{"type": "text", "text": format!("{code}: {detail}")}],
        "structuredContent": {"code": code, "error": detail}, "isError": true})
}

/// The MCP answer to a broker screenshot result: an `image` part, then its metadata.
///
/// A result without a string `image` (a journal replay) answers as text only.
/// Test: `screenshot_result_is_an_image_part`.
pub fn image_result(mut result: Value) -> Value {
    let image = result.as_object_mut().and_then(|r| r.remove("image"));
    let mut content = Vec::new();
    if let Some(Value::String(data)) = image {
        content.push(json!({"type": "image", "data": data,
            "mimeType": result["mime_type"].clone()}));
    }
    content.push(json!({"type": "text", "text": result.to_string()}));
    json!({"content": content, "structuredContent": result, "isError": false})
}

/// One MCP session over one broker connection.
pub struct McpServer {
    state: PathBuf,
    conn: Option<(UnixStream, BufReader<UnixStream>)>,
    grant: Option<String>,
}

impl McpServer {
    /// A server for the broker under `state`; connects on first use.
    pub fn new(state: PathBuf) -> McpServer {
        McpServer {
            state,
            conn: None,
            grant: None,
        }
    }

    fn broker(&mut self, request: &Value) -> io::Result<Value> {
        if self.conn.is_none() {
            let s = UnixStream::connect(self.state.join("agent.sock"))?;
            // connect may wait up to 20 s for the lease; the driver call up to 30 s.
            s.set_read_timeout(Some(Duration::from_secs(60)))?;
            let r = BufReader::new(s.try_clone()?);
            self.conn = Some((s, r));
        }
        match self.conn.as_mut() {
            Some((w, r)) => {
                writeln!(w, "{request}").and_then(|()| w.flush())?;
                let mut line = String::new();
                if r.read_line(&mut line)? == 0 {
                    return Err(io::Error::new(
                        io::ErrorKind::UnexpectedEof,
                        "broker closed",
                    ));
                }
                serde_json::from_str(&line).map_err(|e| io::Error::other(e.to_string()))
            }
            None => Err(io::Error::other("no broker connection")),
        }
    }

    /// Answer one `tools/call`.
    ///
    /// Why: one place maps a tool to a broker request and a refusal to an MCP error.
    /// Test: `mcp_smoke_against_a_fake_broker`.
    pub fn call(&mut self, name: &str, args: &Map<String, Value>) -> Value {
        let mut args = args.clone();
        let rid = match args.remove("request_id") {
            Some(Value::String(r)) if !r.is_empty() => r,
            Some(_) => return tool_error("bad_request", "request_id must be a non-empty string"),
            None => match consent::uuid4() {
                Ok(r) => r,
                Err(e) => return tool_error("random_failed", &e.to_string()),
            },
        };
        let declared = declared_args(name).unwrap_or_default();
        if let Some(extra) = args.keys().find(|k| !declared.contains(&k.as_str())) {
            return tool_error("bad_args", &format!("{name} takes no argument {extra}"));
        }
        let mut req = match (broker_op(name), driver_tool(name)) {
            (Some(op), _) => {
                let mut req = args.clone();
                req.insert("op".into(), json!(op));
                req
            }
            (None, Some(tool)) => {
                let mut req = Map::new();
                req.insert("op".into(), json!("call"));
                req.insert("tool".into(), json!(tool));
                req.insert("args".into(), Value::Object(args));
                req
            }
            (None, None) => return tool_error("unknown_tool", name),
        };
        req.insert("request_id".into(), json!(rid));
        if name != "computer_connect" && name != "computer_status" && name != "computer_grants" {
            match &self.grant {
                Some(g) => {
                    req.insert("grant_id".into(), json!(g));
                }
                None if name == "computer_release" || name == "computer_disconnect" => {}
                None => return tool_error("not_connected", "call computer_connect first"),
            }
        }
        let reply = match self.broker(&Value::Object(req)) {
            Ok(r) => r,
            Err(e) => {
                // The binding lived on that connection; a new one needs a new code.
                self.conn = None;
                self.grant = None;
                return tool_error("broker_unreachable", &e.to_string());
            }
        };
        if reply["ok"] != json!(true) {
            let code = reply["code"].as_str().unwrap_or("broker_error");
            return tool_error(code, reply["error"].as_str().unwrap_or(""));
        }
        let result = reply.get("result").cloned().unwrap_or(Value::Null);
        match name {
            "computer_connect" => self.grant = result["grant"].as_str().map(str::to_owned),
            "computer_disconnect" => self.grant = None,
            _ => {}
        }
        if name == "computer_screenshot" {
            return image_result(result);
        }
        let mut out = json!({"content": [{"type": "text", "text": result.to_string()}],
            "isError": false});
        if result.is_object() {
            out["structuredContent"] = result;
        }
        out
    }

    /// Answer one JSON-RPC message; `None` for a notification.
    pub fn handle(&mut self, msg: &Value) -> Option<Value> {
        let id = msg.get("id").cloned();
        let method = msg.get("method").and_then(Value::as_str).unwrap_or("");
        let params = msg.get("params").cloned().unwrap_or_else(|| json!({}));
        let result = match method {
            "initialize" => Ok(json!({
                "protocolVersion": params["protocolVersion"].as_str().unwrap_or(PROTOCOL_VERSION),
                "capabilities": {"tools": {}},
                "serverInfo": {"name": "trusty-computer-mcp", "version": env!("CARGO_PKG_VERSION")}})),
            "ping" => Ok(json!({})),
            "tools/list" => Ok(json!({"tools": tools()})),
            "tools/call" => {
                let name = params["name"].as_str().unwrap_or("");
                let empty = Map::new();
                let args = params["arguments"].as_object().unwrap_or(&empty);
                Ok(self.call(name, args))
            }
            m if m.starts_with("notifications/") => return None,
            _ => Err(json!({"code": -32601, "message": format!("method not found: {method}")})),
        };
        let id = id?;
        Some(match result {
            Ok(r) => json!({"jsonrpc": "2.0", "id": id, "result": r}),
            Err(e) => json!({"jsonrpc": "2.0", "id": id, "error": e}),
        })
    }

    /// Serve newline-delimited JSON-RPC from `input` to `output` until EOF.
    pub fn serve(&mut self, input: impl BufRead, mut output: impl Write) -> io::Result<()> {
        for line in input.lines() {
            let line = line?;
            if line.trim().is_empty() {
                continue;
            }
            let reply = match serde_json::from_str::<Value>(&line) {
                Ok(msg) => self.handle(&msg),
                Err(e) => Some(json!({"jsonrpc": "2.0", "id": null,
                    "error": {"code": -32700, "message": e.to_string()}})),
            };
            if let Some(reply) = reply {
                writeln!(output, "{reply}")?;
                output.flush()?;
            }
        }
        Ok(())
    }
}
