//! #9284: chunks the embedder truncates at its input window are counted and
//! warned on, and nothing else about the embed pass changes.
//!
//! The chunking `MockEmbedder` counts one token per whitespace word plus
//! `[CLS]`/`[SEP]`, so a chunk of `n` words is `n + 2` tokens.

use super::*;
use trusty_common::embedder::{ChunkError, ChunkTokenizer};

const DIM: usize = 32;
const WINDOW: usize = 8;

fn words(n: usize) -> String {
    (0..n)
        .map(|i| format!("w{i}"))
        .collect::<Vec<_>>()
        .join(" ")
}

fn indexer_with(embedder: Arc<dyn Embedder>) -> CodeIndexer {
    let store: Arc<dyn VectorStore> = Arc::new(UsearchStore::new(DIM).expect("usearch new"));
    CodeIndexer::new("w9284", "/tmp/test").with_components(embedder, store)
}

fn chunking_indexer() -> CodeIndexer {
    indexer_with(Arc::new(MockEmbedder::new(DIM).with_chunking(WINDOW)))
}

async fn embed(indexer: &CodeIndexer, chunks: &[RawChunk]) {
    let out = indexer
        .embed_chunks_in_batches(chunks, None, None, false)
        .await
        .expect("embed pass");
    assert!(out.iter().all(Option::is_some), "every chunk must embed");
}

/// Collects WARN-and-above log lines written on this thread.
#[derive(Clone, Default)]
struct Capture(Arc<std::sync::Mutex<Vec<u8>>>);

impl std::io::Write for Capture {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0.lock().expect("capture lock").extend_from_slice(buf);
        Ok(buf.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

impl Capture {
    fn lines(&self) -> Vec<String> {
        let bytes = self.0.lock().expect("capture lock").clone();
        String::from_utf8_lossy(&bytes)
            .lines()
            .map(str::to_owned)
            .collect()
    }
}

fn capture() -> (Capture, tracing::subscriber::DefaultGuard) {
    let cap = Capture::default();
    let writer = cap.clone();
    let subscriber = tracing_subscriber::fmt()
        .with_writer(move || writer.clone())
        .with_max_level(tracing::Level::WARN)
        .with_ansi(false)
        .without_time()
        .finish();
    (cap, tracing::subscriber::set_default(subscriber))
}

/// One token past the window is one truncated chunk.
#[tokio::test]
async fn a_chunk_over_the_window_counts_exactly_once() {
    let indexer = chunking_indexer();
    // 7 words + 2 specials = 9 tokens, one past the window of 8.
    embed(&indexer, &[raw("a", "src/a.rs", &words(7))]).await;
    let stats = indexer.embed_window_stats();
    assert_eq!(stats.truncated(), 1, "one over-window chunk counts once");
    assert_eq!(stats.unmeasured(), 0);
    assert_eq!(stats.window(), Some(WINDOW));
}

/// A chunk that fills the window exactly is not truncated.
#[tokio::test]
async fn a_chunk_within_the_window_does_not_count() {
    let indexer = chunking_indexer();
    let chunks = [
        raw("a", "src/a.rs", &words(3)),
        // 6 words + 2 specials = 8 tokens: exactly the window.
        raw("b", "src/b.rs", &words(6)),
    ];
    embed(&indexer, &chunks).await;
    let stats = indexer.embed_window_stats();
    assert_eq!(stats.truncated(), 0, "within-window chunks never count");
    assert_eq!(stats.unmeasured(), 0);
    assert_eq!(stats.window(), Some(WINDOW), "the window was measured");
}

/// One warning per file, naming the index, the file and tokens vs window.
#[tokio::test]
async fn truncation_warns_once_per_file_naming_index_file_and_tokens() {
    let (cap, _guard) = capture();
    let indexer = chunking_indexer();
    let chunks = [
        raw("a1", "src/a.rs", &words(20)),
        raw("a2", "src/a.rs", &words(12)),
        raw("b1", "src/b.rs", &words(3)),
    ];
    embed(&indexer, &chunks).await;
    assert_eq!(indexer.embed_window_stats().truncated(), 2);

    let lines = cap.lines();
    let a_lines: Vec<&String> = lines.iter().filter(|l| l.contains("src/a.rs")).collect();
    assert_eq!(a_lines.len(), 1, "one line per file: {lines:#?}");
    let line = a_lines[0];
    for needle in ["index_id=w9284", "chunks=2", "tokens=22", "window=8"] {
        assert!(line.contains(needle), "{needle} missing from {line}");
    }
    assert!(
        !lines.iter().any(|l| l.contains("src/b.rs")),
        "a within-window file is not warned on: {lines:#?}"
    );
}

/// A pass names at most eight files, then one summary carries the rest.
#[tokio::test]
async fn a_large_pass_names_eight_files_and_summarises_the_rest() {
    let (cap, _guard) = capture();
    let indexer = chunking_indexer();
    let chunks: Vec<RawChunk> = (0..10)
        .map(|i| raw(&format!("c{i}"), &format!("src/f{i}.rs"), &words(10)))
        .collect();
    embed(&indexer, &chunks).await;
    assert_eq!(indexer.embed_window_stats().truncated(), 10);

    let lines = cap.lines();
    let per_file = lines.iter().filter(|l| l.contains(" file=")).count();
    assert_eq!(per_file, 8, "per-file lines are capped: {lines:#?}");
    let summary: Vec<&String> = lines
        .iter()
        .filter(|l| l.contains("truncated=10"))
        .collect();
    assert_eq!(summary.len(), 1, "one pass summary: {lines:#?}");
    for needle in ["index_id=w9284", "files=10", "files_unnamed=2", "window=8"] {
        assert!(
            summary[0].contains(needle),
            "{needle} missing: {}",
            summary[0]
        );
    }
}

/// Fail-open check: a window with no tokenizer is unmeasured, not "within".
#[tokio::test]
async fn a_window_without_a_tokenizer_counts_every_chunk_as_unmeasured() {
    let indexer = indexer_with(Arc::new(MockEmbedder::new(DIM).with_input_window(WINDOW)));
    let chunks = [
        raw("a", "src/a.rs", &words(3)),
        raw("b", "src/b.rs", &words(30)),
    ];
    embed(&indexer, &chunks).await;
    let stats = indexer.embed_window_stats();
    assert_eq!(stats.unmeasured(), 2);
    assert_eq!(stats.truncated(), 0);
    assert_eq!(stats.window(), Some(WINDOW));
}

/// Reports a window and fails to load its tokenizer.
struct BrokenTokenizerEmbedder;

#[async_trait::async_trait]
impl Embedder for BrokenTokenizerEmbedder {
    async fn embed(&self, _text: &str) -> anyhow::Result<Vec<f32>> {
        Ok(vec![1.0; DIM])
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![1.0; DIM]).collect())
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn input_window(&self) -> Option<usize> {
        Some(WINDOW)
    }
    fn chunking_tokenizer(&self) -> Result<Option<Arc<ChunkTokenizer>>, ChunkError> {
        Err(ChunkError::Encode("boom".to_string()))
    }
}

/// Fail-open check: a tokenizer error is counted and warned, never "within".
#[tokio::test]
async fn a_tokenizer_error_counts_as_unmeasured_not_within() {
    let (cap, _guard) = capture();
    let indexer = indexer_with(Arc::new(BrokenTokenizerEmbedder));
    embed(&indexer, &[raw("a", "src/a.rs", &words(3))]).await;
    let stats = indexer.embed_window_stats();
    assert_eq!(stats.unmeasured(), 1);
    assert_eq!(stats.truncated(), 0);
    let lines = cap.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains("unmeasured=1") && l.contains("boom")),
        "the unmeasured chunks are warned with the cause: {lines:#?}"
    );
}

