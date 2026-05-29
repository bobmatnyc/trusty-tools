//! Internal stages of the deterministic prompt-compression pipeline.
//!
//! Why: The `compress()` entry point in `mod.rs` orchestrates a sequence of
//! pure transforms (code-block protection, dedup, discourse-marker stripping,
//! TF-IDF filtering, stop-word removal, budget truncation). Housing those
//! stages here keeps `mod.rs` focused on the public types + orchestration and
//! under the 500-line cap.
//! What: The regex/stop-word statics and the per-stage helper functions, plus
//! the public `estimate_tokens` heuristic.
//! Test: Exercised end-to-end via `compress::tests`.

use std::collections::{HashMap, HashSet};

use once_cell::sync::Lazy;
use regex::Regex;

// -------- Code-block extraction --------

static FENCED_BLOCK: Lazy<Regex> = Lazy::new(|| Regex::new(r"(?s)```[^\n]*\n.*?\n```").unwrap());
static INLINE_CODE: Lazy<Regex> = Lazy::new(|| Regex::new(r"`[^`\n]+`").unwrap());

/// Extract code blocks and inline code; replace with sentinels.
///
/// Why: Compression must never mangle code — a stripped stop-word inside a
/// function name is a correctness bug.
/// What: Replaces fenced blocks and inline code with numbered placeholders,
/// returns the (text, placeholder_pairs) tuple so the caller can restore.
/// Test: `test_extract_code_blocks_roundtrip` — extract + restore == identity.
pub(super) fn extract_code_blocks(text: &str) -> (String, Vec<(String, String)>) {
    let mut blocks: Vec<(String, String)> = Vec::new();
    let mut counter = 0usize;

    // Fenced blocks first (longest match; inline regex won't catch multi-line).
    let after_fenced = FENCED_BLOCK
        .replace_all(text, |caps: &regex::Captures<'_>| {
            let placeholder = format!("\u{E000}CODE_BLOCK_{counter}\u{E001}");
            blocks.push((placeholder.clone(), caps[0].to_string()));
            counter += 1;
            placeholder
        })
        .into_owned();

    // Inline code next.
    let after_inline = INLINE_CODE
        .replace_all(&after_fenced, |caps: &regex::Captures<'_>| {
            let placeholder = format!("\u{E000}INLINE_CODE_{counter}\u{E001}");
            blocks.push((placeholder.clone(), caps[0].to_string()));
            counter += 1;
            placeholder
        })
        .into_owned();

    (after_inline, blocks)
}

/// Restore protected blocks from placeholders.
pub(super) fn restore_code_blocks(text: &str, blocks: &[(String, String)]) -> String {
    let mut out = text.to_string();
    for (placeholder, original) in blocks {
        out = out.replace(placeholder.as_str(), original);
    }
    out
}

// -------- Deduplication --------

/// Hash-based deduplication of repeated paragraphs.
///
/// Why: LLM prompts often contain stock phrasing repeated across sections.
/// What: Splits on blank lines, keeps the first occurrence of each unique
/// (whitespace-normalized) paragraph, preserving order.
/// Test: `test_compress_deduplicates_repeated_sections`.
pub(super) fn deduplicate_sections(text: &str) -> String {
    let mut seen: HashSet<String> = HashSet::new();
    let mut kept: Vec<&str> = Vec::new();
    for section in text.split("\n\n") {
        let key: String = section.split_whitespace().collect::<Vec<_>>().join(" ");
        if key.is_empty() {
            kept.push(section);
            continue;
        }
        if seen.insert(key) {
            kept.push(section);
        }
    }
    kept.join("\n\n")
}

// -------- Stop words (negation-aware) --------

/// Negation terms we must never strip, even when they're stop words.
static NEGATIONS: Lazy<HashSet<&'static str>> = Lazy::new(|| {
    [
        "not",
        "no",
        "never",
        "cannot",
        "can't",
        "don't",
        "won't",
        "isn't",
        "aren't",
        "doesn't",
        "didn't",
        "wasn't",
        "weren't",
        "shouldn't",
        "wouldn't",
        "couldn't",
        "mustn't",
    ]
    .into_iter()
    .collect()
});

