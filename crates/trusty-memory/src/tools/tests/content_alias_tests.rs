//! `text` and `content` name one write field (#9746).
//!
//! Why: callers pass `content` to `memory_remember` and `text` to
//! `memory_note`, and both failed with a "missing" error. The two write tools
//! now accept either name for their body, and a call that names both with
//! different values must fail rather than store one of them.
//! What: drives the real handlers through `dispatch_tool` against a temp
//! `AppState`, one table row per in-scope tool.
//! Test: this IS the test module.

use super::*;

/// One in-scope tool: its name, its documented body field and that field's
/// alias.
const TOOLS: [(&str, &str, &str); 2] = [
    ("memory_remember", "text", "content"),
    ("memory_note", "content", "text"),
];

/// A body long enough to pass every content gate without `force`.
const BODY: &str = "The release train leaves every Monday after the freeze lifts";

/// Create `palace` through `palace_create`.
async fn create(state: &AppState, palace: &str) {
    dispatch_tool(state, "palace_create", json!({ "name": palace }))
        .await
        .expect("palace_create");
}

/// `memory_list` of `palace`, as its `drawers` array.
async fn list(state: &AppState, palace: &str) -> Vec<Value> {
    let out = dispatch_tool(
        state,
        "memory_list",
        json!({ "palace": palace, "limit": 50 }),
    )
    .await
    .expect("memory_list");
    out["drawers"].as_array().expect("drawers array").clone()
}

/// The fields of a listed drawer that the write arguments decide.
fn stored(drawer: &Value) -> Value {
    json!({
        "content": drawer["content"],
        "drawer_type": drawer["drawer_type"],
        "importance": drawer["importance"],
        "tags": drawer["tags"],
    })
}

/// Write `BODY` to a fresh `palace` through `tool` with `fields`, and return
/// the one drawer it stored.
async fn write_one(state: &AppState, tool: &str, palace: &str, fields: &[&str]) -> Value {
    create(state, palace).await;
    let mut args = json!({ "palace": palace });
    for field in fields {
        args[*field] = json!(BODY);
    }
    let out = dispatch_tool(state, tool, args)
        .await
        .unwrap_or_else(|e| panic!("{tool} {fields:?}: {e:#}"));
    assert_eq!(out["status"], "stored", "{tool} {fields:?}: {out}");
    let drawers = list(state, palace).await;
    assert_eq!(drawers.len(), 1, "{tool} {fields:?}: {drawers:?}");
    stored(&drawers[0])
}

/// Why (#9746): the alias must be a second spelling of the same field, so a
/// call through it stores exactly what the documented name stores. Naming
/// both with one value is the same call and is accepted.
/// What: per tool, writes the same body through the documented name, the
/// alias, and both at once, each into its own palace, and compares the
/// listed drawers.
#[tokio::test]
async fn aliased_write_field_stores_the_same_drawer_as_the_documented_name() {
    let (state, _tmp) = test_state();
    for (i, (tool, name, alias)) in TOOLS.into_iter().enumerate() {
        let want = write_one(&state, tool, &format!("ca{i}n"), &[name]).await;
        assert_eq!(want["content"], BODY, "{tool}: {want}");
        let via_alias = write_one(&state, tool, &format!("ca{i}a"), &[alias]).await;
        assert_eq!(via_alias, want, "{tool}: `{alias}` alone");
        let both = write_one(&state, tool, &format!("ca{i}b"), &[name, alias]).await;
        assert_eq!(both, want, "{tool}: `{name}` and `{alias}` equal");
    }
}

/// Why (#9746, Fail-Open Check): with two different bodies the handler has no
/// basis to pick one, and storing either one silently loses the other.
/// What: per tool, a call naming both fields with different strings, and one
/// whose alias is not a string, must each fail naming the tool and both
/// fields; the palace stays empty.
#[tokio::test]
async fn conflicting_text_and_content_is_refused_and_writes_nothing() {
    let (state, _tmp) = test_state();
    for (i, (tool, name, alias)) in TOOLS.into_iter().enumerate() {
        let palace = format!("cc{i}");
        create(&state, &palace).await;
        let other = "A different body that the caller also sent in this call";
        for alias_value in [json!(other), json!(42)] {
            let mut args = json!({ "palace": palace });
            args[name] = json!(BODY);
            args[alias] = alias_value.clone();
            let err = dispatch_tool(&state, tool, args)
                .await
                .expect_err("two different bodies must be refused");
            let msg = format!("{err:#}");
            assert!(msg.starts_with(&format!("{tool}: ")), "{msg}");
            assert!(msg.contains(&format!("'{alias}'")), "{msg}");
            if alias_value.is_string() {
                assert!(msg.contains(&format!("'{name}'")), "{msg}");
            }
        }
        assert!(list(&state, &palace).await.is_empty(), "{tool} wrote");
    }
}

/// Why (#9746): a caller that sent neither field needs to learn both accepted
/// names, not only the one the tool happens to document.
/// What: per tool, a call with no body field fails, naming both fields.
#[tokio::test]
async fn missing_text_and_content_error_names_both_fields() {
    let (state, _tmp) = test_state();
    for (i, (tool, name, alias)) in TOOLS.into_iter().enumerate() {
        let palace = format!("cm{i}");
        create(&state, &palace).await;
        let err = dispatch_tool(&state, tool, json!({ "palace": palace }))
            .await
            .expect_err("a call with no body must be refused");
        let msg = format!("{err:#}");
        assert!(msg.starts_with(&format!("{tool}: ")), "{msg}");
        assert!(msg.contains(&format!("'{name}'")), "{msg}");
        assert!(msg.contains(&format!("'{alias}'")), "{msg}");
        assert!(list(&state, &palace).await.is_empty(), "{tool} wrote");
    }
}

/// Why (#9746): clients learn argument names from the schema, so the alias is
/// advertised; the documented name stays the required one.
/// What: each in-scope tool lists its alias as an optional string property.
#[test]
fn write_schemas_advertise_the_text_content_alias() {
    let defs = tool_definitions();
    let tools = defs["tools"].as_array().expect("tools array");
    for (tool, name, alias) in TOOLS {
        let schema = &tools
            .iter()
            .find(|t| t["name"] == tool)
            .unwrap_or_else(|| panic!("{tool} listed"))["inputSchema"];
        assert_eq!(schema["properties"][alias]["type"], "string", "{tool}");
        let required = schema["required"].as_array().expect("required array");
        assert!(required.contains(&json!(name)), "{tool}: {required:?}");
        assert!(!required.contains(&json!(alias)), "{tool}: {required:?}");
    }
}
