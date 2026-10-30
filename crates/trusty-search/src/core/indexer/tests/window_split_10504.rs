//! #10504: the deferred parse splits new chunks at the embedder window, and the
//! catch-up heads each owed chunk that fits.
//!
//! The rig is #10481's (`MockEmbedder` window 20, one token per word plus two
//! special tokens). A guard says whether it must hold before the change too.

use super::super::ingest::window_split::{parse_switch, split_at_window};
use super::window_split_10481::{
    long_chunk, long_fn, rig, roots, text, tokenizer, DIM, OVERHEAD, WINDOW,
};
use super::*;
use trusty_common::embedder::{ChunkError, ChunkTokenizer};

/// A chunking mock whose `split_window` is fixed, while `input_window` stays
/// unreported: a lazy sidecar that has not answered yet.
struct Assumed {
    inner: MockEmbedder,
    split: Result<Option<usize>, ()>,
}

#[async_trait::async_trait]
impl Embedder for Assumed {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Embedder::embed(&self.inner, text).await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Embedder::embed_batch(&self.inner, texts).await
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn split_window(
        &self,
    ) -> Result<Option<(usize, Arc<ChunkTokenizer>)>, trusty_common::embedder::ChunkError> {
        match self.split {
            Ok(Some(w)) => Ok(Some((w, tokenizer()))),
            Ok(None) => Ok(None),
            Err(()) => Err(ChunkError::Encode("no tokenizer in the cache".into())),
        }
    }
}

fn assumed(split: Result<Option<usize>, ()>) -> CodeIndexer {
    let embedder: Arc<dyn Embedder> = Arc::new(Assumed {
        inner: MockEmbedder::new(DIM),
        split,
    });
    let store: Arc<dyn crate::core::store::VectorStore> =
        Arc::new(UsearchStore::new(DIM).expect("usearch new"));
    CodeIndexer::new("w10504-assumed", "/tmp/test").with_components(embedder, store)
}

fn file(content: &str) -> Vec<(String, String)> {
    vec![("src/a.rs".to_string(), content.to_string())]
}

/// Embedded texts, the reindex's own `warm` probe left out.
fn embedded(seen: &std::sync::Mutex<Vec<String>>) -> Vec<String> {
    let seen = seen.lock().expect("seen").clone();
    seen.into_iter().filter(|t| t != "warm").collect()
}

/// A long function parses into windows that each fit, the catch-up gives each a
/// vector, and the unsplit parent id is gone.
#[tokio::test]
async fn a_deferred_index_splits_a_new_long_chunk_once_the_window_is_known() {
    let r = rig(Some(WINDOW));
    let whole = r
        .indexer
        .parse_files_only(file(&long_fn("zebracrossing")))
        .await
        .expect("parse only");
    assert_eq!(whole.chunks.len(), 1, "the plain parse keeps one chunk");
    let parent_id = whole.chunks[0].id.clone();

    let parsed = r
        .indexer
        .parse_files_deferred(file(&long_fn("zebracrossing")), true)
        .await
        .expect("deferred parse");
    assert!(parsed.chunks.len() > 1, "a long function splits");
    assert!(
        parsed.embeddings.iter().all(Option::is_none),
        "no embed yet"
    );
    assert_eq!(parsed.embeddings.len(), parsed.chunks.len());
    let tok = tokenizer();
    for c in &parsed.chunks {
        let n = tok.count_tokens(&c.content).expect("count") + OVERHEAD;
        assert!(n <= WINDOW, "window of {n} tokens exceeds {WINDOW}");
    }
    let ids: Vec<String> = parsed.chunks.iter().map(|c| c.id.clone()).collect();
    r.indexer
        .commit_parsed_batch(parsed, false)
        .await
        .expect("commit");
    r.indexer
        .embed_deferred_chunks_gated(None, None)
        .await
        .expect("catch-up");
    assert!(r.store.contains_many(&ids).await.iter().all(|&b| b));
    let stored = r.indexer.raw_chunks_snapshot().await.expect("chunks");
    assert_eq!(stored.len(), ids.len());
    assert!(stored.iter().all(|c| c.id != parent_id), "no parent row");
    assert_eq!(r.indexer.embed_window_stats().chunks_split(), 1);
}