/// An embedder that reports no window (a sidecar today) counts nothing.
#[tokio::test]
async fn an_embedder_with_no_window_counts_nothing() {
    let indexer = indexer_with(Arc::new(MockEmbedder::new(DIM)));
    embed(&indexer, &[raw("a", "src/a.rs", &words(30))]).await;
    let stats = indexer.embed_window_stats();
    assert_eq!(stats.truncated(), 0);
    assert_eq!(stats.unmeasured(), 0);
    assert_eq!(stats.window(), None);
}

/// Counting changes no vector: a truncated chunk embeds as it did before.
#[tokio::test]
async fn counting_leaves_the_vectors_unchanged() {
    let mock = MockEmbedder::new(DIM).with_chunking(WINDOW);
    let text = words(20);
    let expected = Embedder::embed(&mock, &text).await.expect("direct embed");
    let indexer = indexer_with(Arc::new(mock));
    let out = indexer
        .embed_chunks_in_batches(&[raw("a", "src/a.rs", &text)], None, None, false)
        .await
        .expect("embed pass");
    assert_eq!(out[0].as_deref(), Some(expected.as_slice()));
}

/// Fail-open check: a chunk the wave produced no measure for is unmeasured.
#[test]
fn a_chunk_without_a_measure_counts_as_unmeasured() {
    use super::super::ingest::truncation::{Measure, PassTally};
    let stats = EmbedWindowStats::default();
    let mut tally = PassTally::new(Some(WINDOW));
    let chunks = [raw("a", "src/a.rs", "x"), raw("b", "src/b.rs", "y")];
    tally.record("w9284", &stats, &chunks, &[Measure::Within]);
    assert_eq!(stats.unmeasured(), 1, "the unpaired chunk is not within");
    assert_eq!(stats.truncated(), 0);
}

