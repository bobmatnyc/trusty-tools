//! Tests for `Embedder::input_window` and the token-exact chunker (#9275).
//!
//! Two tokenizers: a BERT pipeline (`BertNormalizer`, `BertPreTokenizer`,
//! `WordPiece`, `BertProcessing`) over a small vocab, built in-process so the
//! split logic is tested with no model files; and the real all-MiniLM-L6-v2
//! tokenizer from the fastembed cache, which CI pre-seeds for the reference
//! accuracy gate.

use super::chunk::overlap_tokens;
use super::fast_embedder::MINILM_MAX_SEQ_LENGTH;
use super::test_env::{EnvVarGuard, env_lock};
use super::{
    CHUNK_OVERLAP_PERCENT, ChunkError, ChunkTokenizer, Embedder, FASTEMBED_MAX_LENGTH,
    FastEmbedder, MockEmbedder, TextChunk, chunk_text, resolve_fastembed_cache_dir,
};
use anyhow::Result;
use async_trait::async_trait;
use fastembed::EmbeddingModel;

/// The fp32 default model fastembed loads (`EmbeddingModel::AllMiniLML6V2`).
const DEFAULT_MODEL_CODE: &str = "Qdrant/all-MiniLM-L6-v2-onnx";

/// Whole words the small vocab knows; one token each.
const WORDS: [&str; 8] = [
    "alpha", "bravo", "charlie", "delta", "echo", "foxtrot", "golf", "hotel",
];

/// `tokenizer.json` for a BERT pipeline whose vocab holds `WORDS`, single
/// letters and digits, their `##` continuations, and some punctuation. Any
/// other word splits into one token per character. The file's own truncation
/// is 16, so a loader that keeps it under-counts.
fn small_bert_tokenizer() -> ChunkTokenizer {
    small_bert_tokenizer_with(&[])
}

/// [`small_bert_tokenizer`] with `extra` vocab entries appended.
fn small_bert_tokenizer_with(extra: &[&str]) -> ChunkTokenizer {
    ChunkTokenizer::from_bytes(small_bert_tokenizer_json(extra).as_bytes())
        .expect("small BERT tokenizer loads")
}

/// The `tokenizer.json` behind [`small_bert_tokenizer_with`].
fn small_bert_tokenizer_json(extra: &[&str]) -> String {
    let specials = ["[PAD]", "[UNK]", "[CLS]", "[SEP]", "[MASK]"];
    let mut vocab = serde_json::Map::new();
    let mut push = |token: String| {
        let id = vocab.len();
        vocab.insert(token, id.into());
    };
    for s in specials {
        push(s.to_string());
    }
    for c in ('a'..='z').chain('0'..='9') {
        push(c.to_string());
        push(format!("##{c}"));
    }
    for p in [".", ",", "!", "?", "-"] {
        push(p.to_string());
    }
    for w in WORDS.iter().chain(extra) {
        push(w.to_string());
    }
    let added: Vec<serde_json::Value> = specials
        .iter()
        .enumerate()
        .map(|(id, content)| {
            serde_json::json!({
                "id": id, "content": content, "single_word": false, "lstrip": false,
                "rstrip": false, "normalized": false, "special": true
            })
        })
        .collect();
    let json = serde_json::json!({
        "version": "1.0",
        "truncation": {"direction": "Right", "max_length": 16, "strategy": "LongestFirst", "stride": 0},
        "padding": null,
        "added_tokens": added,
        "normalizer": {"type": "BertNormalizer", "clean_text": true, "handle_chinese_chars": true,
                       "strip_accents": null, "lowercase": true},
        "pre_tokenizer": {"type": "BertPreTokenizer"},
        "post_processor": {"type": "BertProcessing", "sep": ["[SEP]", 3], "cls": ["[CLS]", 2]},
        "decoder": {"type": "WordPiece", "prefix": "##", "cleanup": true},
        "model": {"type": "WordPiece", "unk_token": "[UNK]", "continuing_subword_prefix": "##",
                  "max_input_chars_per_word": 1_000_000, "vocab": vocab}
    });
    json.to_string()
}

/// The real tokenizer fastembed's default model truncates with.
fn real_tokenizer() -> ChunkTokenizer {
    // #5937 lock order: crate-wide, then the embedder lock — sibling tests
    // point FASTEMBED_CACHE_DIR at a scratch path while holding both.
    let cache_dir = {
        let _home = crate::data_dir::ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let _g = env_lock();
        std::env::var("HF_HOME")
            .map(std::path::PathBuf::from)
            .unwrap_or_else(|_| resolve_fastembed_cache_dir())
    };
    ChunkTokenizer::for_fastembed_model_in(&cache_dir, DEFAULT_MODEL_CODE)
        .expect("fastembed cache holds the default model's tokenizer")
}