/// With the window not yet reported, the embedder's assumed split window still
/// splits the deferred parse, and the same window reaches the inline path.
#[tokio::test]
async fn a_deferred_parse_splits_on_the_assumed_window_before_the_embedder_reports() {
    let indexer = assumed(Ok(Some(WINDOW)));
    let parsed = indexer
        .parse_files_deferred(file(&long_fn("zebracrossing")), true)
        .await
        .expect("deferred parse");
    assert!(parsed.chunks.len() > 1, "the assumed window split it");
    let (out, headers) = indexer
        .split_for_embed(vec![long_chunk("p", &text(10))])
        .await
        .expect("inline split");
    assert!(out.len() > 1 && headers.is_some(), "index_file splits too");
}

/// No window, or a tokenizer that cannot load: today's output.
/// Guard: passes before the change too, since nothing splits there.
#[tokio::test]
async fn with_no_window_and_no_tokenizer_a_deferred_parse_does_not_split() {
    for indexer in [assumed(Ok(None)), assumed(Err(())), rig(None).indexer] {
        let parsed = indexer
            .parse_files_deferred(file(&long_fn("zebracrossing")), true)
            .await
            .expect("deferred parse");
        assert_eq!(parsed.chunks.len(), 1, "no split without a window");
        assert_eq!(indexer.embed_window_stats().chunks_split(), 0);
    }
}

/// The catch-up embeds every window with the header; storage and BM25 never
/// carry it.
#[tokio::test]
async fn the_catch_up_embeds_each_window_with_the_header_and_stores_none() {
    let r = rig(Some(WINDOW));
    let parsed = r
        .indexer
        .parse_files_deferred(file(&long_fn("zebracrossing")), true)
        .await
        .expect("deferred parse");
    assert!(parsed.chunks.len() > 1);
    r.indexer
        .commit_parsed_batch(parsed, false)
        .await
        .expect("commit");
    r.indexer
        .embed_deferred_chunks_gated(None, None)
        .await
        .expect("catch-up");
    let texts = embedded(&r.seen);
    assert!(!texts.is_empty());
    for t in &texts {
        assert!(
            t.starts_with("// path: src/a.rs :: Function long_one\n"),
            "{t}"
        );
    }
    for c in r.indexer.raw_chunks_snapshot().await.expect("chunks") {
        assert!(!c.content.contains("// path:"), "stored: {}", c.content);
    }
    let hits = r.indexer.bm25_search("path", 10, None).await.expect("bm25");
    assert!(hits.is_empty(), "BM25 saw the header: {hits:?}");
}

/// A window cut headerless (the header left under half the window) is not
/// given a header by the catch-up either.
#[tokio::test]
async fn a_window_cut_without_a_header_is_embedded_without_one() {
    let r = rig(Some(WINDOW));
    let mut parent = long_chunk("p", &text(10));
    parent.function_name = Some("a b c d e f g h i j k l".into());
    parent.chunk_type = crate::core::chunker::ChunkType::Function;
    let (windows, cut_headers, _) = split_at_window(&tokenizer(), WINDOW, &roots(), vec![parent]);
    assert!(windows.len() > 1);
    assert!(cut_headers.iter().all(String::is_empty), "cut headerless");
    let headers = r.indexer.catch_up_headers(&windows).await.expect("headers");
    assert!(headers.iter().all(String::is_empty), "{headers:?}");
}

/// A short owed chunk is headed; an old oversize one is left as stored.
#[tokio::test]
async fn a_short_owed_chunk_gets_a_header_and_an_oversize_old_one_does_not() {
    let r = rig(Some(WINDOW));
    let short = long_chunk("s", &text(2));
    let old = long_chunk("o", &text(10));
    let headers = r
        .indexer
        .catch_up_headers(&[short, old])
        .await
        .expect("headers");
    assert_eq!(
        headers,
        vec!["// path: src/a.rs\n".to_string(), String::new()]
    );
}

/// The kill switch: only 0, false and off turn the split off, and `split`
/// false parses as before.
#[tokio::test]
async fn a_kill_switch_off_parses_deferred_unsplit() {
    assert!(parse_switch(None) && parse_switch(Some("1")) && parse_switch(Some("")));
    for off in ["0", "false", " OFF "] {
        assert!(!parse_switch(Some(off)), "{off:?}");
    }
    let r = rig(Some(WINDOW));
    let parsed = r
        .indexer
        .parse_files_deferred(file(&long_fn("zebracrossing")), false)
        .await
        .expect("deferred parse");
    assert_eq!(parsed.chunks.len(), 1, "switch off: one chunk per function");
    assert_eq!(r.indexer.embed_window_stats().chunks_split(), 0);
}

