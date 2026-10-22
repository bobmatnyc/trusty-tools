//! Count and warn on chunks the embedder truncates at its input window (#9284).
//!
//! Why: an embedder encodes at most its window of tokens and drops the rest
//! with no error, so the vector of a long chunk silently describes only its
//! head. Nothing counted it.
//! What: [`EmbedWindowStats`], the per-index counters `GET /indexes/:id/status`
//! reports; [`WindowProbe`], which counts each chunk's tokens with the
//! tokenizer the embedder truncates with; and [`PassTally`], which bounds the
//! warnings one embed pass logs. In memory only: nothing here is persisted,
//! and no chunk or vector changes.
//! Test: `core::indexer::tests::embed_window_9284`.

use std::collections::{BTreeMap, HashSet};
use std::sync::atomic::{AtomicU64, AtomicUsize, Ordering};
use std::sync::Arc;

use trusty_common::embedder::ChunkTokenizer;

use crate::core::chunker::RawChunk;
use crate::core::embed::Embedder;

/// Files one embed pass names in a per-file warning before it only counts.
///
/// Why: a large reindex can truncate chunks in thousands of files; one line
/// each would flood the log. The pass summary carries the rest.
/// Test: `a_large_pass_names_eight_files_and_summarises_the_rest`.
pub(crate) const MAX_FILE_WARNINGS_PER_PASS: usize = 8;

/// Per-index embed-window counters since this indexer was built (#9284).
///
/// Why: an operator needs to see how often the vector lane embedded cut
/// text, without reading daemon logs. The counters are cumulative embed
/// events, not a count of distinct cut chunks: a forced reindex, or a retry
/// after a stall, embeds a chunk again and counts it again.
/// What: `truncated` counts embed events whose chunk token count, special
/// tokens included, exceeded the window. `unmeasured` counts embed events the
/// window applied to but whose length could not be counted. `window` is the
/// window of the latest embed pass, `0` when that embedder reports none, and
/// stays `0` until a pass runs since the indexer was built.
/// Test: `a_chunk_over_the_window_counts_exactly_once`,
/// `a_chunk_within_the_window_does_not_count`,
/// `a_second_embed_of_the_same_chunk_counts_again`.
#[derive(Debug, Default)]
pub struct EmbedWindowStats {
    truncated: AtomicU64,
    unmeasured: AtomicU64,
    window: AtomicUsize,
}

impl EmbedWindowStats {
    /// Embedded chunks cut at the window.
    pub fn truncated(&self) -> u64 {
        self.truncated.load(Ordering::Relaxed)
    }

    /// Embedded chunks whose length could not be counted against the window.
    pub fn unmeasured(&self) -> u64 {
        self.unmeasured.load(Ordering::Relaxed)
    }

    /// The latest pass's window; `None` before any pass has run, or when the
    /// embedder reports none, so the two counters cannot see truncation (#9284).
    pub fn window(&self) -> Option<usize> {
        match self.window.load(Ordering::Relaxed) {
            0 => None,
            n => Some(n),
        }
    }

    /// Record the window an embed pass measures against (`None` = unknown).
    pub(crate) fn set_window(&self, window: Option<usize>) {
        self.window.store(window.unwrap_or(0), Ordering::Relaxed);
    }

    fn add(&self, truncated: u64, unmeasured: u64) {
        self.truncated.fetch_add(truncated, Ordering::Relaxed);
        self.unmeasured.fetch_add(unmeasured, Ordering::Relaxed);
    }
}

/// One embedded chunk's length against the window.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum Measure {
    Within,
    /// `tokens` includes the special tokens, the unit the window counts in.
    Truncated {
        tokens: usize,
    },
    /// The window applies but the length could not be counted.
    Unmeasured {
        reason: String,
    },
}

/// How an embed pass measures chunks against the embedder's window.
///
/// Why: detection must count tokens with the tokenizer the embedder
/// truncates with, never estimate from characters.
/// What: built once per pass from [`Embedder::input_window`] and
/// [`Embedder::chunking_tokenizer`]. `Unknown`: no window is reported, so
/// nothing is counted. `Blind`: a window but no usable tokenizer, so every
/// chunk is unmeasured. `Counting`: each chunk is counted exactly.
/// Test: `a_chunk_over_the_window_counts_exactly_once`,
/// `a_window_without_a_tokenizer_counts_every_chunk_as_unmeasured`,
/// `a_tokenizer_error_counts_as_unmeasured_not_within`,
/// `an_embedder_with_no_window_counts_nothing`.
#[derive(Clone)]
pub(crate) enum WindowProbe {
    // See #9284: a sidecar embedder carries no window on the wire (plan Q2),
    // so its chunks are not counted and no window is invented for it.
    Unknown,
    Blind {
        window: usize,
        reason: String,
    },
    Counting {
        window: usize,
        tokenizer: Arc<ChunkTokenizer>,
    },
}

