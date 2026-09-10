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
        let request = json!({"id":store.index_id,"root_path":store.root,"follow_links":false,"include_paths":["notes"],"extensions":["md"]});
        if let Err(error) = call(socket, search_rpc::METHOD_INDEX_CREATE, request.clone()).await {
            if error
                .downcast_ref::<search_rpc::SearchRpcError>()
                .is_none_or(|e| e.code != -32003)
            {
                return Err(error);
            }
            // #4283: approval uses this host's policy only for its canonical daemon socket.
            let expected = trusty_common::daemon_socket_path("trusty-search")?;
            anyhow::ensure!(
                socket.canonicalize()? == expected.canonicalize()?,
                "Custom search socket requires approval in that daemon's configuration"
            );
            let owned = store.clone();
            tokio::task::spawn_blocking(move || approve_owned_root(&owned, None)).await??;
            call(socket, search_rpc::METHOD_INDEX_CREATE, request).await?;
        }
    } else {
        return Ok(
            json!({"connected":false,"index_id":store.index_id,"reason":"Protected OKG has not been registered with trusty-search"}),
        );
    }
    if store.index_id.starts_with("assistant-okg-") && create {
        let config = call(
            socket,
            "search.index.config.get",
            json!({"index_id":store.index_id}),
        )
        .await?;
        let extensions = config
            .get("extensions")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("Invalid search index configuration"))?;
        if extensions != &vec![json!("md")] {
            call(
                socket,
                "search.index.config.set",
                json!({"index_id":store.index_id,"body":{"extensions":["md"]}}),
            )
            .await?;
            // Reconcile old generated indexes to remove formerly indexed bookkeeping documents.
            call(
                socket,
                search_rpc::METHOD_INDEX_REINDEX,
                json!({"index_id":store.index_id,"body":{"force":true}}),
            )
            .await?;
        }
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

/// Approve only this validated protected root, preserving all existing policy entries.
fn approve_owned_root(store: &ProtectedStore, config: Option<&Path>) -> anyhow::Result<()> {
    use trusty_search::allowlist::{AllowlistConfig, AllowlistEntry};
    anyhow::ensure!(
        store.protected,
        "Only protected Assistant roots can be provisioned"
    );
    let root = store.root.canonicalize()?;
    let config = config
        .map(Path::to_path_buf)
        .unwrap_or_else(AllowlistConfig::default_path);
    let parent = config
        .parent()
        .ok_or_else(|| anyhow::anyhow!("Missing search policy directory"))?;
    std::fs::create_dir_all(parent)?;
    let config = parent.canonicalize()?.join(
        config
            .file_name()
            .ok_or_else(|| anyhow::anyhow!("Missing search policy filename"))?,
    );
    let _guard = crate::knowledge::persistence::lock(&config.with_extension("approval.lock"))?;
    if AllowlistConfig::load_from(&config)?.contains(&root) {
        return Ok(());
    }
    trusty_search::allowlist::add_to_allowlist(
        AllowlistEntry {
            path: root,
            name: Some(store.index_id.clone()),
            exclude: vec![],
            extensions: vec![],
            skip_kg: false,
        },
        Some(&config),
    )
}

#[cfg(test)]
mod tests {
    #[test]
    fn existing_root_approval_preserves_policy_entries() {
        use trusty_search::allowlist::{AllowlistConfig, AllowlistEntry};
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().canonicalize().unwrap();
        let path = root.join("policy.toml");
        let entry = AllowlistEntry {
            path: root.clone(),
            name: Some("existing".into()),
            exclude: vec!["private/**".into()],
            extensions: vec!["md".into()],
            skip_kg: true,
        };
        let original = AllowlistConfig {
            entries: vec![entry],
        };
        original.save_to(&path).unwrap();
        let store = crate::knowledge::ProtectedStore {
            root,
            index_id: "owned".into(),
            protected: true,
        };
        super::approve_owned_root(&store, Some(&path)).unwrap();
        assert_eq!(AllowlistConfig::load_from(&path).unwrap(), original);
        let mut foreign = store;
        foreign.protected = false;
        assert!(super::approve_owned_root(&foreign, Some(&path)).is_err());
    }
}
