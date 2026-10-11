//! Token-exact text chunking for embedders with a bounded input window.
//!
//! Why: #9275 — an embedder silently drops input past its window, so the tail
//! of a long text never reaches its vector. The split has to be measured with
//! the tokenizer the embedder truncates with: a byte or word estimate can
//! under-count and bring the truncation back.
//! What: [`ChunkTokenizer`] (a `tokenizers` tokenizer with truncation and
//! padding off), [`chunk_text`] (window-bounded, overlapping, gap-free byte
//! ranges), [`CHUNK_OVERLAP_PERCENT`] and [`ChunkError`].
//! Test: `chunk_tests.rs`.

use std::ops::Range;
use std::path::{Path, PathBuf};

use fastembed_tokenizers::{AddedToken, Encoding, Tokenizer};

use super::fast_embedder::FASTEMBED_MAX_LENGTH;
use super::types::resolve_fastembed_cache_dir;

/// Share of a chunk's token budget that the next chunk repeats, in percent.
///
/// Why: a fact cut by a chunk boundary is in neither vector; a short repeated
/// span keeps it whole in one of them. Fixed by Architect ruling on #9275: at
/// most 10% of the window, with no knob.
/// What: overlap tokens = `(window - special tokens) * 10 / 100`, rounded down.
/// Test: `chunk_overlap_is_at_most_ten_percent_of_window`,
/// `consecutive_chunks_overlap_by_the_fixed_constant`.
pub const CHUNK_OVERLAP_PERCENT: usize = 10;

/// Failure to load a tokenizer or to split text within a window.
///
/// Why: #9275 — a chunker that cannot count exactly must stop, never fall
/// back to a guessed split that could exceed the window.
/// What: one variant per failure mode.
/// Test: `fastembed_cache_without_the_model_is_an_error`,
/// `window_not_above_special_tokens_is_an_error`.
#[derive(Debug, thiserror::Error)]
pub enum ChunkError {
    /// A tokenizer file is not in the model cache.
    #[error("tokenizer file {} is missing; load the model through fastembed first", path.display())]
    TokenizerMissing { path: PathBuf },
    /// A tokenizer file exists but does not parse.
    #[error("tokenizer file {} could not be loaded: {reason}", path.display())]
    TokenizerLoad { path: PathBuf, reason: String },
    /// The tokenizer refused to encode the text.
    #[error("tokenizer failed to encode text: {0}")]
    Encode(String),
    /// The window has no room for content after the special tokens.
    #[error("window {window} leaves no room past {overhead} special tokens")]
    WindowTooSmall { window: usize, overhead: usize },
    /// A single token span re-encodes past the budget on its own.
    #[error("text at bytes {start}..{end} cannot be split to fit {budget} tokens")]
    Unsplittable {
        start: usize,
        end: usize,
        budget: usize,
    },
}

/// One window-bounded slice of the input, borrowed from it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TextChunk<'a> {
    /// Byte range of `text` within the input.
    pub byte_range: Range<usize>,
    /// The slice itself, `&input[byte_range]`.
    pub text: &'a str,
}

/// Exact token counter for [`chunk_text`].
///
/// Why: #9275 — fastembed keeps its tokenizer private, so the chunker loads
/// its own copy of the same files with the same `tokenizers` release.
/// What: wraps a `tokenizers::Tokenizer` with truncation and padding switched
/// off, so a count is never capped. `special_overhead` is the number of
/// special tokens a single sequence gains (`[CLS]` + `[SEP]` = 2 for BERT).
/// Test: `count_ignores_the_truncation_in_tokenizer_json`,
/// `real_tokenizer_chunks_long_text_within_fastembed_window`.
#[derive(Debug, Clone)]
pub struct ChunkTokenizer {
    tokenizer: Tokenizer,
    special_overhead: usize,
    max_length: Option<usize>,
}

impl ChunkTokenizer {
    /// Build from the bytes of a `tokenizer.json`.
    pub fn from_bytes(tokenizer_json: &[u8]) -> Result<Self, ChunkError> {
        let tokenizer =
            Tokenizer::from_bytes(tokenizer_json).map_err(|e| ChunkError::TokenizerLoad {
                path: PathBuf::from("<bytes>"),
                reason: e.to_string(),
            })?;
        Self::from_tokenizer(tokenizer, None)
    }