/// `n` sentences of whole words, e.g. "Alpha bravo charlie. Delta echo ...".
fn sentences(n: usize) -> String {
    (0..n)
        .map(|i| {
            let words: Vec<&str> = (0..5).map(|j| WORDS[(i * 3 + j) % WORDS.len()]).collect();
            format!("{}.", words.join(" "))
        })
        .collect::<Vec<_>>()
        .join(" ")
}

/// Chunk `text` and assert the #9275 properties: every chunk satisfies
/// `count + 2 <= window`, matches its byte range, and the ranges start at 0,
/// end at `text.len()`, move forward and leave no gap.
fn assert_bounded_and_covered<'a>(
    tok: &ChunkTokenizer,
    text: &'a str,
    window: usize,
) -> Vec<TextChunk<'a>> {
    let chunks = chunk_text(tok, text, window).expect("chunking succeeds");
    assert!(!chunks.is_empty(), "at least one chunk");
    assert_eq!(tok.special_overhead(), 2, "BERT adds [CLS] and [SEP]");
    for c in &chunks {
        let n = tok.count_tokens(c.text).expect("count");
        assert!(
            n + 2 <= window,
            "chunk {:?} has {n} tokens; {n} + 2 > window {window}",
            c.byte_range
        );
        assert_eq!(&text[c.byte_range.clone()], c.text);
    }
    assert_eq!(chunks[0].byte_range.start, 0, "no dropped head");
    assert_eq!(
        chunks[chunks.len() - 1].byte_range.end,
        text.len(),
        "no dropped tail"
    );
    for pair in chunks.windows(2) {
        let (a, b) = (&pair[0].byte_range, &pair[1].byte_range);
        assert!(b.start <= a.end, "gap between {a:?} and {b:?}");
        assert!(
            b.start > a.start && b.end > a.end,
            "{b:?} does not advance past {a:?}"
        );
    }
    chunks
}

#[test]
fn text_within_window_is_one_chunk_equal_to_input() {
    let tok = small_bert_tokenizer();
    let text = "Alpha bravo charlie, zulu!";
    let chunks = chunk_text(&tok, text, 64).expect("chunk");
    assert_eq!(chunks.len(), 1);
    assert_eq!(chunks[0].text, text);
    assert_eq!(chunks[0].byte_range, 0..text.len());

    // Exactly at the bound: count + 2 == window is still one chunk.
    let n = tok.count_tokens(text).expect("count");
    assert_eq!(chunk_text(&tok, text, n + 2).expect("chunk").len(), 1);
    assert!(chunk_text(&tok, text, n + 1).expect("chunk").len() > 1);
}

#[test]
fn empty_input_is_one_empty_chunk() {
    let tok = small_bert_tokenizer();
    let chunks = chunk_text(&tok, "", 64).expect("chunk");
    assert_eq!(
        chunks,
        vec![TextChunk {
            byte_range: 0..0,
            text: ""
        }]
    );
}

#[test]
fn long_multi_sentence_text_is_bounded_and_covered() {
    let tok = small_bert_tokenizer();
    // Mixed vocabulary: whole words, split words, punctuation, padding spaces.
    let text = format!("  {} zulu yankee xray! {}  ", sentences(150), sentences(60));
    assert!(tok.count_tokens(&text).expect("count") > 1000);
    let chunks = assert_bounded_and_covered(&tok, &text, 64);
    assert!(chunks.len() > 15, "{} chunks", chunks.len());
}

#[test]
fn unbroken_run_without_whitespace_is_bounded_and_covered() {
    let tok = small_bert_tokenizer();
    // One 600-character word: every character is its own token, so every
    // cut falls inside the word. (WordPiece is quadratic per word; 600 keeps
    // the test fast.)
    let run = "zulu".repeat(150);
    assert_eq!(tok.count_tokens(&run).expect("count"), 600);
    assert_bounded_and_covered(&tok, &run, 32);
    // Punctuation-joined run, and multi-byte CJK characters (one token each).
    assert_bounded_and_covered(&tok, &"a.b-c,".repeat(800), 32);
    assert_bounded_and_covered(&tok, &"漢字".repeat(500), 32);
}

