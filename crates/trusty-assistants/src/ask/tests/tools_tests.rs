//! The read-only registry and `okg_search` (#11650).
//!
//! Test: this module IS the test surface.

use std::sync::Arc;

use async_trait::async_trait;
use serde_json::{Value, json};
use trusty_code::mcp::McpToolSet;
use trusty_code::tools::{ToolExecutor, ToolRegistry, ToolResult};

use super::{Scripted, empty_servers, instance, text, tool_names};
use crate::ask::okg_search::OkgSearchTool;
use crate::ask::tools::{
    REASON_PATH_ARG, REASON_SENDS, REASON_UNLISTED, REASON_WRITES, build_registry, classify,
    is_read_only, read_only_view,
};
use crate::ask::{AskEngine, Exclusion, OKG_SEARCH_TOOL_NAME};

/// trusty-code's coding and harness tools; none may reach an ask.
const CODING_TOOLS: &[&str] = &[
    "bash",
    "read_file",
    "write_file",
    "write_files",
    "edit",
    "glob",
    "grep",
    "list_dir",
    "delegate_to_agent",
    "finish_task",
    "todo_write",
    "use_skill",
    "set_goal",
    "clear_goal",
    "recall_session",
    "search_code",
];

/// Tools a live catalog can carry, kept by the allowlist.
const KEPT: &[&str] = &[
    "mcp__trusty-search__search",
    "mcp__trusty-memory__memory_recall",
    "mcp__gworkspace__search_gmail_messages",
    "mcp__trusty-mpm__session_status",
];

/// Tools whose schema takes a local path, or whose server cannot be audited
/// (#11650 fix round). None may be registered.
const PATH_OR_UNAUDITED: &[(&str, &str)] = &[
    ("mcp__gworkspace__get_drive_file_content", REASON_WRITES),
    ("mcp__gworkspace__download_gmail_attachment", REASON_WRITES),
    ("mcp__trusty-search__search_similar", REASON_PATH_ARG),
    ("mcp__granola-notes__granola_search", REASON_UNLISTED),
    ("mcp__granola-notes__granola_get", REASON_UNLISTED),
    ("mcp__granola-notes__granola_list_recent", REASON_UNLISTED),
];

/// Tools a live catalog can carry, excluded, with the reason.
const EXCLUDED: &[(&str, &str)] = &[
    ("mcp__gworkspace__compose_email", REASON_SENDS),
    ("mcp__gworkspace__manage_events", REASON_WRITES),
    ("mcp__gworkspace__modify_gmail_messages", REASON_WRITES),
    ("mcp__trusty-mpm__session_send", REASON_SENDS),
    ("mcp__trusty-mpm__agent_delegate", REASON_SENDS),
    ("mcp__trusty-memory__memory_remember", REASON_WRITES),
    ("mcp__trusty-memory__kg_assert", REASON_WRITES),
    ("mcp__trusty-memory__memory_send_message", REASON_SENDS),
    ("mcp__trusty-search__chat", REASON_SENDS),
    ("mcp__trusty-search__reindex", REASON_WRITES),
    ("mcp__trusty-search__delete_index", REASON_WRITES),
    // A server the allowlist does not name, and a renamed one.
    ("mcp__example-memory__recall", REASON_UNLISTED),
    ("mcp__search__search", REASON_UNLISTED),
    ("mcp__gworkspace__ping", REASON_UNLISTED),
    ("mcp__trusty-mpm__console_metrics", REASON_UNLISTED),
];

fn catalog() -> Vec<String> {
    KEPT.iter()
        .copied()
        .chain(EXCLUDED.iter().chain(PATH_OR_UNAUDITED).map(|(t, _)| *t))
        .map(str::to_string)
        .collect()
}

#[test]
fn the_catalog_split_lists_every_exclusion() {
    let (kept, excluded) = classify(&catalog());
    assert_eq!(kept, KEPT);
    let expected: Vec<Exclusion> = EXCLUDED
        .iter()
        .chain(PATH_OR_UNAUDITED)
        .map(|(tool, reason)| Exclusion {
            tool: tool.to_string(),
            reason,
        })
        .collect();
    assert_eq!(excluded, expected);
}

#[test]
fn path_argument_tools_are_never_registered() {
    let mut scratch = ToolRegistry::new();
    for (name, _) in PATH_OR_UNAUDITED {
        assert!(!is_read_only(name), "{name} is on the allowlist");
        scratch.register(Arc::new(Named(name.to_string())));
    }
    let loaded: Vec<String> = PATH_OR_UNAUDITED
        .iter()
        .map(|(t, _)| t.to_string())
        .collect();
    let (view, excluded) = read_only_view(&scratch, &loaded);
    assert!(tool_names(&view).is_empty(), "{:?}", tool_names(&view));
    let reasons: Vec<(&str, &str)> = excluded
        .iter()
        .map(|e| (e.tool.as_str(), e.reason))
        .collect();
    assert_eq!(reasons, PATH_OR_UNAUDITED);
}

/// A stand-in for a bridged MCP tool; only its name matters here.
struct Named(String);

#[async_trait]
impl ToolExecutor for Named {
    fn name(&self) -> &str {
        &self.0
    }
    fn schema(&self) -> Value {
        json!({ "type": "function", "function": { "name": self.0, "parameters": {} } })
    }
    async fn execute(&self, _args: Value) -> ToolResult {
        ToolResult::ok("")
    }
}

