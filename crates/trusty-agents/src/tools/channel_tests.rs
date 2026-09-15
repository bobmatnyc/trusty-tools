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
