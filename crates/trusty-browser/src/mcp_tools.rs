//! The MCP tools of both modes: signatures, frames and call results.
//!
//! Why: each Python tool function (`page_tools.py`, `agent_mcp.py`) fixes
//! its parameters, the order of the fields in its `agent.sock` frame, and the
//! result FastMCP builds from its return value or exception.
//! What: [`CONNECT`] and [`WORKFLOW`] list every tool with its [`Field`]s
//! and frame layout. [`Server::call_tool`] validates, builds the frame
//! (`request_id` defaulted to a fresh uuid4, `modifiers` to `[]`, `paths`
//! read into `files`), sends it, and renders the `CallToolResult`. The
//! `tools/list` answers are static JSON captured from Python
//! (`mcp_connect_tools.json`, `mcp_workflow_tools.json`).
//! Test: `tools_list_and_initialize_are_byte_identical`,
//! `connect_exchange_replays_byte_identically` in `tests/parity_mcp.rs`.

use std::path::PathBuf;

use crate::entropy::{OsEntropy, uuid4};
use crate::mcp_args::{Default as D, Field, Kind as K, field, validate};
use crate::mcp_session::{CodeSession, WorkflowWire};
use crate::mcp_upload::{local, upload_files};
use crate::pyjson::Json;

/// Connect mode's `tools/list` result.
pub const CONNECT_TOOLS: &str = include_str!("mcp_connect_tools.json");
/// Workflow mode's `tools/list` result.
pub const WORKFLOW_TOOLS: &str = include_str!("mcp_workflow_tools.json");

/// One tool: its name, parameters, broker op and frame field order.
#[derive(Debug)]
pub struct Tool {
    /// The MCP tool name.
    pub name: &'static str,
    /// The function's parameters, in declaration order.
    pub fields: &'static [Field],
    /// The `agent.sock` op (`""` for session tools handled locally).
    pub op: &'static str,
    /// Frame field names after `op`; `files` is built from `paths`.
    pub frame: &'static [&'static str],
}

const LEASE: Field = field("lease", K::Str, D::Required);
const REF: Field = field("ref", K::Str, D::Required);
const RID: Field = field("request_id", K::OptStr, D::Null);
const CAPTURE: Field = field("capture_id", K::Str, D::Required);
const X: Field = field("x", K::Int, D::Required);
const Y: Field = field("y", K::Int, D::Required);

const fn tool(
    name: &'static str,
    fields: &'static [Field],
    op: &'static str,
    frame: &'static [&'static str],
) -> Tool {
    Tool {
        name,
        fields,
        op,
        frame,
    }
}