/// Cumulative, not distinct: re-embedding a cut chunk counts it again.
#[tokio::test]
async fn a_second_embed_of_the_same_chunk_counts_again() {
    let indexer = chunking_indexer();
    let chunks = [raw("a", "src/a.rs", &words(20))];
    embed(&indexer, &chunks).await;
    embed(&indexer, &chunks).await;
    assert_eq!(indexer.embed_window_stats().truncated(), 2);
}

/// A pass of `n` chunks where each is `marker` plus filler, for the seam.
fn forced(marker: &str, n: usize) -> Vec<RawChunk> {
    (0..n)
        .map(|i| {
            raw(
                &format!("f{i}"),
                &format!("src/f{i}.rs"),
                &format!("{marker} {}", words(2)),
            )
        })
        .collect()
}

/// Shared checks for both forced-failure arms.
fn assert_all_unmeasured(indexer: &CodeIndexer, cap: &Capture, n: u64, cause: &str) {
    let stats = indexer.embed_window_stats();
    assert_eq!(
        stats.unmeasured(),
        n,
        "every chunk of the pass is unmeasured"
    );
    assert_eq!(stats.truncated(), 0, "an uncounted chunk is not truncated");
    let lines = cap.lines();
    assert!(
        lines
            .iter()
            .any(|l| l.contains(&format!("unmeasured={n}")) && l.contains(cause)),
        "the unmeasured WARN names the cause {cause:?}: {lines:#?}"
    );
}

/// Error arm of `measure`: a failed token count is unmeasured, not within.
#[tokio::test]
async fn a_forced_count_failure_counts_every_chunk_as_unmeasured() {
    use super::super::ingest::truncation::FORCE_COUNT_ERROR;
    let (cap, _guard) = capture();
    let indexer = chunking_indexer();
    // `embed` also asserts every chunk got a vector: indexing completes.
    embed(&indexer, &forced(FORCE_COUNT_ERROR, 3)).await;
    assert_all_unmeasured(&indexer, &cap, 3, "forced count failure");
}

/// Error arm of `PendingMeasure::finish`: a panicking count task is
/// unmeasured, and the panic does not reach the embed pass.
#[tokio::test]
async fn a_panicking_count_task_counts_every_chunk_as_unmeasured() {
    use super::super::ingest::truncation::FORCE_COUNT_PANIC;
    let (cap, _guard) = capture();
    let indexer = chunking_indexer();
    embed(&indexer, &forced(FORCE_COUNT_PANIC, 3)).await;
    assert_all_unmeasured(&indexer, &cap, 3, "the token-count task failed");
}

/// Reports its window only once it has embedded, as a lazy sidecar does.
struct LateWindowEmbedder {
    inner: MockEmbedder,
    answered: std::sync::atomic::AtomicBool,
}

impl LateWindowEmbedder {
    fn answered(&self) -> bool {
        self.answered.load(std::sync::atomic::Ordering::Acquire)
    }
}

#[async_trait::async_trait]
impl Embedder for LateWindowEmbedder {
    async fn embed(&self, text: &str) -> anyhow::Result<Vec<f32>> {
        Embedder::embed(&self.inner, text).await
    }
    async fn embed_batch(&self, texts: &[&str]) -> anyhow::Result<Vec<Vec<f32>>> {
        let out = Embedder::embed_batch(&self.inner, texts).await;
        self.answered
            .store(true, std::sync::atomic::Ordering::Release);
        out
    }
    fn dimension(&self) -> usize {
        DIM
    }
    fn input_window(&self) -> Option<usize> {
        self.answered()
            .then(|| Embedder::input_window(&self.inner))
            .flatten()
    }
    fn chunking_tokenizer(&self) -> Result<Option<Arc<ChunkTokenizer>>, ChunkError> {
        if self.answered() {
            Embedder::chunking_tokenizer(&self.inner)
        } else {
            Ok(None)
        }
    }
}

/// #9284: a sidecar that reports its window only after its first answer still
/// yields a real window and a real count on the very first pass.
#[tokio::test]
async fn a_window_learned_after_the_first_wave_is_reported_and_counted() {
    let indexer = indexer_with(Arc::new(LateWindowEmbedder {
        inner: MockEmbedder::new(DIM).with_chunking(WINDOW),
        answered: std::sync::atomic::AtomicBool::new(false),
    }));
    // 7 words + 2 specials = 9 tokens, one past the window of 8.
    embed(&indexer, &[raw("a", "src/a.rs", &words(7))]).await;
    let stats = indexer.embed_window_stats();
    assert_eq!(stats.window(), Some(WINDOW), "the window is no longer null");
    assert_eq!(stats.truncated(), 1, "the first wave is counted too");
    assert_eq!(stats.unmeasured(), 0);
}
