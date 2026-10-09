//! `index-file` by an absolute path (#9510).
//!
//! Why: `index-file` stored a pushed absolute path verbatim as the chunk key,
//! so the file's root-relative chunks were never replaced, `path_prefix`
//! searches could not find the new ones, and a reindex did not heal it.
//! What: drives the real axum router and the real RPC router from the
//! parent's harness.
//! Test: this file.

use super::*;

/// The `total` the chunk enumeration reports for index `id`.
async fn total_chunks(http: &Router, id: &str) -> u64 {
    let (status, body) = http_bodyless(http, "GET", &format!("/indexes/{id}/chunks")).await;
    assert_eq!(status, StatusCode::OK, "{body}");
    body["total"].as_u64().unwrap_or(0)
}

/// True when a search for `term` scoped to `prefix` returns a chunk holding it.
async fn prefix_search_finds(http: &Router, id: &str, term: &str, prefix: &str) -> bool {
    let query = serde_json::json!({
        "text": term, "top_k": 10, "expand_graph": false, "path_prefix": prefix,
    });
    let body = http_ok(http, "POST", &format!("/indexes/{id}/search"), query).await;
    body["results"].as_array().is_some_and(|hits| {
        hits.iter()
            .any(|hit| hit["content"].as_str().is_some_and(|c| c.contains(term)))
    })
}

/// A planted index over `<tmp>/root` with `src/` on disk, plus an alias
/// symlink `<tmp>/alias -> <tmp>/root`.
fn rooted(tmp: &Path) -> (PathBuf, PathBuf) {
    let root = tmp.join("root");
    std::fs::create_dir_all(root.join("src")).expect("root/src");
    let alias = tmp.join("alias");
    std::os::unix::fs::symlink(&root, &alias).expect("alias");
    (root, alias)
}

/// Why (#9510): an absolute in-root `index-file` path was stored verbatim, so
/// each re-add added a second copy of the file beside its root-relative
/// chunks, and a `path_prefix` search could not reach the new copy.
/// What: the file is indexed by its relative key to set the chunk total. It
/// is then re-added under the raw, canonical and symlinked-root absolute
/// forms, each with a new unique term. After each, the total is unchanged,
/// no chunk sits under the absolute key, and a search scoped by the relative
/// and the absolute prefix finds the new term. Fails before the fix: the
/// total grows with each absolute re-add.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn index_file_by_an_absolute_in_root_path_replaces_its_chunks_9510() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, alias) = rooted(tmp.path());
    let (state, http, rpc) = routers(SearchAppState::new(planted_registry("ia", &root))).await;
    let put = serde_json::json!({ "path": FILE, "content": CONTENT });
    let reply = http_ok(&http, "POST", "/indexes/ia/index-file", put).await;
    assert_eq!(reply["indexed"], true, "{reply}");
    let expected = total_chunks(&http, "ia").await;
    assert!(expected > 0, "the relative write must index something");

    let canonical = std::fs::canonicalize(&root).expect("canonical root");
    let forms = [root.join(FILE), canonical.join(FILE), alias.join(FILE)];
    for (n, absolute) in forms.iter().enumerate() {
        let term = format!("rotate_secret_9510_{n}");
        let content = format!("fn {term}(token: &str) -> bool {{ verify(token) }}\n");
        let absolute = absolute.display().to_string();
        let put = serde_json::json!({ "path": absolute, "content": content });
        let reply = if n == 0 {
            rpc_ok(
                &rpc,
                writes::METHOD_INDEX_FILE_PUT,
                serde_json::json!({ "index_id": "ia", "body": put }),
            )
            .await
        } else {
            http_ok(&http, "POST", "/indexes/ia/index-file", put).await
        };
        assert_eq!(reply["indexed"], true, "{absolute}: {reply}");
        assert_eq!(
            total_chunks(&http, "ia").await,
            expected,
            "{absolute} added chunks instead of replacing them"
        );
        let stray = chunks_for(&state, "ia", &absolute).await;
        assert!(stray.is_empty(), "{absolute} stored its own key: {stray:?}");
        assert!(
            !chunks_for(&state, "ia", FILE).await.is_empty(),
            "{absolute}"
        );
        let abs_prefix = canonical.join("src").display().to_string();
        for prefix in ["src", abs_prefix.as_str()] {
            assert!(
                prefix_search_finds(&http, "ia", &term, prefix).await,
                "{absolute}: path_prefix `{prefix}` missed `{term}`"
            );
        }
    }
}

