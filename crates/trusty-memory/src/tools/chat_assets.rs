//! Explicit asset capability and session-owned image RPCs (#7370).
use crate::AppState;
use anyhow::{Context, Result};
use serde_json::{json, Value};
use trusty_common::memory_core::store::chat_sessions::ChatImageAsset;

/// Why: durable image bytes belong to trusty-memory rather than an assistant-local store.
/// What: dispatch versioned capability, bounded image creation and session-owned reads in the resolved palace.
/// Test: trusty-common `asset_ownership_and_restart_are_enforced` exercises the delegated storage boundary.
pub(crate) async fn handle(state: &AppState, operation: &str, args: Value) -> Result<Value> {
    if operation == "chat_asset_capabilities" {
        return Ok(json!({"version":1,"typed_history_attachments":true,"max_image_bytes":5242880}));
    }
    let palace = super::helpers::resolve_palace(state, &args, operation)?;
    let session = args
        .get("session_id")
        .and_then(Value::as_str)
        .context("session_id is required")?;
    let store = state.session_store(&palace)?;
    if operation == "chat_asset_put" {
        let asset = ChatImageAsset {
            session_id: session.into(),
            name: args
                .get("name")
                .and_then(Value::as_str)
                .context("name is required")?
                .into(),
            mime_type: args
                .get("mime_type")
                .and_then(Value::as_str)
                .context("mime_type is required")?
                .into(),
            data_base64: args
                .get("data_base64")
                .and_then(Value::as_str)
                .context("data_base64 is required")?
                .into(),
        };
        let id = store.put_chat_asset(asset)?;
        Ok(json!({"asset_id":id,"status":"stored","version":1}))
    } else {
        let id = args
            .get("asset_id")
            .and_then(Value::as_str)
            .context("asset_id is required")?;
        Ok(serde_json::to_value(store.get_chat_asset(session, id)?)?)
    }
}
pub(super) fn definitions() -> Vec<Value> {
    vec![
        json!({"name":"chat_asset_capabilities","description":"Read supported durable chat attachment contract.","inputSchema":{"type":"object","properties":{}}}),
        json!({"name":"chat_asset_put","description":"Store a bounded image in an existing palace chat session; returns generated asset_id.","inputSchema":{"type":"object","required":["palace","session_id","name","mime_type","data_base64"],"properties":{"palace":{"type":"string"},"session_id":{"type":"string"},"name":{"type":"string"},"mime_type":{"enum":["image/png","image/jpeg","image/webp"]},"data_base64":{"type":"string"}}}}),
        json!({"name":"chat_asset_get","description":"Read an image owned by the supplied palace and session.","inputSchema":{"type":"object","required":["palace","session_id","asset_id"],"properties":{"palace":{"type":"string"},"session_id":{"type":"string"},"asset_id":{"type":"string"}}}}),
    ]
}