/// Connect mode, in Python's registration order.
pub const CONNECT: &[Tool] = &[
    tool("browser_profiles", &[], "", &[]),
    tool(
        "browser_connect",
        &[field("profile", K::OptStr, D::Null)],
        "",
        &[],
    ),
    tool("browser_disconnect", &[], "", &[]),
    tool("browser_tabs", &[], "tabs", &[]),
    tool(
        "browser_select",
        &[field("tab", K::Int, D::Required)],
        "select",
        &["tab"],
    ),
    tool("browser_switch", &[LEASE], "switch", &["lease"]),
    tool(
        "page_read",
        &[
            LEASE,
            field("scope_ref", K::OptStr, D::Null),
            field("max_chars", K::Int, D::Int(50000)),
            field("viewport_only", K::Bool, D::Bool(false)),
            field("include_text", K::Bool, D::Bool(false)),
        ],
        "page_read",
        &[
            "lease",
            "scope_ref",
            "max_chars",
            "viewport_only",
            "include_text",
        ],
    ),
    tool(
        "page_find",
        &[
            LEASE,
            field("role", K::OptStr, D::Null),
            field("name", K::OptStr, D::Null),
            field("text", K::OptStr, D::Null),
            field("within_ref", K::OptStr, D::Null),
            field("limit", K::Int, D::Int(10)),
        ],
        "page_find",
        &["lease", "role", "name", "text", "within_ref", "limit"],
    ),
    tool(
        "page_click",
        &[LEASE, REF, RID, field("dialog", K::Str, D::Str("dismiss"))],
        "page_click",
        &["lease", "ref", "request_id", "dialog"],
    ),
    tool(
        "page_hover",
        &[LEASE, REF, RID],
        "page_hover",
        &["lease", "ref", "request_id"],
    ),
    tool(
        "page_drag",
        &[LEASE, REF, field("target", K::Str, D::Required), RID],
        "page_drag",
        &["lease", "ref", "target", "request_id"],
    ),
    tool(
        "page_type",
        &[LEASE, REF, field("text", K::Str, D::Required), RID],
        "page_type",
        &["lease", "ref", "text", "request_id"],
    ),
    tool(
        "page_scroll",
        &[
            LEASE,
            RID,
            field("to", K::Str, D::Str("down")),
            field("ref", K::OptStr, D::Null),
            field("wait_ms", K::Int, D::Int(1000)),
        ],
        "page_scroll",
        &["lease", "to", "ref", "wait_ms", "request_id"],
    ),
    tool(
        "page_press",
        &[
            LEASE,
            field("key", K::Str, D::Required),
            RID,
            field("ref", K::OptStr, D::Null),
            field("modifiers", K::OptListStr, D::Null),
        ],
        "page_press",
        &["lease", "ref", "key", "modifiers", "request_id"],
    ),
    tool(
        "page_select",
        &[LEASE, REF, field("value", K::Str, D::Required), RID],
        "page_select",
        &["lease", "ref", "value", "request_id"],
    ),
    tool(
        "page_navigate",
        &[LEASE, field("url", K::Str, D::Required), RID],
        "page_navigate",
        &["lease", "url", "request_id"],
    ),
    tool(
        "page_back",
        &[LEASE, RID],
        "page_back",
        &["lease", "request_id"],
    ),
    tool(
        "page_forward",
        &[LEASE, RID],
        "page_forward",
        &["lease", "request_id"],
    ),
    tool(
        "page_reload",
        &[LEASE, RID],
        "page_reload",
        &["lease", "request_id"],
    ),
    tool(
        "page_upload",
        &[LEASE, REF, field("paths", K::ListStr, D::Required), RID],
        "page_upload",
        &["lease", "ref", "files", "request_id"],
    ),
    tool("page_downloads", &[LEASE], "page_downloads", &["lease"]),
    // #11208: open a tab already leased to this session; close only a leased one.
    tool(
        "browser_open_tab",
        &[field("url", K::Str, D::Required), RID],
        "open_tab",
        &["url", "request_id"],
    ),
    tool(
        "browser_close_tab",
        &[field("tab", K::Int, D::Required), RID],
        "close_tab",
        &["tab", "request_id"],
    ),
    // #11211: a PNG with a capture_id; coordinate input in that image's pixels.
    tool(
        "page_screenshot",
        &[LEASE, field("ref", K::OptStr, D::Null)],
        "page_screenshot",
        &["lease", "ref"],
    ),
    tool(
        "page_click_at",
        &[LEASE, CAPTURE, X, Y, RID],
        "page_click_at",
        &["lease", "capture_id", "x", "y", "request_id"],
    ),
    tool(
        "page_type_at",
        &[
            LEASE,
            CAPTURE,
            X,
            Y,
            field("text", K::Str, D::Required),
            RID,
        ],
        "page_type_at",
        &["lease", "capture_id", "x", "y", "text", "request_id"],
    ),
    tool("browser_release", &[LEASE], "release", &["lease"]),
];

/// Workflow mode (`add_tab_tools`).
pub const WORKFLOW: &[Tool] = &[
    tool("browser_claim", &[], "claim", &[]),
    tool(
        "browser_snapshot",
        &[LEASE, RID],
        "snapshot",
        &["lease", "request_id"],
    ),
    tool(
        "browser_click",
        &[LEASE, field("marker", K::Str, D::Required), RID],
        "click",
        &["lease", "request_id", "marker"],
    ),
    tool("browser_release", &[LEASE], "release", &["lease"]),
];