#[test]
fn chunk_ends_advance_past_a_word_longer_than_the_step() {
    // #9275: a chunk that ends at the start of a word longer than
    // `budget - overlap` tokens must not make the next chunks end there too.
    let window = 32;
    let overlap = overlap_tokens(window - 2);
    assert_eq!(overlap, 3);
    let long_word = "zulu".repeat(150);
    let stalled =
        format!("alpha bravo charlie delta echo foxtrot golf hotel alpha bravo {long_word}");
    // `##q{40}` is one token after `z`, 40 at a word start: the overlap that
    // starts on it re-counts past the budget, so the overlap is dropped.
    let q40 = "q".repeat(40);
    let tok = small_bert_tokenizer_with(&[&format!("##{q40}")]);
    let ballooning = format!("z{q40} alpha bravo {long_word}");
    for text in [&stalled, &ballooning] {
        let chunks = assert_bounded_and_covered(&tok, text, window);
        for pair in chunks.windows(2) {
            let shared = &text[pair[1].byte_range.start..pair[0].byte_range.end];
            let n = tok.count_tokens(shared).expect("count");
            assert!(n <= overlap, "overlap {shared:?} is {n} tokens > {overlap}");
        }
        for (i, a) in chunks.iter().enumerate() {
            for b in &chunks[i + 1..] {
                let (a, b) = (&a.byte_range, &b.byte_range);
                assert!(b.end > a.end, "{b:?} is contained in {a:?}");
            }
        }
    }
}

#[test]
fn consecutive_chunks_overlap_by_the_fixed_constant() {
    let tok = small_bert_tokenizer();
    // One token per word, so a byte overlap counts as whole tokens.
    let text = (0..600)
        .map(|i| WORDS[i % WORDS.len()])
        .collect::<Vec<_>>()
        .join(" ");
    let window = 102;
    let overlap = overlap_tokens(window - 2);
    assert_eq!(overlap, 10);
    let chunks = assert_bounded_and_covered(&tok, &text, window);
    for pair in chunks.windows(2) {
        let shared = &text[pair[1].byte_range.start..pair[0].byte_range.end];
        assert_eq!(
            tok.count_tokens(shared).expect("count"),
            overlap,
            "{shared:?}"
        );
    }
}

#[test]
fn chunk_overlap_is_at_most_ten_percent_of_window() {
    assert_eq!(CHUNK_OVERLAP_PERCENT, 10);
    for window in [3, 16, 128, 256, 512, 8192] {
        let overlap = overlap_tokens(window - 2);
        assert!(overlap * 10 <= window, "window {window}: overlap {overlap}");
        assert!(overlap < window - 2, "window {window}: chunks must advance");
    }
    assert_eq!(overlap_tokens(MINILM_MAX_SEQ_LENGTH - 2), 25);
    assert_eq!(overlap_tokens(126), 12);
}

#[test]
fn count_ignores_the_truncation_in_tokenizer_json() {
    let tok = small_bert_tokenizer();
    // The file truncates at 16; the count must not.
    assert_eq!(tok.count_tokens(&sentences(10)).expect("count"), 60);
    assert_eq!(tok.count_tokens("alpha").expect("count"), 1);
}

#[test]
fn window_not_above_special_tokens_is_an_error() {
    let tok = small_bert_tokenizer();
    for window in [0, 1, 2] {
        let result = chunk_text(&tok, "alpha", window);
        assert!(
            matches!(result, Err(ChunkError::WindowTooSmall { overhead: 2, .. })),
            "window {window}: {result:?}"
        );
    }
}

#[test]
fn fastembed_cache_without_the_model_is_an_error() {
    let empty = tempfile::tempdir().expect("tempdir");
    let result = ChunkTokenizer::for_fastembed_model_in(empty.path(), DEFAULT_MODEL_CODE);
    assert!(
        matches!(result, Err(ChunkError::TokenizerMissing { .. })),
        "{:?}",
        result.err()
    );
}

/// A fastembed (HuggingFace) cache holding `DEFAULT_MODEL_CODE` with the
/// small BERT tokenizer and the given `model_max_length`.
fn fixture_fastembed_cache(model_max_length: usize) -> tempfile::TempDir {
    let dir = tempfile::tempdir().expect("tempdir");
    let repo = dir
        .path()
        .join(format!("models--{}", DEFAULT_MODEL_CODE.replace('/', "--")));
    let snapshot = repo.join("snapshots").join("fixture");
    std::fs::create_dir_all(&snapshot).expect("snapshot dir");
    std::fs::create_dir_all(repo.join("refs")).expect("refs dir");
    let files = [
        (repo.join("refs").join("main"), "fixture".to_string()),
        (
            snapshot.join("tokenizer.json"),
            small_bert_tokenizer_json(&[]),
        ),
        (
            snapshot.join("tokenizer_config.json"),
            serde_json::json!({"model_max_length": model_max_length, "pad_token": "[PAD]"})
                .to_string(),
        ),
        (
            snapshot.join("special_tokens_map.json"),
            serde_json::json!({"cls_token": "[CLS]", "sep_token": "[SEP]", "pad_token": "[PAD]",
                               "unk_token": "[UNK]", "mask_token": "[MASK]"})
            .to_string(),
        ),
    ];
    for (path, body) in files {
        std::fs::write(&path, body).expect("write fixture file");
    }
    dir
}

