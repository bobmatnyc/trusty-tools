//! #10481: new and changed chunks are split at the embedder window, and the
//! text embedded carries a path/symbol header that is never stored.
//!
//! The chunking `MockEmbedder` counts one token per whitespace word plus
//! `[CLS]`/`[SEP]` (2), and its vectors are a bag of the first `window - 2`
//! words, so a word past the window is invisible to the vector lane unless the
//! chunk was split.

use super::super::ingest::window_split::{embed_header, header_path, split_at_window};
use super::*;
use crate::core::chunker::ChunkType;
use crate::core::index_roots::IndexRoots;
use std::sync::Mutex;
use trusty_common::embedder::CHUNK_OVERLAP_PERCENT;

// Wide, so a hash collision between words cannot outrank the window that holds them.
const DIM: usize = 4096;
const WINDOW: usize = 20;
const OVERHEAD: usize = 2;

/// `lines` lines of five words each; word `i` is `w{i}`.
fn text(lines: usize) -> String {
    (0..lines)
        .map(|l| {
            (0..5)
                .map(|w| format!("w{}", l * 5 + w))
                .collect::<Vec<_>>()
                .join(" ")
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn long_chunk(id: &str, content: &str) -> RawChunk {
    let mut c = raw(id, "src/a.rs", content);
    c.start_line = 1;
    c.end_line = content.lines().count();
    c
}

fn roots() -> IndexRoots {
    IndexRoots::new(std::path::PathBuf::from("/tmp/test"), Vec::new())
}

fn tokenizer() -> Arc<trusty_common::embedder::ChunkTokenizer> {
    MockEmbedder::new(DIM)
        .with_chunking(WINDOW)
        .chunking_tokenizer()
        .expect("tokenizer")
        .expect("a chunking mock has one")
}

fn words(s: &str) -> Vec<&str> {
    s.split_whitespace().collect()
}

/// Records every text it embeds, over a chunking mock.
struct Recording {
    inner: MockEmbedder,
    seen: Arc<Mutex<Vec<String>>>,
}

#[async_trait::async_trait]
impl Embedder for Recording {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Embedder::embed(&self.inner, text).await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.seen
            .lock()
            .expect("seen lock")
            .extend(texts.iter().map(|t| (*t).to_string()));
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
    ) -> Result<
        Option<Arc<trusty_common::embedder::ChunkTokenizer>>,
        trusty_common::embedder::ChunkError,
    > {
        Embedder::chunking_tokenizer(&self.inner)
    }
}

struct Rig {
    indexer: CodeIndexer,
    store: Arc<dyn VectorStore>,
    seen: Arc<Mutex<Vec<String>>>,
}

fn rig(window: Option<usize>) -> Rig {
    let mock = match window {
        Some(w) => MockEmbedder::new(DIM).with_chunking(w),
        None => MockEmbedder::new(DIM),
    };
    let seen = Arc::new(Mutex::new(Vec::new()));
    let embedder: Arc<dyn Embedder> = Arc::new(Recording {
        inner: mock,
        seen: Arc::clone(&seen),
    });
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch new"));
    let indexer =
        CodeIndexer::new("w10481", "/tmp/test").with_components(embedder, Arc::clone(&store));
    Rig {
        indexer,
        store,
        seen,
    }
}

/// A Rust function whose last comment line holds a word nothing else has.
fn long_fn(marker: &str) -> String {
    let body: Vec<String> = (0..40).map(|i| format!("    let v{i} = {i};")).collect();
    format!(
        "fn long_one() {{\n{}\n    // {marker}\n}}\n",
        body.join("\n")
    )
}

/// Each window fits with its header, and together they cover the text.
#[test]
fn a_chunk_over_the_window_splits_into_windows_that_each_fit() {
    let tok = tokenizer();
    let parent = long_chunk("p", &text(10));
    let header = embed_header(&parent, &parent.file);
    let header_tokens = tok.count_tokens(&header).expect("count");
    let (out, _h, report) = split_at_window(&tok, WINDOW, &roots(), vec![parent.clone()]);
    assert_eq!(report.parents_split, 1);
    assert!(out.len() > 1, "50 words cannot fit {WINDOW} tokens");
    for w in &out {
        let n = tok.count_tokens(&w.content).expect("count") + header_tokens + OVERHEAD;
        assert!(n <= WINDOW, "window of {n} tokens exceeds {WINDOW}");
        assert!(w.start_line >= 1 && w.end_line <= 10 && w.start_line <= w.end_line);
    }
    assert_eq!(words(&out[0].content)[0], "w0");
    assert_eq!(
        words(&out.last().expect("last").content).last(),
        Some(&"w49")
    );
    assert_eq!(
        out.last().expect("last").end_line,
        10,
        "the span reaches the end"
    );
    let ids: std::collections::HashSet<&str> = out.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), out.len(), "window ids are distinct");
}

