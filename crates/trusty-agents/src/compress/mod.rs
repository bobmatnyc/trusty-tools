//! Deterministic NLP prompt compression.
//!
//! Why: Long prompts waste tokens; compressing at API call construction time
//! lets us cut cost without mutating stored originals.
//! What: A pipeline that extracts code/JSON blocks, deduplicates sections,
//! strips stop words and discourse markers, applies a simple TF-IDF filter,
//! and optionally truncates to a token budget.
//! Test: Same input always produces same output (deterministic); code blocks
//! are preserved verbatim; negations like "do not" are never stripped.

pub mod context;
pub mod dedup;
pub mod history;
pub mod output_prompt;
mod pipeline;
pub mod session;
pub mod tool_output;

pub use context::{TokenBudget, truncate_history};
pub use dedup::dedup_sections;
pub use output_prompt::{OutputStyle, output_compression_prompt};
pub use pipeline::estimate_tokens;
pub use tool_output::{compress_tool_output, compress_tool_output_async};

use pipeline::{
    deduplicate_sections, extract_code_blocks, remove_stop_words, restore_code_blocks,
    strip_discourse_markers, tfidf_filter_sentences, truncate_to_budget,
};

/// Configuration for the compression pipeline.
#[derive(Debug, Clone)]
pub struct CompressConfig {
    /// Enable stop-word removal (default: true).
    pub remove_stop_words: bool,
    /// Enable TF-IDF sentence filtering (default: true).
    pub tfidf_filter: bool,
    /// Enable discourse marker removal (default: true).
    pub strip_discourse_markers: bool,
    /// Sentences scoring below this are dropped (0.0-1.0, default: 0.1).
    pub tfidf_threshold: f64,
    /// Optional hard token budget — truncate to this many tokens if set.
    pub token_budget: Option<usize>,
}

impl Default for CompressConfig {
    fn default() -> Self {
        Self {
            remove_stop_words: true,
            tfidf_filter: true,
            strip_discourse_markers: true,
            tfidf_threshold: 0.1,
            token_budget: None,
        }
    }
}

/// Result of compression with metrics.
#[derive(Debug, Clone)]
pub struct CompressResult {
    pub text: String,
    pub original_len: usize,
    pub compressed_len: usize,
    pub reduction_pct: f64,
}

