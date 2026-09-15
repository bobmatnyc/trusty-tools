//! The merged `channel` tool and its deprecated `listener_config` alias
//! (#7609 slice 5).
//!
//! Why: the merge is only real if the two tools answer identically for the
//! actions they share, and only safe if the alias keeps every existing
//! self-configuration pattern working. Both fail against `origin/main`, where
//! `channel` rejects `get`/`set` outright.
//! What: schema assertions (pure, environment-independent) plus one behavioural
//! forwarding assertion that compares the two tools' answers to the same call.
//! Test: this module IS the test.

use crate::tools::channel::ChannelTool;
use crate::tools::listener_config::ListenerConfigTool;
use crate::tools::traits::ToolExecutor;
use serde_json::json;

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
fn the_channel_tool_absorbs_the_listener_config_actions() {
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

/// `listener_config` stays callable and forwards to the merged tool.
///
/// Pre-change (`origin/main`) this fails: `channel` refuses `action=get` with
/// "Use list, read, or send" while `listener_config` answers the listener view,
/// so the two never agree.
#[tokio::test]
async fn the_listener_config_alias_forwards_to_the_channel_tool() {
    let alias = ListenerConfigTool::new("no-such-assistant-7609");
    let merged = ChannelTool::new("no-such-assistant-7609");

    let through_alias = alias.execute(json!({"action":"get"})).await;
    let direct = merged
        .execute(json!({"action":"get","scope":"assistant"}))
        .await;
    assert_eq!(
        through_alias.content(),
        direct.content(),
        "the alias answers exactly what the merged tool answers"
    );
    assert_eq!(through_alias.is_error(), direct.is_error());
}

/// The alias keeps refusing an argument shape it never accepted.
///
/// Why: forwarding must not widen the surface — `listener_config` still takes
/// only `action`, `revision` and `listeners`, so a caller cannot reach the
/// global scope through the deprecated name.
#[tokio::test]
async fn the_alias_cannot_reach_the_global_scope() {
    let alias = ListenerConfigTool::new("fixture");
    assert!(
        alias
            .execute(json!({"action":"get","scope":"global"}))
            .await
            .is_error(),
        "`scope` is not part of the deprecated tool's schema"
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