#[test]
fn fastembed_truncates_at_minilm_window_on_every_model_revision() {
    // fastembed 5.13.4 truncates at min(TextInitOptions::max_length,
    // tokenizer_config.json `model_max_length`) — `common.rs` `load_tokenizer`.
    let scratch = tempfile::tempdir().expect("tempdir");
    for model in [
        EmbeddingModel::AllMiniLML6V2,
        EmbeddingModel::AllMiniLML6V2Q,
    ] {
        let opts = FastEmbedder::text_init_options(model, scratch.path().to_path_buf());
        assert_eq!(opts.max_length, MINILM_MAX_SEQ_LENGTH);
    }
    assert_eq!(FASTEMBED_MAX_LENGTH, MINILM_MAX_SEQ_LENGTH);
    assert_eq!(MINILM_MAX_SEQ_LENGTH, 256);
    // `model_max_length` is 512 at Qdrant/all-MiniLM-L6-v2-onnx revision
    // 5f1b8cd78bc4 and 256 from 944c7f61a294 on; a smaller cap still wins.
    for (model_max, want) in [(512, 256), (256, 256), (128, 128)] {
        let cache = fixture_fastembed_cache(model_max);
        let tok = ChunkTokenizer::for_fastembed_model_in(cache.path(), DEFAULT_MODEL_CODE)
            .expect("fixture cache loads");
        assert_eq!(tok.max_length(), Some(want), "model_max_length {model_max}");
        assert_eq!(tok.special_overhead(), 2);
    }
}

#[test]
fn production_init_options_pass_the_minilm_window_to_fastembed() {
    // The options `FastEmbedder` hands `TextEmbedding::try_new`. The cache
    // dir points at a tempdir: nothing reads the host cache or downloads.
    let scratch = tempfile::tempdir().expect("tempdir");
    let _home = crate::data_dir::ENV_LOCK
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let _g = env_lock();
    let dir = scratch.path().to_str().expect("utf-8 tempdir");
    let _cache = EnvVarGuard::apply("FASTEMBED_CACHE_DIR", Some(dir));
    let _device = EnvVarGuard::apply("TRUSTY_DEVICE", None);
    for model in [
        EmbeddingModel::AllMiniLML6V2,
        EmbeddingModel::AllMiniLML6V2Q,
    ] {
        let (opts, _provider) = FastEmbedder::init_options(model);
        assert_eq!(opts.max_length, MINILM_MAX_SEQ_LENGTH);
        assert_eq!(opts.cache_dir, scratch.path());
    }
}

#[test]
fn real_tokenizer_chunks_long_text_within_fastembed_window() {
    let tok = real_tokenizer();
    // The snapshot's tokenizer.json pads and truncates (128 or 256, by
    // revision); both must be off.
    assert_eq!(tok.count_tokens("hello world").expect("count"), 2);
    let text = (0..400)
        .map(|i| format!("Sentence {i} records that the palace stores drawer number {i}."))
        .collect::<Vec<_>>()
        .join(" ");
    assert!(tok.count_tokens(&text).expect("count") > 5 * MINILM_MAX_SEQ_LENGTH);
    let chunks = assert_bounded_and_covered(&tok, &text, MINILM_MAX_SEQ_LENGTH);
    assert!(chunks.len() >= 5, "{} chunks", chunks.len());
    // The candle window, and pathological runs with no whitespace.
    assert_bounded_and_covered(&tok, &text, 128);
    assert_bounded_and_covered(&tok, &"x9-q".repeat(2000), MINILM_MAX_SEQ_LENGTH);
    assert_bounded_and_covered(&tok, &"漢字".repeat(1500), 128);
}

struct NoWindow;

#[async_trait]
impl Embedder for NoWindow {
    async fn embed_batch(&self, texts: &[String]) -> Result<Vec<Vec<f32>>> {
        Ok(texts.iter().map(|_| vec![0.0]).collect())
    }

    fn dimension(&self) -> usize {
        1
    }
}

#[test]
fn default_input_window_is_none() {
    assert_eq!(NoWindow.input_window(), None);
}

#[test]
fn mock_embedder_input_window_is_configurable() {
    assert_eq!(MockEmbedder::new(8).input_window(), None);
    assert_eq!(
        MockEmbedder::new(8).with_input_window(64).input_window(),
        Some(64)
    );
}