/// Main compression entry point.
///
/// Why: One-call entry for callers that want the full pipeline.
/// What: Protects code blocks, deduplicates, strips stop words / discourse
/// markers, applies TF-IDF filtering, optionally truncates to a token budget.
/// Test: `test_compress_is_deterministic` — same input yields same output.
pub fn compress(input: &str, config: &CompressConfig) -> CompressResult {
    let original_len = input.len();
    if input.is_empty() {
        return CompressResult {
            text: String::new(),
            original_len: 0,
            compressed_len: 0,
            reduction_pct: 0.0,
        };
    }

    // 1. Protect code/JSON blocks.
    let (mut text, blocks) = extract_code_blocks(input);

    // 2. Deduplicate repeated paragraphs.
    text = deduplicate_sections(&text);

    // 3. Strip discourse markers.
    if config.strip_discourse_markers {
        text = strip_discourse_markers(&text);
    }

    // 4. TF-IDF sentence filtering.
    if config.tfidf_filter {
        text = tfidf_filter_sentences(&text, config.tfidf_threshold);
    }

    // 5. Stop-word removal (negation-aware).
    if config.remove_stop_words {
        text = remove_stop_words(&text);
    }

    // 6. Restore protected blocks.
    text = restore_code_blocks(&text, &blocks);

    // 7. Truncate to token budget if set.
    if let Some(budget) = config.token_budget {
        text = truncate_to_budget(&text, budget);
    }

    let compressed_len = text.len();
    let reduction_pct = if original_len > 0 {
        (1.0 - compressed_len as f64 / original_len as f64) * 100.0
    } else {
        0.0
    };

    CompressResult {
        text,
        original_len,
        compressed_len,
        reduction_pct,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_compress_empty_string() {
        let result = compress("", &CompressConfig::default());
        assert!(result.text.is_empty());
        assert_eq!(result.original_len, 0);
        assert_eq!(result.compressed_len, 0);
    }

    #[test]
    fn test_compress_preserves_code_blocks() {
        let input = "Some prose here.\n\n```rust\nfn main() { let x = 1; }\n```\n\nMore prose.";
        let result = compress(input, &CompressConfig::default());
        assert!(
            result.text.contains("fn main() { let x = 1; }"),
            "code body must be preserved verbatim, got: {}",
            result.text
        );
    }

    #[test]
    fn test_compress_deduplicates_repeated_sections() {
        let para = "The quick brown fox jumps over the lazy dog";
        let input = format!("{para}\n\n{para}\n\n{para}");
        let result = compress(
            &input,
            &CompressConfig {
                remove_stop_words: false,
                tfidf_filter: false,
                strip_discourse_markers: false,
                ..CompressConfig::default()
            },
        );
        // Only one occurrence of the paragraph should remain.
        let count = result.text.matches("quick brown").count();
        assert_eq!(
            count, 1,
            "expected 1 occurrence, got {count} in: {}",
            result.text
        );
    }

    #[test]
    fn test_compress_preserves_negations() {
        let input = "You must do not call this function directly.";
        let result = compress(
            input,
            &CompressConfig {
                tfidf_filter: false,
                strip_discourse_markers: false,
                ..CompressConfig::default()
            },
        );
        assert!(
            result.text.to_lowercase().contains("not"),
            "negation dropped: {}",
            result.text
        );
    }

    #[test]
    fn test_compress_strips_discourse_markers() {
        let input = "Furthermore, the system should validate input carefully.";
        let result = compress(
            input,
            &CompressConfig {
                remove_stop_words: false,
                tfidf_filter: false,
                ..CompressConfig::default()
            },
        );
        assert!(
            !result.text.to_lowercase().contains("furthermore"),
            "discourse marker not stripped: {}",
            result.text
        );
    }

    #[test]
    fn test_compress_reduces_tokens() {
        let input = "This is a very long paragraph about the system. \
            It contains many words that should be removed by the stop word filter. \
            Furthermore, there are also discourse markers in this text. \
            Moreover, the system is indeed quite verbose by design.";
        let result = compress(input, &CompressConfig::default());
        assert!(
            result.compressed_len < result.original_len,
            "compression did not reduce size: {} -> {}",
            result.original_len,
            result.compressed_len
        );
    }

    #[test]
    fn test_compress_is_deterministic() {
        let input = "The quick brown fox jumps over the lazy dog. \
            Additionally, the dog was not amused by this turn of events.";
        let a = compress(input, &CompressConfig::default());
        let b = compress(input, &CompressConfig::default());
        assert_eq!(a.text, b.text);
    }

    #[test]
    fn test_extract_code_blocks_roundtrip() {
        let input = "Before.\n\n```py\nprint('hi')\n```\n\nBetween `inline` and after.";
        let (stripped, blocks) = extract_code_blocks(input);
        let restored = restore_code_blocks(&stripped, &blocks);
        assert_eq!(restored, input);
    }

    #[test]
    fn test_estimate_tokens_rough() {
        let text: String = std::iter::repeat_n("word", 100)
            .collect::<Vec<_>>()
            .join(" ");
        let est = estimate_tokens(&text);
        assert!((120..=140).contains(&est), "estimate out of range: {est}");
    }

    #[test]
    fn test_truncate_to_budget() {
        let input = "Sentence one. Sentence two is here. Sentence three finishes it.";
        let result = compress(
            input,
            &CompressConfig {
                remove_stop_words: false,
                tfidf_filter: false,
                strip_discourse_markers: false,
                token_budget: Some(5),
                ..CompressConfig::default()
            },
        );
        assert!(estimate_tokens(&result.text) <= 6);
    }
}
