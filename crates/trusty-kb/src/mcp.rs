//! MCP dispatch seam — routes JSON-RPC methods to the store.
//!
//! Why: mirrors `tagent mcp-serve` (PR #3771): a single [`dispatch_with`] fn that
//! unit tests drive without real stdin/stdout, static `tools/list`, and
//! `tools/call` results wrapped in the `{content:[{type:"text",text}],isError}`
//! envelope (a failing tool is a normal result with `isError:true`, not a
//! JSON-RPC error). One service instance serves EVERY assistant's tree — the
//! root is resolved per call from the tool arguments, never pinned.
//!
//! What: [`ServerConfig`] carries the service-level [`Roots`] + [`Profile`].
//! [`dispatch_with`] handles `initialize`/`tools/list`/`tools/call`/`ping` and
//! suppresses notifications; [`call_tool`] resolves the per-call root and routes
//! to the [`KbStore`] method (or the service-scoped `kb_list_trees`).
//!
//! Test: `dispatch_initialize_reports_server`, `tool_list_names_are_stable`,
//! `call_status_returns_envelope`, `unknown_tool_is_soft_error`.

use serde::Serialize;
use serde_json::{Value, json};
use trusty_common::mcp::{Request, Response, error_codes, initialize_response, run_stdio_loop};

use crate::roots::{Roots, assert_within};
use crate::schema::Profile;
use crate::store::KbStore;
use crate::tooldefs::tool_definitions;

/// Service-level configuration shared across every tool call.
#[derive(Debug, Clone)]
pub struct ServerConfig {
    /// Root resolution defaults (knowledge dir + default root).
    pub roots: Roots,
    /// The active schema profile.
    pub profile: Profile,
}

impl ServerConfig {
    /// Build a config from a [`Roots`] with the default profile.
    pub fn new(roots: Roots) -> Self {
        Self {
            roots,
            profile: Profile::default_profile(),
        }
    }
}

/// Run the shared stdio MCP loop with this config until EOF.
///
/// Why: installs a stderr tracing subscriber BEFORE the loop (so diagnostics
/// never pollute the JSON-RPC stdout stream) then hands a config-capturing
/// closure to [`run_stdio_loop`].
/// What: the closure clones the config per request and calls [`dispatch_with`].
/// Test: exercised by the live binary smoke.
pub async fn serve(config: ServerConfig) -> anyhow::Result<()> {
    let filter = tracing_subscriber::EnvFilter::try_from_default_env()
        .unwrap_or_else(|_| tracing_subscriber::EnvFilter::new("info"));
    let _ = tracing_subscriber::fmt()
        .with_env_filter(filter)
        .with_writer(std::io::stderr)
        .try_init();

    run_stdio_loop(move |req| {
        let config = config.clone();
        async move { dispatch_with(config, req).await }
    })
    .await
}

/// Handle one JSON-RPC request and produce its response.
///
/// Why/What/Test: see the module doc.
pub async fn dispatch_with(config: ServerConfig, req: Request) -> Response {
    let id = req.id.clone();
    match req.method.as_str() {
        "initialize" => Response::ok(
            id,
            initialize_response(crate::SERVER_NAME, env!("CARGO_PKG_VERSION"), None),
        ),
        "notifications/initialized" | "notifications/cancelled" => Response::suppressed(),
        "tools/list" => Response::ok(id, tool_definitions()),
        "ping" => Response::ok(id, json!({})),
        "tools/call" => {
            let params = req.params.clone().unwrap_or(Value::Null);
            let name = params.get("name").and_then(Value::as_str).unwrap_or("");
            let args = params.get("arguments").cloned().unwrap_or(Value::Null);
            call_tool(&config, name, &args).into_response(id)
        }
        other => Response::err(
            id,
            error_codes::METHOD_NOT_FOUND,
            format!("Method not found: {other}"),
        ),
    }
}

/// Outcome of a `tools/call`: text payload + error flag.
pub struct ToolOutcome {
    text: String,
    is_error: bool,
}

impl ToolOutcome {
    fn ok<T: Serialize>(value: &T) -> Self {
        Self {
            text: serde_json::to_string_pretty(value)
                .unwrap_or_else(|e| format!("serialize error: {e}")),
            is_error: false,
        }
    }
    fn err(msg: impl Into<String>) -> Self {
        Self {
            text: msg.into(),
            is_error: true,
        }
    }
    fn into_response(self, id: Option<Value>) -> Response {
        Response::ok(
            id,
            json!({ "content": [{ "type": "text", "text": self.text }], "isError": self.is_error }),
        )
    }
}

/// Route a named tool to the store, resolving its per-call root first.
///
/// Why/What/Test: see the module doc.
fn call_tool(config: &ServerConfig, name: &str, args: &Value) -> ToolOutcome {
    match run_tool(config, name, args) {
        Ok(outcome) => outcome,
        Err(e) => ToolOutcome::err(format!("{name} failed: {e}")),
    }
}