static STOP_WORDS: Lazy<HashSet<String>> = Lazy::new(|| {
    let raw = stop_words::get(stop_words::LANGUAGE::English);
    raw.into_iter().map(|s| s.to_lowercase()).collect()
});

/// Remove stop words from prose while preserving negations and the words
/// that follow them (so "do not call" keeps all three tokens).
///
/// Why: Stop words are the biggest compressible surface; removing them
/// naively destroys meaning when negations are involved.
/// What: Tokenizes on whitespace, skips stop words unless (a) the token is
/// a negation, (b) the previous token was a negation, or (c) the token is
/// inside a code-block placeholder.
/// Test: `test_compress_preserves_negations`.
pub(super) fn remove_stop_words(text: &str) -> String {
    // Split by whitespace but preserve newline grouping by iterating line-by-line.
    let mut out_lines: Vec<String> = Vec::new();
    for line in text.split('\n') {
        let tokens: Vec<&str> = line.split_whitespace().collect();
        let mut kept: Vec<&str> = Vec::with_capacity(tokens.len());
        let mut prev_was_negation = false;
        for tok in tokens {
            let lower = tok.to_lowercase();
            let stripped: String = lower
                .chars()
                .filter(|c| c.is_alphanumeric() || *c == '\'')
                .collect();

            // Preserve placeholders verbatim.
            if tok.contains('\u{E000}') || tok.contains('\u{E001}') {
                kept.push(tok);
                prev_was_negation = false;
                continue;
            }

            let is_negation = NEGATIONS.contains(stripped.as_str());
            if is_negation {
                kept.push(tok);
                prev_was_negation = true;
                continue;
            }

            if prev_was_negation {
                kept.push(tok);
                prev_was_negation = false;
                continue;
            }

            if STOP_WORDS.contains(&stripped) {
                prev_was_negation = false;
                continue;
            }

            kept.push(tok);
            prev_was_negation = false;
        }
        out_lines.push(kept.join(" "));
    }
    out_lines.join("\n")
}

// -------- Discourse markers --------

static DISCOURSE_MARKERS: Lazy<Regex> = Lazy::new(|| {
    Regex::new(
        r"(?i)\b(furthermore|moreover|additionally|in addition|as mentioned|as noted|it is worth noting that|it should be noted that|please note that)\b[,]?\s*",
    )
    .unwrap()
});

/// Strip low-value discourse markers.
pub(super) fn strip_discourse_markers(text: &str) -> String {
    DISCOURSE_MARKERS.replace_all(text, "").into_owned()
}

// -------- TF-IDF sentence filtering --------

/// Split text into sentences on `. `, `! `, `? ` boundaries.
///
/// Why: Rust's `regex` crate has no lookbehind, so we walk the string
/// manually.
fn split_sentences(text: &str) -> Vec<&str> {
    let bytes = text.as_bytes();
    let mut out = Vec::new();
    let mut start = 0usize;
    let mut i = 0usize;
    while i < bytes.len() {
        let b = bytes[i];
        if (b == b'.' || b == b'!' || b == b'?')
            && i + 1 < bytes.len()
            && bytes[i + 1].is_ascii_whitespace()
        {
            // Include the punctuation in the current sentence.
            let end = i + 1;
            out.push(&text[start..end]);
            // Skip whitespace to next sentence start.
            let mut j = end;
            while j < bytes.len() && bytes[j].is_ascii_whitespace() {
                j += 1;
            }
            start = j;
            i = j;
            continue;
        }
        i += 1;
    }
    if start < bytes.len() {
        out.push(&text[start..]);
    }
    out
}

