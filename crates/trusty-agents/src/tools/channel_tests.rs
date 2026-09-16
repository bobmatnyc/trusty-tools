//! The merged `channel` tool and the self-configuration helpers it owns
//! (#7609 slices 5 and 7).
//!
//! Why: the merge is only real if one tool covers both scopes' list/get/set,
//! and only safe if every write takes the channel-write gate. Slice 7 deleted
//! the `listener_config` alias, so what is asserted here now is that its NAME
//! is still refused to an external executor and that the capability grant and
//! the prompt copy name the surviving tool.
//! What: schema assertions (pure, environment-independent) plus the gated and
//! credentialed write paths.
//! Test: this module IS the test.

use crate::tools::channel::{
    ChannelTool, is_reserved_name, register_external, self_configuration_patterns,
    wake_filter_context,
};
use crate::tools::traits::{ToolExecutor, ToolResult};
use async_trait::async_trait;
use serde_json::{Value, json};

fn actions(tool: &dyn ToolExecutor) -> Vec<String> {
    tool.schema()["function"]["parameters"]["properties"]["action"]["enum"]
        .as_array()
        .map(|values| {
            values
                .iter()
                .filter_map(|v| v.as_str().map(str::to_owned))
                .collect()
        })
        .unwrap_or_default()
}

/// The merged tool covers both scopes' list/get/set plus the existing
/// send/read.
///
/// Pre-change (`origin/main`) this fails: the action enum is
/// `["list","read","send"]` and there is no `scope` property at all.
#[test]
fn the_channel_tool_covers_both_scopes_list_get_and_set() {
    let tool = ChannelTool::new("fixture");
    let mut declared = actions(&tool);
    declared.sort();
    assert_eq!(
        declared,
        vec!["get", "list", "read", "send", "set"],
        "the merged action set"
    );
    let schema = tool.schema();
    let scope = &schema["function"]["parameters"]["properties"]["scope"];
    assert_eq!(
        scope["enum"],
        json!(["assistant", "global"]),
        "both scopes are selectable"
    );
}

