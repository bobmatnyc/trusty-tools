//! Bind the protected OKG to trusty-search without accepting a caller index override.
use crate::knowledge::ProtectedStore;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use trusty_common::search_rpc;

async fn call(socket: &Path, method: &str, params: Value) -> anyhow::Result<Value> {
    search_rpc::call_at(socket, method, params, Duration::from_secs(3)).await
}
fn matches(root: &Path, value: &Value) -> bool {
    value
        .get("root_path")
        .and_then(Value::as_str)
        .and_then(|p| std::fs::canonicalize(p).ok())
        .zip(root.canonicalize().ok())
        .is_some_and(|(a, b)| a == b)
}

/// Observe or provision only the fixed Assistant-owned index; never relabel a foreign root.
/// Test: `index_collision_never_reindexes_a_foreign_root`.
pub(super) async fn status(socket: Option<&Path>, store: &ProtectedStore, create: bool) -> Value {
    let result = async {
        let socket = socket.ok_or_else(|| anyhow::anyhow!("trusty-search is unavailable"))?;
        at(socket, store, create).await
    }
    .await;
    match result {
        Ok(value) => value,
        Err(error) => {
            json!({"connected":false,"index_id":store.index_id,"reason":error.to_string()})
        }
    }
}

pub(super) async fn at(
    socket: &Path,
    store: &ProtectedStore,
    create: bool,
) -> anyhow::Result<Value> {
    let indexes = call(
        socket,
        search_rpc::METHOD_INDEXES_LIST,
        json!({"details":true}),
    )
    .await?;
    let list = indexes
        .get("indexes")
        .and_then(Value::as_array)
        .or_else(|| indexes.as_array())
        .ok_or_else(|| anyhow::anyhow!("Search returned an invalid index catalogue"))?;
    let existing = list.iter().find(|v| {
        v.get("id")
            .or_else(|| v.get("index_id"))
            .and_then(Value::as_str)
            == Some(store.index_id.as_str())
    });
    if let Some(existing) = existing {
        anyhow::ensure!(
            matches(&store.root, existing),
            "Protected index is registered to another root; no changes were made"
        );
    } else if create {
        call(
            socket,
            search_rpc::METHOD_INDEX_CREATE,
            json!({"id":store.index_id,"root_path":store.root,"follow_links":false}),
        )
        .await?;
    } else {
        return Ok(
            json!({"connected":false,"index_id":store.index_id,"reason":"Protected OKG has not been registered with trusty-search"}),
        );
    }
    let status = call(
        socket,
        search_rpc::METHOD_INDEX_STATUS,
        json!({"index_id":store.index_id}),
    )
    .await?;
    anyhow::ensure!(
        matches(&store.root, &status),
        "Search did not confirm this Assistant's protected OKG root"
    );
    let failed = status
        .get("stages")
        .and_then(Value::as_object)
        .is_some_and(|stages| {
            stages.values().any(|v| {
                v.as_str().is_some_and(|s| s.eq_ignore_ascii_case("failed"))
                    || v.get("status")
                        .and_then(Value::as_str)
                        .is_some_and(|s| s.eq_ignore_ascii_case("failed"))
            })
        });
    let status_text = status
        .get("status")
        .or_else(|| status.get("index_status"))
        .and_then(Value::as_str);
    let connected = !failed && status_text == Some("ready");
    Ok(
        json!({"connected":connected,"index_id":store.index_id,"status":status,"reason":if connected {None} else {Some("Search has not reported this protected index ready")}}),
    )
}