/// Consecutive windows share the common chunker's overlap and leave no gap.
#[test]
fn consecutive_windows_overlap_by_the_common_chunker_constant() {
    let tok = tokenizer();
    let parent = long_chunk("p", &text(10));
    let header_tokens = tok
        .count_tokens(&embed_header(&parent, &parent.file))
        .expect("count");
    let budget = WINDOW - header_tokens - OVERHEAD;
    let overlap = budget * CHUNK_OVERLAP_PERCENT / 100;
    let (out, _h, _) = split_at_window(&tok, WINDOW, &roots(), vec![parent]);
    assert!(overlap >= 1, "the fixture must exercise an overlap");
    for pair in out.windows(2) {
        let (a, b) = (words(&pair[0].content), words(&pair[1].content));
        assert_eq!(
            &a[a.len() - overlap..],
            &b[..overlap],
            "overlap of {overlap}"
        );
    }
}

/// A chunk that fits keeps its id and content, and nothing is counted.
#[test]
fn a_chunk_within_the_window_keeps_its_id_and_content() {
    let tok = tokenizer();
    let short = long_chunk("short-id", &text(2));
    let (out, _h, report) = split_at_window(&tok, WINDOW, &roots(), vec![short.clone()]);
    assert_eq!(report.parents_split, 0);
    assert_eq!(out.len(), 1);
    assert_eq!(
        (out[0].id.as_str(), out[0].content.as_str()),
        ("short-id", short.content.as_str())
    );
}

/// With no room past the header the chunk stays whole and is reported.
#[test]
fn a_window_that_cannot_be_split_keeps_the_chunk_whole() {
    let tok = tokenizer();
    let parent = long_chunk("p", &text(10));
    let (out, _h, report) = split_at_window(&tok, 5, &roots(), vec![parent.clone()]);
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, "p");
    assert_eq!(report.kept_whole, 1);
    assert!(report.first_error.is_some());
}

/// One physical line: every window has the same span, so ids take `::dup::`.
#[test]
fn windows_of_a_one_line_chunk_get_distinct_ids() {
    let tok = tokenizer();
    let one_line = (0..60)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ");
    let mut parent = long_chunk("p", &one_line);
    parent.end_line = 1;
    let (out, _h, _) = split_at_window(&tok, WINDOW, &roots(), vec![parent]);
    assert!(out.len() > 2);
    let ids: std::collections::HashSet<&str> = out.iter().map(|c| c.id.as_str()).collect();
    assert_eq!(ids.len(), out.len());
    assert!(
        out[1..].iter().all(|c| c.id.contains("::dup::")),
        "{:?}",
        out.iter().map(|c| &c.id).collect::<Vec<_>>()
    );
}

/// The first window keeps the edges; every window keeps name, type, language.
#[test]
fn edges_go_to_window_zero_and_identity_to_all() {
    let tok = tokenizer();
    let mut parent = long_chunk("p", &text(10));
    parent.function_name = Some("long_one".into());
    parent.chunk_type = ChunkType::Function;
    parent.calls = vec!["callee".into()];
    parent.virtual_terms = vec!["term".into()];
    let (out, _h, _) = split_at_window(&tok, WINDOW, &roots(), vec![parent]);
    assert_eq!(out[0].calls, vec!["callee"]);
    assert!(out[1..]
        .iter()
        .all(|c| c.calls.is_empty() && c.virtual_terms.is_empty()));
    assert!(out
        .iter()
        .all(|c| c.function_name.as_deref() == Some("long_one")
            && c.chunk_type == ChunkType::Function
            && c.language.as_deref() == Some("rust")));
}