impl WindowProbe {
    /// Read the window and tokenizer from `embedder`.
    pub(crate) fn from_embedder(embedder: &dyn Embedder) -> Self {
        let Some(window) = embedder.input_window() else {
            return Self::Unknown;
        };
        // #9284: a tokenizer that is missing or fails to load leaves every
        // chunk unmeasured; it never reads as "within the window".
        match embedder.chunking_tokenizer() {
            Ok(Some(tokenizer)) => Self::Counting { window, tokenizer },
            Ok(None) => Self::Blind {
                window,
                reason: "the embedder reports a window but no tokenizer".to_string(),
            },
            Err(e) => Self::Blind {
                window,
                reason: format!("the embedder's tokenizer did not load: {e}"),
            },
        }
    }

    /// The window this probe measures against, if the embedder reports one.
    pub(crate) fn window(&self) -> Option<usize> {
        match self {
            Self::Unknown => None,
            Self::Blind { window, .. } | Self::Counting { window, .. } => Some(*window),
        }
    }

    /// Start measuring `chunks`; tokenizing runs on a blocking thread so it
    /// overlaps the embed call and never stalls the runtime.
    pub(crate) fn start(&self, chunks: &[RawChunk]) -> PendingMeasure {
        match self {
            Self::Unknown => PendingMeasure::Skip,
            Self::Blind { reason, .. } => PendingMeasure::Ready(vec![
                Measure::Unmeasured {
                    reason: reason.clone(),
                };
                chunks.len()
            ]),
            Self::Counting { window, tokenizer } => {
                let (window, tokenizer) = (*window, Arc::clone(tokenizer));
                let texts: Vec<String> = chunks.iter().map(|c| c.content.clone()).collect();
                let n = texts.len();
                let task = tokio::task::spawn_blocking(move || {
                    texts
                        .iter()
                        .map(|t| measure(&tokenizer, window, t))
                        .collect()
                });
                PendingMeasure::Running { task, n }
            }
        }
    }
}

/// Chunk text that makes the test-build token count fail (#9284).
#[cfg(test)]
pub(crate) const FORCE_COUNT_ERROR: &str = "\u{1}force-count-error";
/// Chunk text that makes the test-build token count panic (#9284).
#[cfg(test)]
pub(crate) const FORCE_COUNT_PANIC: &str = "\u{1}force-count-panic";

/// Count `text`'s tokens; the one call the error arms below guard.
///
/// Why: `ChunkTokenizer` almost never fails to encode, so the failure arms of
/// [`measure`] and [`PendingMeasure::finish`] had no way to run in a test.
/// What: production builds call `count_tokens` directly. Test builds first
/// honour the marker texts above, so a test forces a failure or a panic in
/// the real count path. The markers do not exist outside `cfg(test)`.
/// Test: `a_forced_count_failure_counts_every_chunk_as_unmeasured`,
/// `a_panicking_count_task_counts_every_chunk_as_unmeasured`.
fn count(
    tokenizer: &ChunkTokenizer,
    text: &str,
) -> Result<usize, trusty_common::embedder::ChunkError> {
    #[cfg(test)]
    {
        if text.contains(FORCE_COUNT_ERROR) {
            return Err(trusty_common::embedder::ChunkError::Encode(
                "forced count failure".to_string(),
            ));
        }
        if text.contains(FORCE_COUNT_PANIC) {
            panic!("forced count panic");
        }
    }
    tokenizer.count_tokens(text)
}

/// Count `text` against `window`, special tokens included.
fn measure(tokenizer: &ChunkTokenizer, window: usize, text: &str) -> Measure {
    match count(tokenizer, text) {
        Ok(count) => {
            let tokens = count.saturating_add(tokenizer.special_overhead());
            if tokens > window {
                Measure::Truncated { tokens }
            } else {
                Measure::Within
            }
        }
        // #9284: a failed count is unmeasured, never "within".
        Err(e) => Measure::Unmeasured {
            reason: e.to_string(),
        },
    }
}