/// The fallible body of [`call_tool`] — any `Err` becomes an `isError` envelope.
fn run_tool(config: &ServerConfig, name: &str, args: &Value) -> anyhow::Result<ToolOutcome> {
    if name == "kb_list_trees" {
        return Ok(ToolOutcome::ok(&list_trees(config)?));
    }
    if name == "kb_convert_tree" {
        return convert_tool(config, args);
    }
    let store = resolve_store(config, args)?;
    let now = now_iso();
    match name {
        "kb_status" => Ok(ToolOutcome::ok(&store.status()?)),
        "kb_ensure_structure" => Ok(ToolOutcome::ok(&store.ensure_structure()?)),
        "kb_validate" => Ok(ToolOutcome::ok(&store.validate()?)),
        "kb_list" => Ok(ToolOutcome::ok(
            &store.list(arg_str(args, "collection"), arg_str(args, "filter"))?,
        )),
        "kb_get_entity" => {
            let (coll, ent) = required_pair(args)?;
            match store.get_entity(coll, ent)? {
                Some(e) => Ok(ToolOutcome::ok(&json!({
                    "collection": coll,
                    "frontmatter": frontmatter_json(&e),
                    "body": e.body,
                }))),
                None => Ok(ToolOutcome::err(format!("entity not found: {coll}/{ent}"))),
            }
        }
        "kb_put_entity" => {
            let (coll, ent) = required_pair(args)?;
            let fm = match args.get("frontmatter").cloned() {
                Some(v) if !v.is_null() => serde_yaml::to_value(v)?,
                _ => crate::put::empty_map(),
            };
            let body = arg_str(args, "body");
            let merge = args.get("merge").and_then(Value::as_bool).unwrap_or(false);
            Ok(ToolOutcome::ok(
                &store.put_entity(coll, ent, fm, body, merge, &now)?,
            ))
        }
        other => Ok(ToolOutcome::err(format!("unknown tool: {other}"))),
    }
}

/// Build a [`KbStore`] over the per-call resolved, confinement-checked root.
fn resolve_store(config: &ServerConfig, args: &Value) -> anyhow::Result<KbStore> {
    let root = config
        .roots
        .resolve(arg_str(args, "root"), arg_str(args, "agent"));
    assert_within(&config.roots.knowledge_dir, &root).or_else(|_| {
        // An explicit `root` outside knowledge_dir is allowed only when it IS
        // the default_root (service owner's tree); otherwise reject traversal.
        if root == config.roots.default_root {
            Ok(())
        } else {
            anyhow::bail!("root {} is outside the knowledge directory", root.display())
        }
    })?;
    Ok(KbStore::new(root, config.profile.clone()))
}

/// The `kb_convert_tree` arm (its own root arg + mode).
fn convert_tool(config: &ServerConfig, args: &Value) -> anyhow::Result<ToolOutcome> {
    let Some(source) = arg_str(args, "source_root") else {
        return Ok(ToolOutcome::err("missing required 'source_root'"));
    };
    let report_only = arg_str(args, "mode")
        .map(|m| m != "in_place")
        .unwrap_or(true);
    let store = KbStore::new(std::path::PathBuf::from(source), config.profile.clone());
    Ok(ToolOutcome::ok(
        &store.convert_tree(report_only, &now_iso())?,
    ))
}

/// A tree enumeration row for `kb_list_trees`.
#[derive(Debug, Serialize)]
struct TreeInfo {
    name: String,
    path: String,
    entity_count: usize,
    last_updated: Option<String>,
}

/// Enumerate the trees under the knowledge directory.
fn list_trees(config: &ServerConfig) -> anyhow::Result<Vec<TreeInfo>> {
    let dir = &config.roots.knowledge_dir;
    let mut out = Vec::new();
    if !dir.is_dir() {
        return Ok(out);
    }
    let mut names: Vec<String> = std::fs::read_dir(dir)?
        .filter_map(|e| e.ok())
        .filter(|e| e.path().is_dir())
        .map(|e| e.file_name().to_string_lossy().to_string())
        .filter(|n| !n.starts_with('.'))
        .collect();
    names.sort();
    for name in names {
        let store = KbStore::new(config.roots.tree_path(&name), config.profile.clone());
        let mut count = 0;
        let mut last: Option<String> = None;
        for coll in store.collection_dirs_on_disk()? {
            for (_slug, path) in store.entity_files(&coll)? {
                if let Some(e) = store.read_entity_at(&path)? {
                    count += 1;
                    if let Some(ts) = e.get_str("updated").or_else(|| e.get_str("created"))
                        && last.as_deref().map(|l| ts > l).unwrap_or(true)
                    {
                        last = Some(ts.to_string());
                    }
                }
            }
        }
        out.push(TreeInfo {
            name: name.clone(),
            path: store.root.to_string_lossy().to_string(),
            entity_count: count,
            last_updated: last,
        });
    }
    Ok(out)
}

/// Frontmatter of an entity as JSON (for tool output).
fn frontmatter_json(entity: &crate::entity::Entity) -> Value {
    serde_json::to_value(&entity.frontmatter).unwrap_or(Value::Null)
}