/// Which agent.sock client the server holds.
#[derive(Debug)]
pub enum Mode {
    /// `run_codes`: a session opened by `browser_connect`, and the upload root.
    Connect(CodeSession, Option<PathBuf>),
    /// `run`: the workflow ticket's connection.
    Workflow(WorkflowWire),
}

/// The server state behind `tools/call`.
#[derive(Debug)]
pub struct Server {
    /// The mode and its client.
    pub mode: Mode,
}

/// A tool's outcome: `Ok` is the structured result, `Err` the error text.
pub type Outcome = Result<Json, String>;

fn error_text(tool: &str, frame: &Json) -> String {
    // Python `Refused`: one compact ensure_ascii JSON frame.
    format!(
        "Error executing tool {tool}: {}",
        frame.dumps(true).unwrap_or_default()
    )
}

impl Server {
    /// The tool table of this mode.
    pub fn tools(&self) -> &'static [Tool] {
        match self.mode {
            Mode::Connect(..) => CONNECT,
            Mode::Workflow(_) => WORKFLOW,
        }
    }

    /// Run one tool call. `Err` carries the full `CallToolResult` error text.
    ///
    /// Why/What: see the module doc. `Unknown tool: <name>` has no prefix,
    /// like FastMCP's `ToolError`.
    /// Test: `connect_exchange_replays_byte_identically`.
    pub fn call_tool(&mut self, name: &str, args: &Json) -> Outcome {
        let Some(tool) = self.tools().iter().find(|t| t.name == name) else {
            return Err(format!("Unknown tool: {name}"));
        };
        let values = validate(tool.name, tool.fields, args)
            .map_err(|e| format!("Error executing tool {name}: {e}"))?;
        let get = |k: &str| values.iter().find(|(n, _)| *n == k).map(|(_, v)| v.clone());
        match &mut self.mode {
            Mode::Connect(session, root) => {
                let result = match tool.name {
                    "browser_profiles" => session.profiles(),
                    "browser_disconnect" => Ok(session.disconnect()),
                    "browser_connect" => match get("profile") {
                        Some(Json::Str(p)) if p.is_empty() => Err(local(
                            "InvalidRequest",
                            "profile must be a non-empty string",
                        )),
                        Some(Json::Str(p)) => session.connect(Some(&p)),
                        _ => session.connect(None),
                    },
                    _ => match frame_fields(tool, &get, root.as_deref()) {
                        Ok(fields) => session.call(tool.op, fields),
                        Err(refusal) => Err(refusal),
                    },
                };
                result.map_err(|frame| error_text(name, &frame))
            }
            Mode::Workflow(wire) => {
                let fields = frame_fields(tool, &get, None).map_err(|f| error_text(name, &f))?;
                wire.call(tool.op, fields)
                    .map_err(|e| format!("Error executing tool {name}: {e}"))
            }
        }
    }
}

/// The frame fields after `op`, in Python's keyword order.
fn frame_fields(
    tool: &Tool,
    get: &dyn Fn(&str) -> Option<Json>,
    root: Option<&std::path::Path>,
) -> Result<Vec<(&'static str, Json)>, Json> {
    let mut out = Vec::new();
    for key in tool.frame {
        let value = match *key {
            "files" => upload_files(&get("paths").unwrap_or(Json::Null), root)?,
            "request_id" => match get("request_id") {
                Some(Json::Null) | None => Json::Str(uuid4(&OsEntropy)),
                Some(v) => v,
            },
            "modifiers" => match get("modifiers") {
                Some(Json::Null) | None => Json::Array(Vec::new()),
                Some(v) => v,
            },
            k => get(k).unwrap_or(Json::Null),
        };
        out.push((*key, value));
    }
    Ok(out)
}