/// Why (#9510): an absolute path outside the root names no file in the
/// index; `index-file` must refuse it as `remove-file` does (#9236), not
/// store it under a key no search or reindex can reach.
/// What: a foreign path, a real file beside the root, a `..` climb out of
/// the root and through the alias, and the root itself each answer
/// `400 index_file_path_outside_root` with `indexed: false`; the socket
/// renders the same refusal as `CODE_INVALID_PARAMS`; the total is
/// unchanged. Fails before the fix: these answer 403 or 200.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn index_file_refuses_an_absolute_path_outside_the_root_9510() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, alias) = rooted(tmp.path());
    std::fs::write(tmp.path().join("outside.rs"), "fn outside() {}\n").expect("outside");
    let (_state, http, rpc) = routers(SearchAppState::new(planted_registry("ia", &root))).await;
    let put = serde_json::json!({ "path": FILE, "content": CONTENT });
    http_ok(&http, "POST", "/indexes/ia/index-file", put).await;
    let before = total_chunks(&http, "ia").await;

    let root_s = root.display().to_string();
    for outside in [
        "/definitely/not/the/root/src/auth.rs".to_string(),
        tmp.path().join("outside.rs").display().to_string(),
        format!("{root_s}/../outside.rs"),
        alias.join("..").join("outside.rs").display().to_string(),
        format!("{root_s}/"),
    ] {
        let body = serde_json::json!({ "path": outside, "content": CONTENT });
        let over_http = http_err(&http, "POST", "/indexes/ia/index-file", body.clone()).await;
        assert_eq!(
            over_http.0,
            StatusCode::BAD_REQUEST,
            "{outside}: {}",
            over_http.1
        );
        assert_eq!(
            over_http.1["error"], "index_file_path_outside_root",
            "{outside}"
        );
        assert_eq!(over_http.1["indexed"], false, "{outside}");
        let over_socket = rpc_err(
            &rpc,
            writes::METHOD_INDEX_FILE_PUT,
            serde_json::json!({ "index_id": "ia", "body": body }),
        )
        .await;
        assert_same_refusal(
            &over_http,
            &over_socket,
            trusty_common::uds::server::CODE_INVALID_PARAMS,
            &outside,
        );
        assert_eq!(total_chunks(&http, "ia").await, before, "{outside}");
    }
}

/// Why (#9510): the fix normalizes absolute paths only; a relative path must
/// keep its pre-fix behaviour.
/// What: a relative write lands under its own key and echoes it, and a
/// relative `..` climb is still the 403 `index_file_excluded` the admission
/// gate (#8922) answers. Passes before and after the fix by design.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn index_file_keeps_a_relative_path_unchanged_9510() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, _alias) = rooted(tmp.path());
    let (state, http, _rpc) = routers(SearchAppState::new(planted_registry("ia", &root))).await;
    let put = serde_json::json!({ "path": FILE, "content": CONTENT });
    let reply = http_ok(&http, "POST", "/indexes/ia/index-file", put).await;
    assert_eq!(reply["path"], FILE, "{reply}");
    assert!(!chunks_for(&state, "ia", FILE).await.is_empty());

    let climb = serde_json::json!({ "path": "../outside.rs", "content": CONTENT });
    let (status, body) = http_err(&http, "POST", "/indexes/ia/index-file", climb).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "index_file_excluded", "{body}");
}

