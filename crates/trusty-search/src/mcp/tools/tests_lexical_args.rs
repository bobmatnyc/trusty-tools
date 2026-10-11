//! MCP forwarding of the lexical-lane arguments (#9258).
//!
//! Why: the daemon honours `ripgrep_fallback` / `lexical_limit` only when the
//! bridge passes them through; a dropped value silently answers the default.
//! What: asserts the daemon body each search tool sends, the type checks, and
//! the schema advertisement.
//! Test: this module.

use serde_json::Value;

use super::lexical_args::LEXICAL_LANE_TOOLS;
use super::test_daemon::{recording_daemon, unreachable_server};
use super::tests::req;
use super::{error_codes, tool_descriptors};

#[tokio::test]
async fn lexical_lane_args_are_forwarded_and_type_checked() {
    let (daemon, calls) = recording_daemon(|_, _| Ok(serde_json::json!({ "results": [] }))).await;
    let server = daemon.server();
    let last = || {
        let (method, params) = calls
            .lock()
            .expect("the call log")
            .last()
            .cloned()
            .expect("a forwarded call");
        if method == "search.query.all" {
            params
        } else {
            params["body"].clone()
        }
    };
    let both = serde_json::json!({ "ripgrep_fallback": false, "lexical_limit": 7 });
    for (tool, mut args) in [
        (
            "search",
            serde_json::json!({ "index_id": "demo", "query": "q" }),
        ),
        (
            "search_lexical",
            serde_json::json!({ "index_id": "demo", "query": "q" }),
        ),
        ("search_all", serde_json::json!({ "query": "q" })),
    ] {
        let resp = server.dispatch(req(tool, args.clone())).await;
        assert!(resp.error.is_none(), "{tool}: {:?}", resp.error);
        let body = last();
        assert!(body.get("ripgrep_fallback").is_none(), "{tool}: {body}");
        assert!(body.get("lexical_limit").is_none(), "{tool}: {body}");

        for (k, v) in both.as_object().expect("object") {
            args[k] = v.clone();
        }
        let resp = server.dispatch(req(tool, args)).await;
        assert!(resp.error.is_none(), "{tool}: {:?}", resp.error);
        let body = last();
        assert_eq!(
            body["ripgrep_fallback"],
            Value::Bool(false),
            "{tool}: {body}"
        );
        assert_eq!(body["lexical_limit"], Value::from(7u64), "{tool}: {body}");
    }

    for (key, bad) in [
        ("ripgrep_fallback", Value::String("false".into())),
        ("lexical_limit", Value::String("7".into())),
        ("lexical_limit", Value::from(-3)),
    ] {
        let mut args = serde_json::json!({ "index_id": "demo", "query": "q" });
        args[key] = bad.clone();
        let resp = unreachable_server()
            .dispatch(req("search_lexical", args))
            .await;
        let err = resp
            .error
            .unwrap_or_else(|| panic!("{key}={bad} must be refused"));
        assert_eq!(err.code, error_codes::INVALID_PARAMS, "{}", err.message);
        assert!(err.message.contains(key), "{}", err.message);
    }
}

#[test]
fn search_tools_advertise_the_lexical_lane_args() {
    let defs = tool_descriptors();
    for name in LEXICAL_LANE_TOOLS {
        let tool = defs
            .as_array()
            .and_then(|a| a.iter().find(|t| t["name"] == *name))
            .unwrap_or_else(|| panic!("{name} is listed"));
        let props = &tool["inputSchema"]["properties"];
        assert_eq!(props["ripgrep_fallback"]["type"], "boolean", "{name}");
        assert_eq!(props["lexical_limit"]["type"], "integer", "{name}");
        assert_eq!(
            props["lexical_limit"]["maximum"],
            Value::from(crate::core::indexer::MAX_LEXICAL_LIMIT),
            "{name}"
        );
    }
}
