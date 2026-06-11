/// Mode filter (code/text/data/all) and archive downranking tests for the indexer.
use super::*;

#[tokio::test]
async fn test_archive_downrank_demotes_deprecated_chunks() {
    let idx = make_indexer();
    idx.add_chunk(raw("live", "src/auth.rs", "fn authenticate_user_xyz() {}"))
        .await
        .unwrap();
    idx.add_chunk(raw(
        "old",
        "src/legacy/auth_old.rs",
        "fn authenticate_user_xyz_old() {}",
    ))
    .await
    .unwrap();
    let results = idx
        .search(&SearchQuery {
            text: "authenticate_user_xyz".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let pos_live = results.iter().position(|c| c.id == "live");
    let pos_old = results.iter().position(|c| c.id == "old");
    assert!(pos_live.is_some(), "live chunk missing from results");
    assert!(pos_old.is_some(), "archived chunk missing from results");
    assert!(
        pos_live.unwrap() < pos_old.unwrap(),
        "live chunk should outrank archived chunk: live={pos_live:?} old={pos_old:?}"
    );
    let old_chunk = results.iter().find(|c| c.id == "old").unwrap();
    assert!(
        old_chunk.archive_reason.is_some(),
        "archived chunk missing archive_reason: {:?}",
        old_chunk
    );
    let reason = old_chunk.archive_reason.as_deref().unwrap();
    assert!(
        reason.starts_with("path:"),
        "expected path-prefix reason, got {reason}"
    );
}

#[tokio::test]
async fn test_exclude_archived_drops_archive_chunks() {
    let idx = make_indexer();
    idx.add_chunk(raw("live", "src/auth.rs", "fn authenticate_user_xyz() {}"))
        .await
        .unwrap();
    for (id, path) in [
        ("a1", "src/_archive/auth.rs"),
        ("a2", "src/archive/auth.rs"),
        ("a3", "src/_deprecated/auth.rs"),
        ("a4", "src/old/auth.rs"),
        ("a5", "src/.archive/auth.rs"),
    ] {
        idx.add_chunk(raw(id, path, "fn authenticate_user_xyz_old() {}"))
            .await
            .unwrap();
    }

    let downranked = idx
        .search(&SearchQuery {
            text: "authenticate_user_xyz".to_string(),
            top_k: 10,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        downranked.iter().any(|c| c.id.starts_with('a')),
        "pre-condition: archived chunks should be present (downranked) without the flag"
    );

    let filtered = idx
        .search(&SearchQuery {
            text: "authenticate_user_xyz".to_string(),
            top_k: 10,
            expand_graph: false,
            compact: false,
            exclude_archived: true,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        filtered.iter().all(|c| c.id == "live"),
        "exclude_archived must drop every archived chunk; got {:?}",
        filtered.iter().map(|c| &c.file).collect::<Vec<_>>()
    );
    assert!(filtered.iter().any(|c| c.id == "live"));
}

#[tokio::test]
async fn test_archive_downrank_skips_clean_chunks() {
    let idx = make_indexer();
    idx.add_chunk(raw("clean", "src/main.rs", "fn run_main() {}"))
        .await
        .unwrap();
    let results = idx
        .search(&SearchQuery {
            text: "run_main".to_string(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    let chunk = results.iter().find(|c| c.id == "clean").unwrap();
    assert!(chunk.archive_reason.is_none());
}

#[tokio::test]
async fn test_mode_filter_code_returns_only_source() {
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Code,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    let lib_abs = abs("src/lib.rs");
    let license_abs = abs("LICENSE");
    assert!(
        files.contains(&lib_abs.as_str()),
        "code mode must include source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".md")),
        "code mode must exclude .md: {files:?}"
    );
    assert!(
        !files.contains(&license_abs.as_str()),
        "code mode must exclude named docs: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".toml")),
        "code mode must exclude config: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".json")),
        "code mode must exclude data: {files:?}"
    );
}

#[tokio::test]
async fn test_mode_filter_text_returns_only_prose_and_named_docs() {
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Text,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    let license_abs = abs("LICENSE");
    assert!(
        files.iter().any(|f| f.ends_with(".md")),
        "text mode must include prose: {files:?}"
    );
    assert!(
        files.contains(&license_abs.as_str()),
        "text mode must include named docs without extension: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".rs")),
        "text mode must exclude source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".toml")),
        "text mode must exclude config: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".json")),
        "text mode must exclude data: {files:?}"
    );
}

#[tokio::test]
async fn test_mode_filter_data_returns_only_structured_data() {
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::Data,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<&str> = results.iter().map(|c| c.file.as_str()).collect();
    assert!(
        files.iter().any(|f| f.ends_with(".toml")),
        "data mode must include config: {files:?}"
    );
    assert!(
        files.iter().any(|f| f.ends_with(".json")),
        "data mode must include data files: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".rs")),
        "data mode must exclude source: {files:?}"
    );
    assert!(
        !files.iter().any(|f| f.ends_with(".md")),
        "data mode must exclude prose: {files:?}"
    );
    assert!(
        !files.contains(&abs("LICENSE").as_str()),
        "data mode must exclude named docs: {files:?}"
    );
}

#[tokio::test]
async fn test_mode_filter_all_returns_everything() {
    let idx = make_indexer();
    seed_mode_filter_corpus(&idx).await;
    let q = SearchQuery {
        text: "alpha_qwerty".to_string(),
        top_k: 20,
        expand_graph: false,
        compact: false,
        mode: SearchMode::All,
        ..Default::default()
    };
    let results = idx.search(&q).await.unwrap();
    let files: Vec<String> = results.iter().map(|c| c.file.clone()).collect();
    for expected_rel in &[
        "src/lib.rs",
        "docs/intro.md",
        "LICENSE",
        "Cargo.toml",
        "fixtures/alpha.json",
    ] {
        let expected = abs(expected_rel);
        assert!(
            files.contains(&expected),
            "all mode must include {expected}: {files:?}"
        );
    }
}
