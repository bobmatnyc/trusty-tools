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
    /// The cached `refs/main` revision is not a plain directory name.
    #[error("model cache revision {revision:?} is not a plain name; refusing to read from it")]
    InvalidRevision { revision: String },
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
    /// records `max_length = min(FASTEMBED_MAX_LENGTH, model_max_length)`,
    /// the length fastembed truncates at. A missing file is
    /// [`ChunkError::TokenizerMissing`]. A `refs/main` revision that is not a
    /// plain name (see [`validate_revision`]) is [`ChunkError::InvalidRevision`]
    /// and no snapshot file is read (#10514).
    /// Test: `fastembed_cache_without_the_model_is_an_error`,
    /// `fastembed_truncates_at_minilm_window_on_every_model_revision`,
    /// `traversal_revision_is_rejected_without_reading`,
    /// `backslash_revision_is_rejected`, `sha_revision_still_loads`.
    pub fn for_fastembed_model_in(cache_dir: &Path, model_code: &str) -> Result<Self, ChunkError> {
        let repo = cache_dir.join(format!("models--{}", model_code.replace('/', "--")));
        let revision = read_file(&repo.join("refs").join("main"))?;
        let snapshot = repo.join("snapshots").join(validate_revision(&revision)?);

        let tokenizer_path = snapshot.join("tokenizer.json");
        let mut tokenizer = Tokenizer::from_bytes(read_file(&tokenizer_path)?).map_err(|e| {
            ChunkError::TokenizerLoad {
                path: tokenizer_path.clone(),
                reason: e.to_string(),
            }
        })?;

        let config_path = snapshot.join("tokenizer_config.json");
        let config = read_json(&config_path)?;
        let model_max =
            config["model_max_length"]
                .as_f64()
                .ok_or_else(|| ChunkError::TokenizerLoad {
                    path: config_path.clone(),
                    reason: "model_max_length is not a number".to_string(),
                })?;
        // `as` saturates: BGE-style 1e30 maxima become usize::MAX.
        let max_length = FASTEMBED_MAX_LENGTH.min(model_max as usize);

        let specials = read_json(&snapshot.join("special_tokens_map.json"))?;
        let added: Vec<AddedToken> = specials
            .as_object()
            .into_iter()
            .flat_map(|map| map.values())
            .filter_map(special_token)
            .collect();
        tokenizer.add_special_tokens(&added);

        Self::from_tokenizer(tokenizer, Some(max_length))
    }

    fn from_tokenizer(
        mut tokenizer: Tokenizer,
        max_length: Option<usize>,
    ) -> Result<Self, ChunkError> {
        // The MiniLM tokenizer.json truncates at 128; a capped count would
        // hide an over-window chunk.
        tokenizer
            .with_truncation(None)
            .map_err(|e| ChunkError::Encode(e.to_string()))?;
        tokenizer.with_padding(None);
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
/// `count + special_overhead <= window`. Each chunk ends past the previous
/// chunk's end. Consecutive chunks share at most [`CHUNK_OVERLAP_PERCENT`] of
/// the budget (none when the overlap alone re-counts past it); ranges start
/// at 0, end at `text.len()` and leave no gap. Errors when the window has no
/// room past the special tokens or a single token span cannot fit.
/// Test: `text_within_window_is_one_chunk_equal_to_input`,
/// `long_multi_sentence_text_is_bounded_and_covered`,
/// `unbroken_run_without_whitespace_is_bounded_and_covered`,
/// `chunk_ends_advance_past_a_word_longer_than_the_step`,
/// `consecutive_chunks_overlap_by_the_fixed_constant`,
/// `real_tokenizer_chunks_long_text_within_fastembed_window`.
pub fn chunk_text<'a>(
    tokenizer: &ChunkTokenizer,
    text: &'a str,
    window: usize,
) -> Result<Vec<TextChunk<'a>>, ChunkError> {
    let budget = window
        .checked_sub(tokenizer.special_overhead)
        .filter(|b| *b > 0)
        .ok_or(ChunkError::WindowTooSmall {
            window,
            overhead: tokenizer.special_overhead,
        })?;
    let encoding = tokenizer.encode(text)?;
    let total = encoding.len();
    if total <= budget {
        return Ok(vec![TextChunk {
            byte_range: 0..text.len(),
            text,
        }]);
    }
    let offsets = encoding.get_offsets();
    let word_ids = encoding.get_word_ids();
    let overlap = overlap_tokens(budget);

    let mut chunks = Vec::new();
    let (mut start_tok, mut start_byte, mut prev_end_tok) = (0, 0, 0);
    loop {
        // #9275: cut past the previous chunk's end, so every chunk ends later
        // than the one before and none sits inside it.
        let lo = start_tok.max(prev_end_tok);
        let mut end_tok = (start_tok + budget).min(total);
        if end_tok < total {
            end_tok = word_start_at_or_before(word_ids, lo, end_tok);
        }
        // Re-count the slice itself: a cut inside a word can re-tokenize longer.
        let fitted = loop {
            let end_byte = if end_tok == total {
                text.len()
            } else {
                offsets[end_tok].0
            };
            if let Some(s) = text.get(start_byte..end_byte)
                && end_byte > start_byte
                && tokenizer.count_tokens(s)? <= budget
            {
                break Ok((end_byte, s));
            }
            if end_tok <= lo + 1 {
                break Err(end_byte);
            }
            end_tok = word_start_at_or_before(word_ids, lo, end_tok - 1);
        };
        let (end_byte, slice) = match fitted {
            Ok(fit) => fit,
            // The overlap alone re-counts past the budget: drop it.
            Err(_) if start_tok < prev_end_tok => {
                start_tok = prev_end_tok;
                start_byte = offsets[start_tok].0;
                continue;
            }
            Err(end) => {
                return Err(ChunkError::Unsplittable {
                    start: start_byte,
                    end,
                    budget,
                });
            }
        };
        chunks.push(TextChunk {
            byte_range: start_byte..end_byte,
            text: slice,
        });
        if end_tok == total {
            return Ok(chunks);
        }
        prev_end_tok = end_tok;
        start_tok = end_tok.saturating_sub(overlap).max(start_tok + 1);
        start_byte = offsets[start_tok].0;
    }
}

/// Overlap, in tokens, for a chunk budget of `budget` tokens.
pub(super) fn overlap_tokens(budget: usize) -> usize {
    budget * CHUNK_OVERLAP_PERCENT / 100
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

/// Why: #10514 — `refs/main` is joined into the snapshot path; `../../x`
/// would read the tokenizer files from an arbitrary directory.
/// What: trims `raw` and accepts only a non-empty name of `[A-Za-z0-9._-]`
/// that is neither `.` nor contains `..`; returns the trimmed name.
/// Test: `traversal_revision_is_rejected_without_reading`,
/// `backslash_revision_is_rejected`, `sha_revision_still_loads`.
fn validate_revision(raw: &str) -> Result<&str, ChunkError> {
    let revision = raw.trim();
    let plain = !revision.is_empty()
        && revision != "."
        && !revision.contains("..")
        && revision
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'.' | b'_' | b'-'));
    if plain {
        Ok(revision)
    } else {
        Err(ChunkError::InvalidRevision {
            revision: revision.to_string(),
        })
    }
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