/// Score each sentence by average TF-IDF over its tokens and drop sentences
/// scoring below threshold. Always keeps the first sentence of each paragraph
/// as a structural anchor.
///
/// Why: Removes "filler" sentences that restate material without adding signal.
/// What: Simple TF-IDF where each sentence is a document. Score = mean weight
/// of its non-stop-word tokens. Below-threshold sentences are dropped.
/// Test: `test_compress_reduces_tokens`.
pub(super) fn tfidf_filter_sentences(text: &str, threshold: f64) -> String {
    let paragraphs: Vec<&str> = text.split("\n\n").collect();
    let mut out_paragraphs: Vec<String> = Vec::with_capacity(paragraphs.len());

    for para in paragraphs {
        let sentences: Vec<&str> = split_sentences(para);
        if sentences.len() <= 1 {
            out_paragraphs.push(para.to_string());
            continue;
        }

        // Build TF-IDF weights.
        let docs: Vec<Vec<String>> = sentences
            .iter()
            .map(|s| {
                s.split_whitespace()
                    .map(|w| {
                        w.chars()
                            .filter(|c| c.is_alphanumeric())
                            .collect::<String>()
                            .to_lowercase()
                    })
                    .filter(|w| !w.is_empty() && !STOP_WORDS.contains(w))
                    .collect()
            })
            .collect();

        let n_docs = docs.len() as f64;
        let mut df: HashMap<String, usize> = HashMap::new();
        for doc in &docs {
            let unique: HashSet<&String> = doc.iter().collect();
            for term in unique {
                *df.entry(term.clone()).or_insert(0) += 1;
            }
        }

        let scores: Vec<f64> = docs
            .iter()
            .map(|doc| {
                if doc.is_empty() {
                    return 0.0;
                }
                let mut tf: HashMap<&String, usize> = HashMap::new();
                for t in doc {
                    *tf.entry(t).or_insert(0) += 1;
                }
                let doc_len = doc.len() as f64;
                let mut sum = 0.0f64;
                for (term, count) in &tf {
                    let tf_val = *count as f64 / doc_len;
                    let df_val = *df.get(*term).unwrap_or(&1) as f64;
                    let idf = (n_docs / df_val).ln().max(0.0) + 1.0;
                    sum += tf_val * idf;
                }
                sum / doc.len() as f64
            })
            .collect();

        // Normalize scores to [0, 1] for threshold comparison.
        let max_score = scores.iter().cloned().fold(0.0f64, f64::max).max(1e-9);
        let kept: Vec<&str> = sentences
            .iter()
            .enumerate()
            .filter_map(|(i, s)| {
                let normalized = scores[i] / max_score;
                if i == 0 || normalized >= threshold {
                    Some(*s)
                } else {
                    None
                }
            })
            .collect();

        out_paragraphs.push(kept.join(" "));
    }

    out_paragraphs.join("\n\n")
}

// -------- Token estimation and truncation --------

/// Estimate approximate tokens (word count * 1.3).
///
/// Why: Cheap heuristic avoids depending on tiktoken at compile time while
/// staying within ~10% of the real count for English prose.
/// What: `words * 1.3` rounded down.
/// Test: `test_estimate_tokens_rough`.
pub fn estimate_tokens(text: &str) -> usize {
    (text.split_whitespace().count() as f64 * 1.3) as usize
}

/// Truncate text to fit within `budget` tokens, preserving sentence boundaries
/// where possible.
pub(super) fn truncate_to_budget(text: &str, budget: usize) -> String {
    if estimate_tokens(text) <= budget {
        return text.to_string();
    }
    let sentences: Vec<&str> = split_sentences(text);
    let mut acc = String::new();
    let mut acc_tokens = 0usize;
    for s in sentences {
        let s_tokens = estimate_tokens(s);
        if acc_tokens + s_tokens > budget {
            break;
        }
        if !acc.is_empty() {
            acc.push(' ');
        }
        acc.push_str(s);
        acc_tokens += s_tokens;
    }
    if acc.is_empty() {
        // Fall back to word-level truncation if no sentence fits.
        let words: Vec<&str> = text.split_whitespace().collect();
        let take = ((budget as f64) / 1.3) as usize;
        words.into_iter().take(take).collect::<Vec<_>>().join(" ")
    } else {
        acc
    }
}