/// A wave's measurement, possibly still running.
pub(crate) enum PendingMeasure {
    Skip,
    Ready(Vec<Measure>),
    Running {
        task: tokio::task::JoinHandle<Vec<Measure>>,
        n: usize,
    },
}

impl PendingMeasure {
    /// The measurements, one per chunk; `None` when nothing is measured.
    pub(crate) async fn finish(self) -> Option<Vec<Measure>> {
        match self {
            Self::Skip => None,
            Self::Ready(measures) => Some(measures),
            Self::Running { task, n } => Some(match task.await {
                Ok(measures) => measures,
                // #9284: a lost count task leaves its chunks unmeasured.
                Err(e) => vec![
                    Measure::Unmeasured {
                        reason: format!("the token-count task failed: {e}"),
                    };
                    n
                ],
            }),
        }
    }
}

/// One embed pass's truncation tally and its bounded warnings (#9284).
///
/// Why: a per-chunk warning floods the log on a large reindex.
/// What: [`Self::record`] adds each embedded wave to the index counters and
/// logs one WARN per file with truncated chunks, for the first
/// [`MAX_FILE_WARNINGS_PER_PASS`] files. [`Self::finish`] logs one summary
/// WARN for the pass, and one for unmeasured chunks with the first cause.
/// Test: `truncation_warns_once_per_file_naming_index_file_and_tokens`,
/// `a_large_pass_names_eight_files_and_summarises_the_rest`.
pub(crate) struct PassTally {
    window: usize,
    truncated: u64,
    unmeasured: u64,
    max_tokens: usize,
    files: HashSet<String>,
    files_named: usize,
    first_reason: Option<String>,
}

impl PassTally {
    pub(crate) fn new(window: Option<usize>) -> Self {
        Self {
            window: window.unwrap_or(0),
            truncated: 0,
            unmeasured: 0,
            max_tokens: 0,
            files: HashSet::new(),
            files_named: 0,
            first_reason: None,
        }
    }

    /// Count the embedded `chunks`; `measures[i]` measures `chunks[i]`, and a
    /// chunk with no measure counts as unmeasured, never as within.
    pub(crate) fn record(
        &mut self,
        index_id: &str,
        stats: &EmbedWindowStats,
        chunks: &[RawChunk],
        measures: &[Measure],
    ) {
        let mut per_file: BTreeMap<&str, (u64, usize)> = BTreeMap::new();
        let (mut truncated, mut unmeasured) = (0_u64, 0_u64);
        let missing = Measure::Unmeasured {
            reason: "no token count was produced for this chunk".to_string(),
        };
        for (i, chunk) in chunks.iter().enumerate() {
            match measures.get(i).unwrap_or(&missing) {
                Measure::Within => {}
                Measure::Truncated { tokens } => {
                    truncated += 1;
                    let entry = per_file.entry(chunk.file.as_str()).or_insert((0, 0));
                    entry.0 += 1;
                    entry.1 = entry.1.max(*tokens);
                    self.max_tokens = self.max_tokens.max(*tokens);
                }
                Measure::Unmeasured { reason } => {
                    unmeasured += 1;
                    self.first_reason.get_or_insert_with(|| reason.clone());
                }
            }
        }
        stats.add(truncated, unmeasured);
        self.truncated += truncated;
        self.unmeasured += unmeasured;
        let window = self.window;
        for (file, (chunks, tokens)) in per_file {
            // Insert first, so `files` counts every file, named or not.
            if !self.files.insert(file.to_owned()) || self.files_named >= MAX_FILE_WARNINGS_PER_PASS
            {
                continue;
            }
            self.files_named += 1;
            tracing::warn!(
                index_id = %index_id,
                file = %file,
                chunks,
                tokens,
                window,
                "embedder input truncated: the vector misses the tail of these chunks (#9284)",
            );
        }
    }

    /// Log the pass summary; silent when nothing was cut or unmeasured.
    pub(crate) fn finish(&self, index_id: &str) {
        let window = self.window;
        if self.truncated > 0 {
            tracing::warn!(
                index_id = %index_id,
                truncated = self.truncated,
                files = self.files.len(),
                files_unnamed = self.files.len() - self.files_named,
                max_tokens = self.max_tokens,
                window,
                "embed pass sent chunks longer than the embedder window (#9284)",
            );
        }
        if self.unmeasured > 0 {
            tracing::warn!(
                index_id = %index_id,
                unmeasured = self.unmeasured,
                window,
                reason = %self.first_reason.as_deref().unwrap_or("unknown"),
                "embed pass could not count chunks against the embedder window (#9284)",
            );
        }
    }
}