/// The header names the path and, for a named chunk, the kind and symbol.
#[test]
fn the_header_names_the_path_and_symbol() {
    let mut c = long_chunk("p", "x");
    assert_eq!(embed_header(&c, &c.file), "// path: src/a.rs\n");
    c.function_name = Some("Type::method".into());
    c.chunk_type = ChunkType::Method;
    assert_eq!(
        embed_header(&c, &c.file),
        "// path: src/a.rs :: Method Type::method\n"
    );
}

/// No window, no split: today's output, and no header.
#[tokio::test]
async fn an_embedder_with_no_window_does_not_split() {
    let r = rig(None);
    let chunks = vec![long_chunk("p", &text(10))];
    let (out, headers) = r.indexer.split_for_embed(chunks).await.expect("split");
    assert!(headers.is_none());
    assert_eq!(out.len(), 1);
    assert_eq!(out[0].id, "p");
    assert_eq!(r.indexer.embed_window_stats().chunks_split(), 0);
}

/// The vector lane finds text past the window through the window that holds it.
#[tokio::test]
async fn a_query_on_text_past_the_window_hits_its_window_chunk() {
    let r = rig(Some(WINDOW));
    let content = long_fn("tapirnose okapispot");
    r.indexer
        .index_file("src/a.rs", &content)
        .await
        .expect("index");
    let q = Embedder::embed(
        &MockEmbedder::new(DIM).with_chunking(WINDOW),
        "tapirnose okapispot",
    )
    .await
    .expect("query embed");
    let hits = r.store.search(&q, 1).await.expect("vector search");
    let chunks = r.indexer.raw_chunks_snapshot().await.expect("chunks");
    let top = chunks
        .iter()
        .find(|c| c.id == hits[0].chunk_id)
        .expect("the hit is in the corpus");
    assert!(
        top.content.contains("okapispot"),
        "the hit holds the text: {:?}",
        top.content
    );
    assert!(
        top.start_line > 1,
        "the hit is a tail window, not the whole function"
    );
    assert!(chunks.len() > 1, "the function was split");
}

/// The embedder sees the header on every text; storage and BM25 never do.
#[tokio::test]
async fn the_header_is_embedded_but_never_stored() {
    let r = rig(Some(WINDOW));
    r.indexer
        .index_file("src/a.rs", &long_fn("zebracrossing"))
        .await
        .expect("index");
    let seen = r.seen.lock().expect("seen").clone();
    let embedded: Vec<&String> = seen.iter().filter(|t| t.as_str() != "warm").collect();
    assert!(!embedded.is_empty());
    for t in &embedded {
        assert!(
            t.starts_with("// path: src/a.rs :: Function long_one\n"),
            "{t}"
        );
        assert!(
            words(t).len() + OVERHEAD <= WINDOW,
            "embedded {t:?} exceeds the window"
        );
    }
    for c in r.indexer.raw_chunks_snapshot().await.expect("chunks") {
        assert!(
            !c.content.contains("// path:"),
            "stored text carries the header: {}",
            c.content
        );
    }
    let hits = r.indexer.bm25_search("path", 10, None).await.expect("bm25");
    assert!(hits.is_empty(), "BM25 saw the header: {hits:?}");
}

/// A changed file's old chunk is replaced by the new windows.
#[tokio::test]
async fn a_changed_file_supersedes_its_old_chunk_with_windows() {
    let r = rig(Some(WINDOW));
    r.indexer
        .index_file("src/a.rs", &long_fn("firstmarker"))
        .await
        .expect("v1");
    r.indexer
        .index_file("src/a.rs", &long_fn("secondmarker"))
        .await
        .expect("v2");
    let chunks = r.indexer.raw_chunks_snapshot().await.expect("chunks");
    assert!(chunks.len() > 1);
    assert!(chunks.iter().all(|c| !c.content.contains("firstmarker")));
    assert!(chunks.iter().any(|c| c.content.contains("secondmarker")));
    let ids = r.indexer.chunk_ids_for_file("src/a.rs").await;
    assert_eq!(ids.len(), chunks.len(), "no stale id survives");
}