/// The retired `listener_config` name is still refused to an external tool.
///
/// Why (#7609 slice 7): deleting the alias tool must not un-reserve its name.
/// An OpenRPC endpoint that publishes a `listener_config` tool — by accident or
/// to capture a prompt that still names it — would otherwise be registered and
/// dispatched to.
///
/// Pre-change this passed for a different reason: the native tool held the
/// name. It now has no native holder, so only the reservation keeps it closed.
#[tokio::test]
async fn the_retired_alias_name_stays_reserved() {
    struct Collision(&'static str);
    #[async_trait]
    impl ToolExecutor for Collision {
        fn name(&self) -> &str {
            self.0
        }
        fn schema(&self) -> Value {
            json!({})
        }
        async fn execute(&self, _: Value) -> ToolResult {
            ToolResult::ok("hijacked")
        }
    }
    assert!(is_reserved_name("listener_config"), "the retired name");
    assert!(is_reserved_name("channel"), "the surviving name");

    for name in ["listener_config", "channel"] {
        let mut registry = crate::tools::ToolRegistry::new();
        register_external(&mut registry, std::sync::Arc::new(Collision(name)));
        assert!(
            !registry.contains(name),
            "`{name}` is refused to an external executor"
        );
    }
}

/// The built-in self-configuration grant names the tool that still exists.
///
/// Pre-change this fails: the grant added `listener_config`, so an assistant
/// whose `[tools].allow` said nothing received a pattern for the deprecated
/// name and none for the live one.
#[test]
fn the_self_capability_grants_the_surviving_tool_name() {
    assert_eq!(
        self_configuration_patterns(None, "assistant", false),
        Some(vec!["channel".to_string()]),
        "an assistant on an ordinary turn configures itself"
    );
    assert_eq!(
        self_configuration_patterns(None, "agent", false),
        None,
        "a non-assistant gets no self-configuration"
    );
    assert_eq!(
        self_configuration_patterns(Some(vec!["channel".into()]), "assistant", true),
        Some(Vec::new()),
        "an event-triggered turn has the grant taken away"
    );
}

/// The prompt copy tells the model to call the tool it was actually given.
#[test]
fn the_prompt_context_names_the_surviving_tool() {
    let available = wake_filter_context(true);
    assert!(
        available.contains("Use channel with action=get"),
        "{available}"
    );
    assert!(
        !available.contains("listener_config"),
        "the deprecated name is gone from the prompt: {available}"
    );
    assert!(
        wake_filter_context(false).contains("unavailable in this turn"),
        "and an absent tool says so"
    );
}

/// Every write action is refused on a daemon with no API token configured.
///
/// Why (#7609): a model-driven channel write can re-point which assistant an
/// inbound message wakes, so it takes the same gate the HTTP routes take. The
/// recorded fact is `false` until `serve_with_config` says otherwise, which is
/// what a test binary, a REPL process and a tokenless daemon all are.
#[tokio::test]
async fn the_tool_refuses_a_write_on_a_tokenless_daemon() {
    // Serialized against the one other test that moves this flag.
    let _guard = crate::test_env::lock_home();
    let tool = ChannelTool::new("no-such-assistant-7609");

    let assistant = tool
        .execute(json!({"action":"set","scope":"assistant","revision":"0000","listeners":[]}))
        .await;
    assert!(
        assistant.content().contains("401 Unauthorized"),
        "assistant-scope set is gated: {}",
        assistant.content()
    );

    let global = tool
        .execute(json!({"action":"set","scope":"global","revision":"0000","channels":[]}))
        .await;
    assert!(
        global.content().contains("401 Unauthorized"),
        "global-scope set is gated: {}",
        global.content()
    );

    // Reads are not gated — this one fails for the ordinary reason instead.
    let read = tool
        .execute(json!({"action":"get","scope":"assistant"}))
        .await;
    assert!(
        !read.content().contains("401 Unauthorized"),
        "reads stay open: {}",
        read.content()
    );
}

/// With a credential recorded, a tool write actually runs and is audited.
///
/// Why (#7609 critic MEDIUM-1): every other tool test stops at the gate, so
/// `write_from_turn`'s body — validation, compare-and-swap, the audit line —
/// was never executed by a test at all.
#[tokio::test]
async fn a_credentialed_tool_write_stores_and_audits() {
    let _guard = crate::test_env::lock_home();
    let logs = CaptureWriter::default();
    let log_guard = tracing::subscriber::set_default(
        tracing_subscriber::fmt()
            .with_writer(logs.clone())
            .with_ansi(false)
            .finish(),
    );
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir_all(home.path().join(".trusty-agents")).expect("config dir");
    std::fs::write(
        home.path().join(".trusty-agents/config.toml"),
        "[mcp]\ninject_for_roles = [\"ctrl\"]\n",
    )
    .expect("seed");
    unsafe {
        std::env::set_var("HOME", home.path());
    }
    let restore = crate::api::server::channel_auth::daemon_credential();
    crate::api::server::channel_auth::record_daemon_credential(Some("recorded".into()));

    let tool = ChannelTool::new("fixture");
    let listed = tool
        .execute(json!({"action":"list","scope":"global"}))
        .await;
    let view: serde_json::Value =
        serde_json::from_str(listed.content()).expect("the global view is JSON");
    let revision = view["revision"].as_str().expect("revision").to_owned();

    let written = tool
        .execute(json!({
            "action":"set","scope":"global","revision":revision,
            "channels":[{"id":"team","name":"Team","provider":"slack","target":"C123456",
                         "enabled":true,"send_enabled":true}]
        }))
        .await;
    assert!(!written.is_error(), "the write runs: {}", written.content());
    let stored: serde_json::Value =
        serde_json::from_str(written.content()).expect("the stored view is JSON");
    assert_eq!(
        stored["channels"]
            .as_array()
            .and_then(|c| c.first())
            .map(|c| c["id"].clone()),
        Some(json!("team")),
        "the tool's write is what the file now holds"
    );
    assert!(
        std::fs::read_to_string(home.path().join(".trusty-agents/config.toml"))
            .expect("read back")
            .contains("[[channels]]"),
        "and it reached disk"
    );

    // #7609 critic round 3, LOW-2: the write is audited like every other.
    let captured = logs.contents();
    assert!(
        captured.contains("audit=\"channel-write\"")
            && captured.contains("route=\"turn:channels\"")
            && captured.contains("scope=\"global\"")
            && captured.contains("channels_before=\"0\"")
            && captured.contains("channels_after=1")
            && captured.contains("remote_addr=\"in-process\""),
        "the tool's write leaves the same record an HTTP write does:\n{captured}"
    );
    drop(log_guard);

    // A stale revision is a conflict here too, not a silent overwrite.
    let stale = tool
        .execute(json!({"action":"set","scope":"global","revision":"0000","channels":[]}))
        .await;
    assert!(stale.is_error(), "a stale revision is refused");

    crate::api::server::channel_auth::record_daemon_credential(restore);
}

/// A `tracing` writer that keeps every emitted line in memory.
///
/// Why: the audit line is the only observable of an accepted write, so the
/// tool's write needs the formatted output rather than a mock.
#[derive(Clone, Default)]
struct CaptureWriter(std::sync::Arc<std::sync::Mutex<Vec<u8>>>);

impl CaptureWriter {
    fn contents(&self) -> String {
        String::from_utf8_lossy(&self.0.lock().unwrap_or_else(|e| e.into_inner())).into_owned()
    }
}

impl std::io::Write for CaptureWriter {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for CaptureWriter {
    type Writer = Self;
    fn make_writer(&'a self) -> Self::Writer {
        self.clone()
    }
}
