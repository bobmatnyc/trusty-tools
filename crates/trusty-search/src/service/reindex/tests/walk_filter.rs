/// Walk/filter wiring tests for the reindex pipeline.
use super::*;

#[tokio::test]
async fn reindex_honours_include_paths_filter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::create_dir_all(root.join("api")).unwrap();
    fs::create_dir_all(root.join("ui")).unwrap();
    fs::write(root.join("api/keep.rs"), "fn keep_me() {}\n").unwrap();
    fs::write(root.join("ui/drop.rs"), "fn drop_me() {}\n").unwrap();

    let indexer = CodeIndexer::new("filter-test", root.clone());
    let handle = Arc::new(IndexHandle {
        id: IndexId::new("filter-test"),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: root.clone(),
        include_paths: vec![root.join("api")],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: false,
        respect_gitignore: true,
        path_filter: vec![],
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        defer_embed: true,
        stages: Arc::new(tokio::sync::RwLock::new(IndexStages::default())),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    });
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex(handle.clone(), progress.clone(), false);

    for _ in 0..100 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        1,
        "only api/keep.rs should be walked"
    );

    let idx = handle.indexer.read().await;
    let r = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "keep_me".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(r.iter().any(|c| c.content.contains("keep_me")));
    let r2 = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "drop_me".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        !r2.iter().any(|c| c.content.contains("drop_me")),
        "ui/drop.rs must not have been indexed"
    );
}

#[tokio::test]
async fn reindex_honours_path_filter() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    std::fs::create_dir_all(root.join("common-utils")).unwrap();
    std::fs::create_dir_all(root.join("other-repo")).unwrap();
    std::fs::write(root.join("common-utils/keep.rs"), "fn keep_common() {}\n").unwrap();
    std::fs::write(root.join("other-repo/drop.rs"), "fn drop_other() {}\n").unwrap();

    let indexer = CodeIndexer::new("pf-test", root.clone());
    let handle = Arc::new(IndexHandle {
        id: IndexId::new("pf-test"),
        indexer: Arc::new(tokio::sync::RwLock::new(indexer)),
        root_path: root.clone(),
        include_paths: vec![],
        exclude_globs: vec![],
        extensions: vec![],
        domain_terms: vec![],
        include_docs: false,
        respect_gitignore: true,
        path_filter: vec!["common-*".to_string()],
        context_embedding: Arc::new(tokio::sync::RwLock::new(None)),
        context_summary: Arc::new(tokio::sync::RwLock::new(None)),
        indexed_head_sha: Arc::new(tokio::sync::RwLock::new(None)),
        last_indexed_at: Arc::new(tokio::sync::RwLock::new(None)),
        lexical_only: false,
        skip_kg: false,
        defer_embed: true,
        stages: Arc::new(tokio::sync::RwLock::new(IndexStages::default())),
        search_pressure: Arc::new(tokio::sync::Notify::new()),
        walk_diagnostics: Arc::new(tokio::sync::RwLock::new(
            crate::core::registry::WalkDiagnostics::default(),
        )),
    });
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex(handle.clone(), progress.clone(), false);

    for _ in 0..100 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    assert_eq!(
        progress.total_files.load(Ordering::Acquire),
        1,
        "only common-utils/keep.rs should pass the path_filter"
    );

    let idx = handle.indexer.read().await;
    let r = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "keep_common".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(r.iter().any(|c| c.content.contains("keep_common")));
    let r2 = idx
        .search(&crate::core::indexer::SearchQuery {
            text: "drop_other".into(),
            top_k: 5,
            expand_graph: false,
            compact: false,
            ..Default::default()
        })
        .await
        .unwrap();
    assert!(
        !r2.iter().any(|c| c.content.contains("drop_other")),
        "other-repo must not have been indexed"
    );
}

#[tokio::test]
async fn reindex_walks_directory_and_emits_events() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let root = tmp.path().to_path_buf();
    fs::write(root.join("a.rs"), "fn a() {}").unwrap();
    fs::write(root.join("b.py"), "def b():\n    pass\n").unwrap();
    fs::create_dir(root.join("target")).unwrap();
    fs::write(root.join("target/skip.rs"), "fn skip() {}").unwrap();

    let indexer = CodeIndexer::new("test".to_string(), root.clone());
    let handle = Arc::new(IndexHandle::bare(
        IndexId::new("test"),
        Arc::new(tokio::sync::RwLock::new(indexer)),
        root.clone(),
    ));
    let progress = Arc::new(ReindexProgress::new());
    spawn_reindex(handle, progress.clone(), false);

    for _ in 0..100 {
        if progress.status.load() == ReindexStatus::Complete {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
    }
    assert_eq!(progress.status.load(), ReindexStatus::Complete);
    assert_eq!(progress.total_files.load(Ordering::Acquire), 2);
    assert_eq!(progress.indexed.load(Ordering::Acquire), 2);

    let events = progress.events.lock().await;
    assert!(
        events
            .first()
            .map(|s| s.contains("\"walk_complete\""))
            .unwrap_or(false),
        "first event must be walk_complete (issue #317); got: {:?}",
        events.first()
    );
    assert!(
        events
            .get(1)
            .map(|s| s.contains("\"start\""))
            .unwrap_or(false),
        "second event must be start; got: {:?}",
        events.get(1)
    );
    assert!(
        events
            .last()
            .map(|s| s.contains("\"complete\""))
            .unwrap_or(false),
        "last event must be complete; got: {:?}",
        events.last()
    );
}
