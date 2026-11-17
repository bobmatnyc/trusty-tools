//! Bind the protected OKG to trusty-search without accepting a caller index override.
use crate::knowledge::ProtectedStore;
use serde_json::{Value, json};
use std::{path::Path, time::Duration};
use trusty_common::search_rpc;

/// #10740: the OKG binding admits every concept path except these trees
/// (SPEC-OKF-01 §5.3.1 rule 1, D3). Literal entries, so the §5.3.1 rule 4
/// exclusion proof can read them back from `search.index.config.get`.
const OKG_EXCLUDE_GLOBS: [&str; 2] = ["_sources/**", "_state/**"];

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

/// Bind the protected OKG at `socket`, creating it when `create` is set.
/// Why: the binding decides which bundle paths trusty-search admits (#10740).
/// What: creates the index with [`OKG_EXCLUDE_GLOBS`] and no allowlist, or
/// reconciles an existing `assistant-okg-` binding to carry them.
/// Test: `new_okg_binding_admits_every_concept_and_no_bookkeeping`,
/// `existing_okg_binding_gains_exclusions_and_drops_indexed_bookkeeping`,
/// `failed_bookkeeping_removal_is_retried_on_the_next_bind`,
/// `config_set_purge_error_fails_the_bind`.
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
        // #10740: no `include_paths` allowlist — a concept can sit in any directory.
        let request = json!({"id":store.index_id,"root_path":store.root,"follow_links":false,"exclude_globs":OKG_EXCLUDE_GLOBS,"extensions":["md"]});
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
        let mut body = serde_json::Map::new();
        let wrong_extensions = extensions != &vec![json!("md")];
        if wrong_extensions {
            body.insert("extensions".into(), json!(["md"]));
        }
        // #10740: an existing binding gains the literal exclusions, keeping any
        // other entries it already carries.
        // #10740: residual — legacy include_paths; cleared via #10739
        let mut globs = config
            .get("exclude_globs")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let missing: Vec<&str> = OKG_EXCLUDE_GLOBS
            .into_iter()
            .filter(|glob| !globs.iter().any(|v| v.as_str() == Some(glob)))
            .collect();
        globs.extend(missing.iter().map(|glob| json!(glob)));
        if !missing.is_empty() {
            body.insert("exclude_globs".into(), Value::Array(globs));
        }
        if !body.is_empty() {
            let reply = call(
                socket,
                "search.index.config.set",
                json!({"index_id":store.index_id,"body":body}),
            )
            .await?;
            // #10740: a 200 reply can still report a failed purge of excluded files.
            if let Some(error) = reply.pointer("/profile_purge/error") {
                anyhow::bail!("Search could not purge excluded bookkeeping files: {error}");
            }
        }
        if wrong_extensions {
            // Reconcile old generated indexes to remove formerly indexed bookkeeping documents.
            call(
                socket,
                search_rpc::METHOD_INDEX_REINDEX,
                json!({"index_id":store.index_id,"body":{"force":true}}),
            )
            .await?;
        }
    }
    if store.index_id.starts_with("assistant-okg-") {
        // #10740: every bind re-checks, so a removal that failed earlier is retried.
        scrub_bookkeeping(socket, store).await?;
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

/// Every indexed path with a `_sources` or `_state` component, nested included.
async fn bookkeeping_paths(
    socket: &Path,
    store: &ProtectedStore,
) -> anyhow::Result<std::collections::BTreeSet<String>> {
    let trees: Vec<&str> = OKG_EXCLUDE_GLOBS
        .iter()
        .map(|glob| glob.trim_end_matches("/**"))
        .collect();
    let mut found = std::collections::BTreeSet::new();
    let mut after = String::new();
    loop {
        let page = call(
            socket,
            "search.chunks.list",
            json!({"index_id":store.index_id,"limit":1000,"after":after}),
        )
        .await?;
        let chunks = page
            .get("chunks")
            .and_then(Value::as_array)
            .ok_or_else(|| anyhow::anyhow!("Search returned an invalid chunk page"))?;
        for chunk in chunks {
            let path = chunk
                .get("path")
                .and_then(Value::as_str)
                .map(str::to_owned)
                .or_else(|| {
                    let file = Path::new(chunk.get("file")?.as_str()?);
                    let relative = file.strip_prefix(&store.root).unwrap_or(file);
                    Some(relative.to_string_lossy().into_owned())
                })
                .ok_or_else(|| anyhow::anyhow!("Search returned a chunk without a path"))?;
            let bookkeeping = Path::new(&path)
                .components()
                .any(|c| trees.iter().any(|tree| c.as_os_str() == *tree));
            if bookkeeping {
                found.insert(path);
            }
        }
        match page.get("next_cursor").and_then(Value::as_str) {
            Some(next) if !next.is_empty() && !chunks.is_empty() => after = next.to_owned(),
            _ => return Ok(found),
        }
    }
}

/// Remove indexed `_sources`/`_state` files; fail the bind unless none remain.
/// Why: SPEC-OKF-01 §5.3.1 — a removal that failed once must not read as bound.
/// What: lists the leaked paths, removes them in batches, re-lists, and errors
/// when any path is still indexed.
/// Test: `failed_bookkeeping_removal_is_retried_on_the_next_bind`.
async fn scrub_bookkeeping(socket: &Path, store: &ProtectedStore) -> anyhow::Result<()> {
    let leaked = bookkeeping_paths(socket, store).await?;
    if leaked.is_empty() {
        return Ok(());
    }
    let leaked: Vec<String> = leaked.into_iter().collect();
    for batch in leaked.chunks(10_000) {
        call(
            socket,
            "search.index.files.remove",
            json!({"index_id":store.index_id,"body":{"paths":batch}}),
        )
        .await?;
    }
    let left = bookkeeping_paths(socket, store).await?;
    anyhow::ensure!(
        left.is_empty(),
        "Search still indexes bookkeeping files: {left:?}"
    );
    Ok(())
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
    use crate::knowledge::ProtectedStore;
    use serde_json::{Value, json};
    use std::collections::BTreeSet;
    use std::path::Path;
    use std::sync::{Arc, Mutex};
    use trusty_common::search_rpc;

    /// The daemon side of the fake: its config, its indexed paths, every call.
    #[derive(Default)]
    struct Daemon {
        listed: Option<Value>,
        config: Value,
        indexed: BTreeSet<String>,
        calls: Vec<(String, Value)>,
        /// `files.remove` calls still to refuse, as a busy daemon does.
        refuse_removes: usize,
        /// A `profile_purge.error` for the `config.set` reply to carry.
        purge_error: Option<String>,
    }

    /// A fake trusty-search that keeps `config.set` and `files.remove` effects.
    async fn fake(
        root: &Path,
        daemon: Daemon,
    ) -> (crate::uds_mock::MockMemoryDaemon, Arc<Mutex<Daemon>>) {
        let state = Arc::new(Mutex::new(daemon));
        let shared = state.clone();
        let root = root.to_path_buf();
        let mock = crate::uds_mock::spawn(move |method, params| {
            let mut d = shared.lock().unwrap();
            d.calls.push((method.to_owned(), params.clone()));
            if method == "search.index.files.remove" && d.refuse_removes > 0 {
                d.refuse_removes -= 1;
                return Box::pin(async {
                    Err(crate::uds_mock::RpcError::new(-32050, "remove_files_busy"))
                });
            }
            let reply = match method {
                search_rpc::METHOD_INDEXES_LIST => json!({"indexes": d.listed.iter().collect::<Vec<_>>()}),
                search_rpc::METHOD_INDEX_CREATE => {
                    d.config = json!({"extensions": params["extensions"], "exclude_globs": params["exclude_globs"]});
                    json!({"created": true})
                }
                "search.index.config.get" => d.config.clone(),
                "search.index.config.set" => {
                    for (key, value) in params["body"].as_object().unwrap() {
                        d.config[key] = value.clone();
                    }
                    match &d.purge_error {
                        Some(error) => json!({"persisted": true, "profile_purge": {"error": error}}),
                        None => json!({"persisted": true}),
                    }
                }
                "search.chunks.list" => {
                    let chunks: Vec<Value> = d.indexed.iter().map(|p| json!({"path": p})).collect();
                    json!({"chunks": chunks, "next_cursor": null})
                }
                "search.index.files.remove" => {
                    let body = &params["body"];
                    let paths: BTreeSet<String> = body["paths"]
                        .as_array()
                        .map(|a| a.iter().filter_map(Value::as_str).map(str::to_owned).collect())
                        .unwrap_or_default();
                    let prefix = body["path_prefix"].as_str().map(str::to_owned);
                    d.indexed.retain(|p| {
                        !paths.contains(p) && !prefix.as_ref().is_some_and(|x| Path::new(p).starts_with(x))
                    });
                    json!({})
                }
                search_rpc::METHOD_INDEX_STATUS => json!({"root_path": root, "status": "ready"}),
                _ => json!({}),
            };
            Box::pin(async move { Ok(reply) })
        })
        .await;
        (mock, state)
    }

    fn store(root: &Path) -> ProtectedStore {
        ProtectedStore {
            root: root.to_path_buf(),
            index_id: "assistant-okg-twin".into(),
            protected: true,
        }
    }

    /// Request-shape check only: what a binding's `include_paths`,
    /// `exclude_globs` (through the daemon's own glob matcher) and `extensions`
    /// say about a root-relative path. It does not run trusty-search's
    /// admission code, which is crate-private.
    fn admits(binding: &Value, path: &str) -> bool {
        let strings = |key: &str| -> Vec<String> {
            binding[key]
                .as_array()
                .map(|a| {
                    a.iter()
                        .filter_map(Value::as_str)
                        .map(str::to_owned)
                        .collect()
                })
                .unwrap_or_default()
        };
        let (include, globs, extensions) = (
            strings("include_paths"),
            strings("exclude_globs"),
            strings("extensions"),
        );
        let path = Path::new(path);
        (include.is_empty() || include.iter().any(|p| path.starts_with(p)))
            && !trusty_search::core::repo_config::path_matches_any_glob(path, &globs)
            && (extensions.is_empty()
                || path
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| extensions.iter().any(|x| x == e)))
    }

    /// Why: SPEC-OKF-01 §5.3.1 rule 1 (D3, #10740) — the bundle's bookkeeping
    /// trees must never be indexed, and every concept path must be.
    /// What: binds a fresh OKG and checks the create request's admission over
    /// bookkeeping and concept paths, and that it names no allowlist.
    #[tokio::test]
    async fn new_okg_binding_admits_every_concept_and_no_bookkeeping() {
        let tmp = tempfile::tempdir().unwrap();
        let (mock, daemon) = fake(tmp.path(), Daemon::default()).await;
        super::at(mock.socket(), &store(tmp.path()), true)
            .await
            .unwrap();
        let calls = daemon.lock().unwrap().calls.clone();
        let (_, binding) = calls
            .iter()
            .find(|(m, _)| m == search_rpc::METHOD_INDEX_CREATE)
            .expect("the binding is created");
        assert_eq!(
            binding["exclude_globs"],
            json!(["_sources/**", "_state/**"])
        );
        assert!(binding.get("include_paths").is_none(), "{binding}");
        for refused in [
            "_sources/registry.toml",
            "_sources/notes-ledger.md",
            "_state/conversion-log.md",
        ] {
            assert!(!admits(binding, refused), "{refused} admitted by {binding}");
        }
        for concept in ["index.md", "log.md", "notes/a.md", "drive/team/plans/q3.md"] {
            assert!(admits(binding, concept), "{concept} refused by {binding}");
        }
    }

    /// Why: an OKG bound before #10740 carries no exclusions, and may already
    /// hold `_sources/` or `_state/` chunks (SPEC-OKF-01 §5.3.1, D3).
    /// What: the next bind adds both literal globs, keeps the binding's other
    /// globs, removes both prefixes, and changes nothing on a second bind.
    #[tokio::test]
    async fn existing_okg_binding_gains_exclusions_and_drops_indexed_bookkeeping() {
        let tmp = tempfile::tempdir().unwrap();
        let root = tmp.path().to_path_buf();
        let legacy = Daemon {
            listed: Some(json!({"id": "assistant-okg-twin", "root_path": root})),
            config: json!({"extensions": ["md"], "exclude_globs": ["**/node_modules/**"]}),
            indexed: [
                "_sources/registry.toml",
                "_state/conversion-log.md",
                "notes/a.md",
            ]
            .map(str::to_owned)
            .into(),
            ..Daemon::default()
        };
        let (mock, daemon) = fake(&root, legacy).await;
        super::at(mock.socket(), &store(&root), true).await.unwrap();
        {
            let d = daemon.lock().unwrap();
            assert_eq!(
                d.config["exclude_globs"],
                json!(["**/node_modules/**", "_sources/**", "_state/**"])
            );
            assert_eq!(d.indexed, BTreeSet::from(["notes/a.md".to_owned()]));
            assert!(
                !d.calls
                    .iter()
                    .any(|(m, _)| m == search_rpc::METHOD_INDEX_REINDEX)
            );
        }
        daemon.lock().unwrap().calls.clear();
        super::at(mock.socket(), &store(&root), true).await.unwrap();
        let methods: Vec<String> = daemon
            .lock()
            .unwrap()
            .calls
            .iter()
            .map(|(m, _)| m.clone())
            .collect();
        assert!(
            !methods
                .iter()
                .any(|m| m == "search.index.config.set" || m == "search.index.files.remove"),
            "a corrected binding is left alone: {methods:?}"
        );
    }

    fn legacy(root: &Path) -> Daemon {
        Daemon {
            listed: Some(json!({"id": "assistant-okg-twin", "root_path": root})),
            config: json!({"extensions": ["md"]}),
            indexed: [
                "_sources/registry.toml",
                "nested/_state/conversion-log.md",
                "notes/a.md",
            ]
            .map(str::to_owned)
            .into(),
            ..Daemon::default()
        }
    }

    /// Why: a refused removal (busy daemon, timeout) after `config.set` stored
    /// the globs must not leave the bookkeeping indexed behind a bound OKG.
    /// What: the first bind's `files.remove` fails; the second bind still
    /// removes the top-level and nested bookkeeping and only then binds.
    #[tokio::test]
    async fn failed_bookkeeping_removal_is_retried_on_the_next_bind() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = Daemon {
            refuse_removes: 1,
            ..legacy(tmp.path())
        };
        let (mock, daemon) = fake(tmp.path(), daemon).await;
        assert!(
            super::at(mock.socket(), &store(tmp.path()), true)
                .await
                .is_err()
        );
        daemon.lock().unwrap().refuse_removes = 0;
        let bound = super::status(Some(mock.socket()), &store(tmp.path()), true).await;
        assert_eq!(bound["connected"], json!(true), "{bound}");
        assert_eq!(
            daemon.lock().unwrap().indexed,
            BTreeSet::from(["notes/a.md".to_owned()])
        );
    }

    /// Why: trusty-search answers 200 to a `config.set` whose purge of newly
    /// excluded files failed, and reports it only in `profile_purge.error`.
    /// What: such a reply fails the bind, so the OKG never reads connected.
    #[tokio::test]
    async fn config_set_purge_error_fails_the_bind() {
        let tmp = tempfile::tempdir().unwrap();
        let daemon = Daemon {
            purge_error: Some("could not list indexed files".into()),
            ..legacy(tmp.path())
        };
        let (mock, _daemon) = fake(tmp.path(), daemon).await;
        let bound = super::status(Some(mock.socket()), &store(tmp.path()), true).await;
        assert_eq!(bound["connected"], json!(false), "{bound}");
    }

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