/// Current UTC timestamp as `YYYY-MM-DDTHH:MM:SSZ`.
fn now_iso() -> String {
    chrono::Utc::now().format("%Y-%m-%dT%H:%M:%SZ").to_string()
}

fn arg_str<'a>(args: &'a Value, key: &str) -> Option<&'a str> {
    args.get(key)
        .and_then(Value::as_str)
        .filter(|s| !s.is_empty())
}

/// Extract the required `collection` + `name` pair, erroring if either is absent.
fn required_pair(args: &Value) -> anyhow::Result<(&str, &str)> {
    let coll =
        arg_str(args, "collection").ok_or_else(|| anyhow::anyhow!("missing 'collection'"))?;
    let name = arg_str(args, "name").ok_or_else(|| anyhow::anyhow!("missing 'name'"))?;
    Ok((coll, name))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    fn config() -> (tempfile::TempDir, ServerConfig) {
        let tmp = tempfile::tempdir().unwrap();
        let kdir = tmp.path().to_path_buf();
        let roots = Roots::new(kdir.clone(), kdir.join("bob-kb"));
        (tmp, ServerConfig::new(roots))
    }

    fn req(method: &str, id: i64, params: Option<Value>) -> Request {
        Request {
            jsonrpc: Some("2.0".into()),
            id: Some(json!(id)),
            method: method.into(),
            params,
        }
    }

    /// Why: initialize is the MCP handshake; serverInfo must identify this crate.
    /// What: asserts the pinned protocol version + server name/version.
    /// Test: self-contained.
    #[tokio::test]
    async fn dispatch_initialize_reports_server() {
        let (_t, c) = config();
        let resp = dispatch_with(c, req("initialize", 1, None)).await;
        let result = resp.result.expect("initialize result");
        assert_eq!(result["protocolVersion"], "2024-11-05");
        assert_eq!(result["serverInfo"]["name"], "trusty-kb");
    }

    /// Why: the tool surface is the whole contract; a dropped/renamed tool
    /// breaks clients.
    /// What: asserts tools/list returns exactly the eight expected tool names.
    /// Test: self-contained.
    #[tokio::test]
    async fn tool_list_names_are_stable() {
        let (_t, c) = config();
        let resp = dispatch_with(c, req("tools/list", 2, None)).await;
        let tools = resp.result.expect("tools")["tools"].clone();
        let names: Vec<&str> = tools
            .as_array()
            .unwrap()
            .iter()
            .map(|t| t["name"].as_str().unwrap())
            .collect();
        assert_eq!(
            names,
            vec![
                "kb_status",
                "kb_list_trees",
                "kb_put_entity",
                "kb_get_entity",
                "kb_list",
                "kb_ensure_structure",
                "kb_validate",
                "kb_convert_tree"
            ]
        );
    }

    /// Why: a tools/call must return the MCP text envelope with isError:false on
    /// success, and the root must resolve per-call from an explicit arg.
    /// What: calls kb_status against an explicit temp root; asserts the envelope
    /// and that the status text parses back to JSON.
    /// Test: self-contained.
    #[tokio::test]
    async fn call_status_returns_envelope() {
        let (tmp, c) = config();
        let root = tmp.path().join("bob-kb");
        let params =
            json!({ "name": "kb_status", "arguments": { "root": root.to_string_lossy() } });
        let resp = dispatch_with(c, req("tools/call", 3, Some(params))).await;
        let result = resp.result.expect("result");
        assert_eq!(result["isError"], false);
        let text = result["content"][0]["text"].as_str().unwrap();
        let parsed: Value = serde_json::from_str(text).unwrap();
        assert!(parsed["collections"].is_array());
    }

    /// Why: an unknown tool must fail SOFT (isError:true), not panic the loop.
    /// What: calls a bogus tool; asserts the soft-error envelope.
    /// Test: self-contained.
    #[tokio::test]
    async fn unknown_tool_is_soft_error() {
        let (_t, c) = config();
        let params = json!({ "name": "nope", "arguments": {} });
        let resp = dispatch_with(c, req("tools/call", 4, Some(params))).await;
        let result = resp.result.expect("result");
        assert_eq!(result["isError"], true);
        assert!(
            result["content"][0]["text"]
                .as_str()
                .unwrap()
                .contains("unknown tool")
        );
    }

    /// Why: traversal via an explicit out-of-tree root must be rejected.
    /// What: passes a root outside knowledge_dir (not the default); asserts a
    /// soft error mentioning the boundary.
    /// Test: self-contained.
    #[tokio::test]
    async fn out_of_tree_root_is_rejected() {
        let (_t, c) = config();
        let params = json!({ "name": "kb_status", "arguments": { "root": "/etc" } });
        let resp = dispatch_with(c, req("tools/call", 5, Some(params))).await;
        let result = resp.result.expect("result");
        assert_eq!(result["isError"], true);
        let _ = PathBuf::from("/etc");
    }
}