    /// Load the tokenizer fastembed cached for `model_code`
    /// (e.g. `Qdrant/all-MiniLM-L6-v2-onnx`).
    ///
    /// Why: the chunker must count with the tokenizer fastembed truncates with.
    /// What: resolves the cache directory the way fastembed does (`HF_HOME`,
    /// else [`resolve_fastembed_cache_dir`]) and calls
    /// [`Self::for_fastembed_model_in`]. Never downloads.
    /// Test: `real_tokenizer_chunks_long_text_within_fastembed_window`.
    pub fn for_fastembed_model(model_code: &str) -> Result<Self, ChunkError> {
        // fastembed's `pull_from_hf` lets `HF_HOME` override its cache dir.
        let cache_dir = std::env::var("HF_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| resolve_fastembed_cache_dir());
        Self::for_fastembed_model_in(&cache_dir, model_code)
    }

    /// Load `model_code`'s tokenizer from the HuggingFace cache at `cache_dir`.
    ///
    /// Why: fastembed builds its tokenizer from three cached files; any
    /// difference changes the count.
    /// What: reads `refs/main`, then the snapshot's `tokenizer.json`,
    /// `tokenizer_config.json` and `special_tokens_map.json`. Mirrors
    /// fastembed's `load_tokenizer`: registers the special-token map and
    /// records `max_length = min(512, model_max_length)`. A missing file is
    /// [`ChunkError::TokenizerMissing`].
    /// Test: `fastembed_cache_without_the_model_is_an_error`,
    /// `fastembed_window_matches_cached_tokenizer_max_length`.
    pub fn for_fastembed_model_in(cache_dir: &Path, model_code: &str) -> Result<Self, ChunkError> {
        // #9275 red stub.
        let _ = (cache_dir, model_code, FASTEMBED_MAX_LENGTH);
        Err(ChunkError::Encode("stub".to_string()))
    }

    fn from_tokenizer(tokenizer: Tokenizer, max_length: Option<usize>) -> Result<Self, ChunkError> {
        // The MiniLM tokenizer.json truncates at 128; a capped count would
        // hide an over-window chunk.
        // #9275 red stub: truncation and padding left on.
        let special_overhead = tokenizer
            .encode("", true)
            .map_err(|e| ChunkError::Encode(e.to_string()))?
            .len();
        Ok(Self {
            tokenizer,
            special_overhead,
            max_length,
        })
    }

    /// Number of tokens in `text`, special tokens excluded, never truncated.
    pub fn count_tokens(&self, text: &str) -> Result<usize, ChunkError> {
        Ok(self.encode(text)?.len())
    }

    /// Special tokens a single sequence gains when encoded for the model.
    pub fn special_overhead(&self) -> usize {
        self.special_overhead
    }

    /// fastembed's truncation length for this tokenizer, when loaded from
    /// the fastembed cache; `None` for [`Self::from_bytes`].
    pub fn max_length(&self) -> Option<usize> {
        self.max_length
    }

    fn encode(&self, text: &str) -> Result<Encoding, ChunkError> {
        self.tokenizer
            .encode(text, false)
            .map_err(|e| ChunkError::Encode(e.to_string()))
    }
}

/// Split `text` into chunks that each fit `window` tokens, special tokens
/// included.
///
/// Why: #9275 — an embedder with a bounded window truncates long text; each
/// chunk is embedded on its own so no part of the text is dropped.
/// What: text whose count fits returns one chunk equal to the input (empty
/// input included). Longer text is cut at token boundaries, preferring word
/// starts; every chunk is re-counted with `tokenizer` and shrunk until
/// `count + special_overhead <= window`. Consecutive chunks share
/// [`CHUNK_OVERLAP_PERCENT`] of the budget; ranges start at 0, end at
/// `text.len()` and leave no gap. Errors when the window has no room past
/// the special tokens or a single token span cannot fit.
/// Test: `text_within_window_is_one_chunk_equal_to_input`,
/// `long_multi_sentence_text_is_bounded_and_covered`,
/// `unbroken_run_without_whitespace_is_bounded_and_covered`,
/// `consecutive_chunks_overlap_by_the_fixed_constant`,
/// `real_tokenizer_chunks_long_text_within_fastembed_window`.
pub fn chunk_text<'a>(
    tokenizer: &ChunkTokenizer,
    text: &'a str,
    window: usize,
) -> Result<Vec<TextChunk<'a>>, ChunkError> {
    // #9275 red stub.
    let _ = (tokenizer, text, window);
    Ok(Vec::new())
}

/// Overlap, in tokens, for a chunk budget of `budget` tokens.
pub(super) fn overlap_tokens(budget: usize) -> usize {
    // #9275 red stub.
    let _ = budget;
    0
}

/// Largest token index in `(lo, hi]` that starts a word, else `hi`.
fn word_start_at_or_before(word_ids: &[Option<u32>], lo: usize, hi: usize) -> usize {
    (lo + 1..=hi)
        .rev()
        .find(|&t| match (word_ids.get(t - 1), word_ids.get(t)) {
            (Some(Some(prev)), Some(Some(cur))) => prev != cur,
            _ => true,
        })
        .unwrap_or(hi)
}

fn read_file(path: &Path) -> Result<String, ChunkError> {
    std::fs::read_to_string(path).map_err(|e| match e.kind() {
        std::io::ErrorKind::NotFound => ChunkError::TokenizerMissing {
            path: path.to_path_buf(),
        },
        _ => ChunkError::TokenizerLoad {
            path: path.to_path_buf(),
            reason: e.to_string(),
        },
    })
}

fn read_json(path: &Path) -> Result<serde_json::Value, ChunkError> {
    serde_json::from_str(&read_file(path)?).map_err(|e| ChunkError::TokenizerLoad {
        path: path.to_path_buf(),
        reason: e.to_string(),
    })
}

/// One `special_tokens_map.json` entry as fastembed's `load_tokenizer` reads it.
fn special_token(value: &serde_json::Value) -> Option<AddedToken> {
    if let Some(content) = value.as_str() {
        return Some(AddedToken {
            content: content.to_string(),
            special: true,
            ..Default::default()
        });
    }
    Some(AddedToken {
        content: value["content"].as_str()?.to_string(),
        single_word: value["single_word"].as_bool()?,
        lstrip: value["lstrip"].as_bool()?,
        rstrip: value["rstrip"].as_bool()?,
        normalized: value["normalized"].as_bool()?,
        special: true,
    })
}