#[test]
fn gating_drops_write_and_send_tools_from_a_registry() {
    let mut scratch = ToolRegistry::new();
    for name in catalog()
        .into_iter()
        .chain(CODING_TOOLS.iter().map(|t| t.to_string()))
    {
        scratch.register(Arc::new(Named(name)));
    }
    let (view, _) = read_only_view(&scratch, &catalog());
    let mut expected: Vec<String> = KEPT.iter().map(|t| t.to_string()).collect();
    expected.sort();
    assert_eq!(tool_names(&view), expected);
}

#[tokio::test]
async fn the_registry_holds_no_coding_write_or_send_tool() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let servers = empty_servers(&tmp);

    let set = McpToolSet::load_at(&servers, home.path()).await;
    let tools = build_registry(&home.okg_dir(), &set);
    assert_eq!(tool_names(&tools.registry), vec![OKG_SEARCH_TOOL_NAME]);

    // What the model is actually offered on an ask.
    let llm = Arc::new(Scripted::new(vec![text("ok")]));
    AskEngine::new(home, llm.clone())
        .with_mcp_config(servers)
        .ask("hello")
        .await
        .unwrap();
    let offered: Vec<String> = llm.requests()[0]
        .tools
        .iter()
        .flatten()
        .map(|t| t.function.name.clone())
        .collect();
    assert_eq!(offered, vec![OKG_SEARCH_TOOL_NAME]);
    for name in &offered {
        assert!(!CODING_TOOLS.contains(&name.as_str()), "{name}");
        assert!(!EXCLUDED.iter().any(|(t, _)| t == name), "{name}");
    }
}

#[tokio::test]
async fn okg_search_finds_lines_under_the_okg_only() {
    let tmp = tempfile::tempdir().unwrap();
    let home = instance(&tmp);
    let okg = home.okg_dir();
    std::fs::create_dir_all(okg.join("people")).unwrap();
    std::fs::write(okg.join("people/jane.md"), "# Jane\nCFO is Jane Doe.\n").unwrap();
    std::fs::write(okg.join("people/skip.bin"), "CFO in a binary file\n").unwrap();
    let outside = tmp.path().join("outside");
    std::fs::create_dir_all(&outside).unwrap();
    std::fs::write(outside.join("leak.md"), "CFO outside the OKG\n").unwrap();
    #[cfg(unix)]
    std::os::unix::fs::symlink(&outside, okg.join("link")).unwrap();

    let tool = OkgSearchTool::new(&okg);
    match tool.execute(json!({ "query": "cfo" })).await {
        ToolResult::Success { text, .. } => {
            assert_eq!(text, "people/jane.md:2: CFO is Jane Doe.");
        }
        other => panic!("{other:?}"),
    }
    match tool.execute(json!({ "query": "nobody" })).await {
        ToolResult::Success { text, .. } => assert!(text.starts_with("no matches")),
        other => panic!("{other:?}"),
    }
}

#[tokio::test]
async fn okg_search_rejects_an_empty_query() {
    let tmp = tempfile::tempdir().unwrap();
    let tool = OkgSearchTool::new(instance(&tmp).okg_dir());
    for args in [json!({}), json!({ "query": "  " })] {
        assert!(tool.execute(args).await.is_error());
    }
    let missing = OkgSearchTool::new(tmp.path().join("no-such-okg"));
    assert!(missing.execute(json!({ "query": "x" })).await.is_error());
}

/// #11650 item 5: an unreadable path is reported; with no hits the result is
/// an error, never "no matches".
#[tokio::test]
async fn okg_search_reports_unreadable_paths() {
    let tmp = tempfile::tempdir().unwrap();
    let okg = instance(&tmp).okg_dir();
    std::fs::create_dir_all(&okg).unwrap();
    std::fs::write(okg.join("bad.md"), b"CFO \xff\xfe not utf-8\n").unwrap();
    let tool = OkgSearchTool::new(&okg);

    let result = tool.execute(json!({ "query": "cfo" })).await;
    match &result {
        ToolResult::Error { .. } => {}
        other => panic!("an unread file must not read as no matches: {other:?}"),
    }
    let text = format!("{result:?}");
    assert!(text.contains("bad.md"), "{text}");
    assert!(!text.contains("no matches in this"), "{text}");

    // With a hit elsewhere the hits come back, plus a note naming the path.
    std::fs::write(okg.join("good.md"), "CFO is Jane.\n").unwrap();
    match tool.execute(json!({ "query": "cfo" })).await {
        ToolResult::Success { text, .. } => {
            assert!(text.starts_with("good.md:1: CFO is Jane."), "{text}");
            assert!(text.contains("could not be read: bad.md"), "{text}");
        }
        other => panic!("{other:?}"),
    }
}

/// #11650 item 5: the walk reads at most `MAX_FILES_SCANNED` files and says
/// it stopped.
#[tokio::test]
async fn okg_search_stops_at_the_scan_budget() {
    use crate::ask::okg_search::MAX_FILES_SCANNED;

    let tmp = tempfile::tempdir().unwrap();
    let okg = instance(&tmp).okg_dir();
    std::fs::create_dir_all(&okg).unwrap();
    for i in 0..MAX_FILES_SCANNED {
        std::fs::write(okg.join(format!("a{i:05}.md")), "filler\n").unwrap();
    }
    // Sorted last, so only a walk past the budget would find it.
    std::fs::write(okg.join("z-last.md"), "CFO is Jane.\n").unwrap();

    match OkgSearchTool::new(&okg)
        .execute(json!({ "query": "cfo" }))
        .await
    {
        ToolResult::Success { text, .. } => {
            assert!(text.starts_with("no matches"), "{text}");
            assert!(
                text.contains(&format!("stopped after {MAX_FILES_SCANNED} files")),
                "{text}"
            );
        }
        other => panic!("{other:?}"),
    }
}