/// A catch-up that fails leaves windows, BM25 and corpus agreeing, and the
/// next pass embeds exactly the windows still owed.
#[tokio::test]
async fn a_failed_catch_up_leaves_a_consistent_state_and_resumes() {
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
    struct Flaky {
        inner: MockEmbedder,
        fail: Arc<AtomicBool>,
        texts: Arc<AtomicUsize>,
    }
    #[async_trait::async_trait]
    impl Embedder for Flaky {
        async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
            Embedder::embed(&self.inner, text).await
        }
        async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
            if self.fail.load(Ordering::SeqCst) {
                anyhow::bail!("injected embed failure");
            }
            self.texts.fetch_add(texts.len(), Ordering::SeqCst);
            Embedder::embed_batch(&self.inner, texts).await
        }
        fn dimension(&self) -> usize {
            DIM
        }
        fn input_window(&self) -> Option<usize> {
            Embedder::input_window(&self.inner)
        }
        fn chunking_tokenizer(
            &self,
        ) -> Result<Option<Arc<ChunkTokenizer>>, trusty_common::embedder::ChunkError> {
            Embedder::chunking_tokenizer(&self.inner)
        }
    }
    let fail = Arc::new(AtomicBool::new(true));
    let texts = Arc::new(AtomicUsize::new(0));
    let embedder: Arc<dyn Embedder> = Arc::new(Flaky {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        fail: Arc::clone(&fail),
        texts: Arc::clone(&texts),
    });
    let store: Arc<dyn crate::core::store::VectorStore> =
        Arc::new(UsearchStore::new(DIM).expect("usearch new"));
    let indexer =
        CodeIndexer::new("w10504-flaky", "/tmp/test").with_components(embedder, Arc::clone(&store));
    let parsed = indexer
        .parse_files_deferred(file(&long_fn("zebracrossing")), true)
        .await
        .expect("deferred parse");
    let n = parsed.chunks.len();
    assert!(n > 1);
    indexer
        .commit_parsed_batch(parsed, false)
        .await
        .expect("commit");
    let _ = indexer.embed_deferred_chunks_gated(None, None).await;
    assert_eq!(store.len().await.expect("len"), 0, "nothing embedded");
    assert_eq!(indexer.pending_embed_count().await, n, "all windows owed");
    let ids: Vec<String> = indexer
        .raw_chunks_snapshot()
        .await
        .expect("chunks")
        .into_iter()
        .map(|c| c.id)
        .collect();
    assert_eq!(ids.len(), n, "corpus holds the windows, no parent");
    let bm25 = indexer
        .bm25_search("long_one", 50, None)
        .await
        .expect("bm25");
    assert!(
        bm25.iter().all(|h| ids.contains(&h.0)),
        "BM25 ids are window ids"
    );

    fail.store(false, Ordering::SeqCst);
    indexer
        .embed_deferred_chunks_gated(None, None)
        .await
        .expect("resumed pass");
    assert_eq!(texts.load(Ordering::SeqCst), n, "exactly the owed windows");
    assert_eq!(indexer.pending_embed_count().await, 0);
}

/// A chunk stored before the change keeps its id and is embedded bare, while a
/// new file parsed beside it splits (guard for the old chunk: the same before).
#[tokio::test]
async fn a_pre_upgrade_oversize_chunk_keeps_its_id_and_is_not_split() {
    let r = rig(Some(WINDOW));
    let old = r
        .indexer
        .parse_files_only(vec![("src/old.rs".to_string(), long_fn("oldmarker"))])
        .await
        .expect("old parse");
    let old_id = old.chunks[0].id.clone();
    r.indexer
        .commit_parsed_batch(old, false)
        .await
        .expect("commit old");
    let fresh = r
        .indexer
        .parse_files_deferred(vec![("src/new.rs".to_string(), long_fn("newmarker"))], true)
        .await
        .expect("new parse");
    assert!(fresh.chunks.len() > 1);
    r.indexer
        .commit_parsed_batch(fresh, false)
        .await
        .expect("commit new");
    r.indexer
        .embed_deferred_chunks_gated(None, None)
        .await
        .expect("catch-up");
    let chunks = r.indexer.raw_chunks_snapshot().await.expect("chunks");
    let kept: Vec<_> = chunks.iter().filter(|c| c.id == old_id).collect();
    assert_eq!(kept.len(), 1, "the old chunk keeps its id");
    assert!(kept[0].content.contains("oldmarker"));
    assert!(
        r.store.contains_many(&[old_id]).await[0],
        "and gets a vector"
    );
    let texts = embedded(&r.seen);
    let bare = texts.iter().filter(|t| !t.starts_with("// path:")).count();
    assert_eq!(bare, 1, "only the old oversize chunk embeds bare");
    assert_eq!(r.indexer.embed_window_stats().chunks_split(), 1);
}