/// A chunk committed before the change keeps its id and vector, and a later
/// write of another file does not touch it (guard: green before and after).
#[tokio::test]
async fn an_existing_long_chunk_keeps_its_id_and_vector() {
    let r = rig(Some(WINDOW));
    let old = long_chunk("old-long-id", &text(10));
    r.indexer
        .add_chunk(old.clone())
        .await
        .expect("add old chunk");
    r.indexer
        .index_file("src/other.rs", &long_fn("othermarker"))
        .await
        .expect("other");
    let chunks = r.indexer.raw_chunks_snapshot().await.expect("chunks");
    let kept = chunks
        .iter()
        .find(|c| c.id == "old-long-id")
        .expect("old chunk kept");
    assert_eq!(kept.content, old.content, "content unchanged");
    assert!(
        r.store.contains_many(&["old-long-id".to_string()]).await[0],
        "vector kept"
    );
}

/// The deferred catch-up embeds corpus chunks as stored: no split, no header
/// (guard: green before and after).
#[tokio::test]
async fn a_deferred_catch_up_pass_never_splits_corpus_chunks() {
    let r = rig(Some(WINDOW));
    let parsed = r
        .indexer
        .parse_files_only(vec![("src/a.rs".to_string(), long_fn("zebracrossing"))])
        .await
        .expect("parse only");
    assert_eq!(
        parsed.chunks.len(),
        1,
        "the unembedded fast pass keeps one chunk"
    );
    let id = parsed.chunks[0].id.clone();
    r.indexer
        .commit_parsed_batch(parsed, false)
        .await
        .expect("commit");
    r.indexer
        .embed_deferred_chunks_gated(None, None)
        .await
        .expect("catch-up");
    let chunks = r.indexer.raw_chunks_snapshot().await.expect("chunks");
    assert_eq!(chunks.len(), 1, "the catch-up added no window rows");
    assert_eq!(chunks[0].id, id);
    assert!(
        r.store.contains_many(&[id]).await[0],
        "the chunk got its vector"
    );
    assert_eq!(r.indexer.embed_window_stats().chunks_split(), 0);
    let seen = r.seen.lock().expect("seen").clone();
    assert!(
        seen.iter().all(|t| !t.starts_with("// path:")),
        "the deferred pass embeds stored text with no header"
    );
}

/// A chunking mock that reports its window only after its first answer, like
/// the lazy sidecar.
struct LazyWindow {
    inner: MockEmbedder,
    answered: std::sync::atomic::AtomicBool,
}

#[async_trait::async_trait]
impl Embedder for LazyWindow {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Embedder::embed(&self.inner, text).await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        self.answered
            .store(true, std::sync::atomic::Ordering::SeqCst);
        Embedder::embed_batch(&self.inner, texts).await
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn input_window(&self) -> Option<usize> {
        let known = self.answered.load(std::sync::atomic::Ordering::SeqCst);
        known.then(|| Embedder::input_window(&self.inner)).flatten()
    }
    fn chunking_tokenizer(
        &self,
    ) -> Result<
        Option<Arc<trusty_common::embedder::ChunkTokenizer>>,
        trusty_common::embedder::ChunkError,
    > {
        Embedder::chunking_tokenizer(&self.inner)
    }
}

/// A window the embedder reports only after its first answer is not guessed:
/// a write before that answer goes unsplit, and the next one splits.
#[tokio::test]
async fn an_unknown_window_does_not_split_until_the_embedder_reports_it() {
    let lazy = Arc::new(LazyWindow {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        answered: std::sync::atomic::AtomicBool::new(false),
    });
    let embedder: Arc<dyn Embedder> = lazy.clone();
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch new"));
    let indexer = CodeIndexer::new("w10481-lazy", "/tmp/test").with_components(embedder, store);
    let (out, headers) = indexer
        .split_for_embed(vec![long_chunk("p", &text(10))])
        .await
        .expect("split");
    assert!(
        headers.is_none() && out.len() == 1,
        "no window yet, so no split"
    );
    assert!(
        !lazy.answered.load(std::sync::atomic::Ordering::SeqCst),
        "the split sent the embedder no probe"
    );
    indexer.warm_embedder().await;
    let (out, headers) = indexer
        .split_for_embed(vec![long_chunk("p", &text(10))])
        .await
        .expect("split");
    assert!(
        headers.is_some() && out.len() > 1,
        "the window is known now"
    );
}