/// Why (#9510): before the fix, an absolute `index-file` path was stored
/// verbatim. An absolute re-add now writes only the relative key, so a
/// pre-fix copy under the verbatim key stayed and kept answering searches.
/// What: the file is planted under its verbatim absolute key through the
/// indexer, as `remove_file_still_removes_a_pushed_absolute_key_9236` does,
/// which sets the single-copy chunk total. The same file is re-added through
/// `index-file` by that absolute path. The total stays the single-copy total,
/// no chunk is left under the absolute key, and an unscoped search for the
/// file's term finds only chunks whose id names the relative key. Fails
/// before the purge: the old copy stays and the total doubles.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn index_file_by_an_absolute_path_purges_a_pre_fix_verbatim_key_9510() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, _alias) = rooted(tmp.path());
    let (state, http, _rpc) = routers(SearchAppState::new(planted_registry("ia", &root))).await;
    let absolute = root.join(FILE).display().to_string();
    let term = "purge_stale_copy_9510";
    let content = format!("fn {term}(token: &str) -> bool {{ verify(token) }}\n");
    {
        let handle = state.registry.get(&IndexId::new("ia")).expect("resident");
        let indexer = handle.indexer.read().await;
        indexer
            .index_file(&absolute, &content)
            .await
            .expect("plant the pre-#9510 absolute key");
    }
    let single = total_chunks(&http, "ia").await;
    assert!(single > 0, "the planted key must index something");

    let put = serde_json::json!({ "path": absolute, "content": content });
    let reply = http_ok(&http, "POST", "/indexes/ia/index-file", put).await;
    assert_eq!(reply["indexed"], true, "{reply}");

    let query = serde_json::json!({ "text": term, "top_k": 10, "expand_graph": false });
    let body = http_ok(&http, "POST", "/indexes/ia/search", query).await;
    let ids: Vec<String> = body["results"]
        .as_array()
        .unwrap_or_else(|| panic!("no results array: {body}"))
        .iter()
        .filter(|hit| hit["content"].as_str().is_some_and(|c| c.contains(term)))
        .map(|hit| hit["id"].as_str().unwrap_or_default().to_string())
        .collect();
    assert!(
        !ids.is_empty() && ids.iter().all(|id| id.starts_with(FILE)),
        "a search must answer only the relative-key copy; got {ids:?}"
    );
    assert_eq!(
        total_chunks(&http, "ia").await,
        single,
        "the verbatim absolute copy survived the re-add"
    );
    let stray = chunks_for(&state, "ia", &absolute).await;
    assert!(stray.is_empty(), "{absolute} kept its own key: {stray:?}");
}

/// Why (#9510): before the fix, the #8922 gate got the raw path, so an
/// excluded absolute re-add purged the verbatim absolute copy. The gate now
/// gets the relative key, which left a pre-fix absolute copy of an excluded
/// file answering searches.
/// What: a file under the built-in `node_modules` skip dir — one the
/// #8922 table refuses — is planted under its verbatim absolute key through
/// the indexer, then re-added through `index-file` by that absolute path. The
/// reply is 403 `index_file_excluded` with `removed_chunks > 0`, no chunk is
/// left under the absolute key, and the index is empty. Fails before the
/// fix: the planted copy survives the 403.
/// Test: this function IS the test.
#[tokio::test(flavor = "multi_thread")]
async fn index_file_excluded_by_an_absolute_path_purges_a_pre_fix_verbatim_key_9510() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let (root, _alias) = rooted(tmp.path());
    let (state, http, _rpc) = routers(SearchAppState::new(planted_registry("ia", &root))).await;
    let absolute = root.join("node_modules/pkg/auth.rs").display().to_string();
    let content = "fn excluded_stale_copy_9510(token: &str) -> bool { verify(token) }\n";
    {
        let handle = state.registry.get(&IndexId::new("ia")).expect("resident");
        let indexer = handle.indexer.read().await;
        indexer
            .index_file(&absolute, content)
            .await
            .expect("plant the pre-#9510 absolute key");
    }
    assert!(total_chunks(&http, "ia").await > 0, "the plant must index");

    let put = serde_json::json!({ "path": absolute, "content": content });
    let (status, body) = http_err(&http, "POST", "/indexes/ia/index-file", put).await;
    assert_eq!(status, StatusCode::FORBIDDEN, "{body}");
    assert_eq!(body["error"], "index_file_excluded", "{body}");
    assert_eq!(body["path"], absolute.as_str(), "{body}");
    assert!(body["removed_chunks"].as_u64().unwrap_or(0) > 0, "{body}");
    let stray = chunks_for(&state, "ia", &absolute).await;
    assert!(stray.is_empty(), "{absolute} kept its own key: {stray:?}");
    assert_eq!(total_chunks(&http, "ia").await, 0, "a copy survived");
}