/// An absolute path is made root-relative in the header, so a home directory
/// never reaches the embedded text and one chunk has one header.
#[test]
fn an_absolute_path_is_made_root_relative_in_the_header() {
    assert_eq!(header_path("/tmp/test/src/a.rs", &roots()), "src/a.rs");
    assert_eq!(header_path("src/a.rs", &roots()), "src/a.rs");
    let extra = IndexRoots::new(
        std::path::PathBuf::from("/tmp/test"),
        vec![std::path::PathBuf::from("/other/extra")],
    );
    assert_eq!(header_path("/other/extra/x.rs", &extra), "@root1/x.rs");
}

/// Under no root only the file name is kept.
#[test]
fn an_absolute_path_under_no_root_keeps_only_its_file_name() {
    assert_eq!(
        header_path("/Users/someone/private/dir/b.rs", &roots()),
        "b.rs"
    );
}

/// `index_file` with an absolute path embeds the same header as the relative one.
#[tokio::test]
async fn index_file_embeds_the_same_header_for_an_absolute_path() {
    let r = rig(Some(WINDOW));
    r.indexer
        .index_file("/tmp/test/src/a.rs", &long_fn("zebracrossing"))
        .await
        .expect("index");
    let seen = r.seen.lock().expect("seen").clone();
    let embedded: Vec<&String> = seen.iter().filter(|t| t.as_str() != "warm").collect();
    assert!(!embedded.is_empty());
    for t in embedded {
        assert!(
            t.starts_with("// path: src/a.rs :: Function long_one\n"),
            "{t}"
        );
        assert!(!t.contains("/tmp/test"), "an absolute root leaked: {t}");
    }
}

/// A header that leaves under half the window is dropped for that chunk, and the
/// content is cut at the full window instead.
#[test]
fn a_header_that_leaves_too_little_room_is_dropped() {
    let tok = tokenizer();
    let mut parent = long_chunk("p", &text(10));
    parent.function_name = Some("a b c d e f g h i j".into());
    parent.chunk_type = ChunkType::Function;
    // Header: 5 words + 10 name words = 15 tokens, leaving 5 of 20.
    let (out, headers, _) = split_at_window(&tok, WINDOW, &roots(), vec![parent]);
    assert!(out.len() > 1);
    assert!(headers.iter().all(String::is_empty), "{headers:?}");
    assert_eq!(headers.len(), out.len());
    for w in &out {
        let n = tok.count_tokens(&w.content).expect("count") + OVERHEAD;
        assert!(n <= WINDOW, "window of {n} tokens exceeds {WINDOW}");
    }
    assert!(
        out.iter()
            .any(|w| tok.count_tokens(&w.content).expect("count") > WINDOW - 8),
        "windows use the full window, not the room the header left"
    );
}

/// Re-indexing a file that shrank from several windows to one chunk leaves no
/// trace of the old windows in the vector store, BM25 or the corpus.
#[tokio::test]
async fn a_shrinking_file_leaves_no_old_window_behind() {
    let r = rig(Some(WINDOW));
    r.indexer
        .index_file("src/a.rs", &long_fn("oldtailmarker"))
        .await
        .expect("long version");
    let old_ids = r.indexer.chunk_ids_for_file("src/a.rs").await;
    assert!(
        old_ids.len() >= 3,
        "the long version has 3+ windows: {}",
        old_ids.len()
    );
    assert!(
        r.store.contains_many(&old_ids).await.iter().all(|&h| h),
        "the old windows have vectors"
    );
    assert!(!r
        .indexer
        .bm25_search("oldtailmarker", 5, None)
        .await
        .expect("bm25")
        .is_empty());

    r.indexer
        .index_file("src/a.rs", "fn long_one() {}\n")
        .await
        .expect("short version");
    assert!(
        r.store.contains_many(&old_ids).await.iter().all(|&h| !h),
        "an old window id still has a vector"
    );
    let hits = r
        .indexer
        .bm25_search("oldtailmarker", 5, None)
        .await
        .expect("bm25");
    assert!(
        hits.is_empty(),
        "BM25 still finds the old windows: {hits:?}"
    );
    let chunks = r.indexer.raw_chunks_snapshot().await.expect("chunks");
    assert_eq!(chunks.len(), 1, "only the new chunk remains");
    assert!(chunks[0].content.contains("fn long_one() {}"));
}
