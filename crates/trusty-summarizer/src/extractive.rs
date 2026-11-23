//! The extractive inference backends: a summary with no LLM.
//!
//! Why: the cheapest summary modes spend no tokens (plan section 1, step 4).
//! `extractive` ranks sentences by embedding with the local model;
//! `extractive-tf` ranks by term frequency and needs no model at all.
//!
//! What: drops Markdown headings, fenced code and table rows, splits the
//! rest into sentences, scores each (by similarity to the centroid of the
//! sentence embeddings, or by the document frequency of its content words),
//! adds a bonus for early position, and returns the top sentences in
//! document order.

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Instant;

use crate::backend::{InferenceBackend, InferenceRequest, InferenceResponse, Usage};
use crate::embed::{Embedder, LocalEmbedder, ModelSpec, centroid, dot};
use crate::error::{Error, Result};
use crate::segment::{heading_text, truncate_chars};

/// Most sentences embedded per file by the `extractive` mode; the rest are
/// not candidates. Bounds the per-file cost.
const MAX_EMBEDDED_SENTENCES: usize = 64;

const SENTENCE_CHARS: usize = 300;
const MIN_WORDS: usize = 4;
const STOPWORDS: &[&str] = &[
    "the", "and", "for", "are", "but", "not", "you", "all", "any", "can", "had", "her", "was",
    "one", "our", "out", "has", "have", "this", "that", "with", "from", "they", "will", "would",
    "there", "their", "what", "when", "which", "who", "how", "its", "into", "than", "then", "them",
    "these", "those", "been", "were", "also", "each", "only", "other", "some", "such", "more",
    "most", "very", "just", "over", "your", "about", "after", "before", "does", "did", "may",
    "must", "should", "could", "use", "used", "uses", "per", "via",
];

/// How sentences are ranked.
#[derive(Clone)]
pub enum Ranking {
    /// Document frequency of content words (`extractive-tf`).
    TermFrequency,
    /// Similarity to the centroid of the sentence embeddings (`extractive`).
    Embedding(Arc<dyn Embedder>),
}

impl std::fmt::Debug for Ranking {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Ranking::TermFrequency => f.write_str("TermFrequency"),
            Ranking::Embedding(e) => write!(f, "Embedding({})", e.name()),
        }
    }
}

/// Sentence-ranking summarizer, registered as `extractive` (embedding) and
/// `extractive-tf` (term frequency).
#[derive(Debug, Clone)]
pub struct ExtractiveBackend {
    max_sentences: usize,
    ranking: Ranking,
}

impl Default for ExtractiveBackend {
    fn default() -> Self {
        ExtractiveBackend {
            max_sentences: 3,
            ranking: Ranking::TermFrequency,
        }
    }
}

impl ExtractiveBackend {
    /// The term-frequency backend from `[backends.extractive-tf]`; the one
    /// option is `max_sentences` (integer, at least 1, default 3).
    pub fn from_options(options: &toml::Table) -> Result<Self> {
        Self::build("extractive-tf", options, false)
    }

    /// The embedding backend from `[backends.extractive]`: `max_sentences`,
    /// plus `model` and `cache_dir` for the local model (shared with the
    /// `local` classifier when they match, so it loads once).
    pub fn embedding_from_options(options: &toml::Table) -> Result<Self> {
        Self::build("extractive", options, true)
    }

    /// The embedding backend over a given embedder.
    pub fn with_embedder(max_sentences: usize, embedder: Arc<dyn Embedder>) -> Self {
        ExtractiveBackend {
            max_sentences: max_sentences.max(1),
            ranking: Ranking::Embedding(embedder),
        }
    }

    fn build(name: &str, options: &toml::Table, embedding: bool) -> Result<Self> {
        let mut b = ExtractiveBackend::default();
        for (key, value) in options {
            match (key.as_str(), value) {
                ("max_sentences", toml::Value::Integer(n)) if *n >= 1 => {
                    b.max_sentences = usize::try_from(*n).unwrap_or(usize::MAX);
                }
                ("model" | "cache_dir", _) if embedding => {}
                _ => {
                    return Err(Error::Config(format!(
                        "backends.{name}: unsupported option `{key} = {value}`"
                    )));
                }
            }
        }
        if embedding {
            let spec = ModelSpec::from_options(&format!("backends.{name}"), options)?;
            b.ranking = Ranking::Embedding(LocalEmbedder::shared(spec));
        }
        Ok(b)
    }

    /// Picks the summary sentences from `text`.
    ///
    /// Postcondition: at most `max_sentences` sentences, in the order they
    /// appear in `text`; empty only when `text` has no prose.
    /// Errors: the embedder's error, for the `extractive` mode.
    /// Test: `extractive_picks_central_sentences_in_order`,
    /// `embedding_ranking_prefers_central_sentences`
    pub fn summarize(&self, text: &str) -> Result<String> {
        let all = sentences(text);
        let long: Vec<&String> = all
            .iter()
            .filter(|s| s.split_whitespace().count() >= MIN_WORDS)
            .collect();
        let candidates: Vec<&String> = if long.is_empty() {
            all.iter().collect()
        } else {
            long
        };
        if candidates.is_empty() {
            return Ok(String::new());
        }
        let scored = match &self.ranking {
            Ranking::TermFrequency => tf_scores(&candidates),
            Ranking::Embedding(e) => embedding_scores(e.as_ref(), &candidates)?,
        };
        let mut scored = scored;
        scored.sort_by(|a, b| b.1.total_cmp(&a.1).then(a.0.cmp(&b.0)));
        let mut picked: Vec<usize> = scored
            .into_iter()
            .take(self.max_sentences)
            .map(|(i, _)| i)
            .collect();
        picked.sort_unstable();
        Ok(picked
            .into_iter()
            .map(|i| truncate_chars(candidates[i], SENTENCE_CHARS))
            .collect::<Vec<_>>()
            .join(" "))
    }
}

/// `(index, score)` per candidate by content-word document frequency.
fn tf_scores(candidates: &[&String]) -> Vec<(usize, f64)> {
    let mut freq: HashMap<String, f64> = HashMap::new();
    let words: Vec<Vec<String>> = candidates.iter().map(|s| content_words(s)).collect();
    for w in words.iter().flatten() {
        *freq.entry(w.clone()).or_default() += 1.0;
    }
    words
        .iter()
        .enumerate()
        .map(|(i, ws)| {
            let base = if ws.is_empty() {
                0.0
            } else {
                ws.iter().map(|w| freq[w]).sum::<f64>() / (ws.len() as f64).sqrt()
            };
            (i, base * (1.0 + 1.0 / (1.0 + i as f64)))
        })
        .collect()
}

/// `(index, score)` per candidate by cosine to the centroid of the first
/// [`MAX_EMBEDDED_SENTENCES`] candidates' embeddings.
fn embedding_scores(embedder: &dyn Embedder, candidates: &[&String]) -> Result<Vec<(usize, f64)>> {
    let take = candidates.len().min(MAX_EMBEDDED_SENTENCES);
    let texts: Vec<&str> = candidates[..take].iter().map(|s| s.as_str()).collect();
    let vectors = embedder.embed(&texts)?;
    let center = centroid(&vectors).ok_or_else(|| Error::Backend {
        backend: embedder.name().to_string(),
        message: "no sentence vectors".into(),
        usage: None,
    })?;
    Ok(vectors
        .iter()
        .enumerate()
        .map(|(i, v)| {
            let sim = f64::from(dot(v, &center));
            (i, sim * (1.0 + 0.25 / (1.0 + i as f64)))
        })
        .collect())
}

impl InferenceBackend for ExtractiveBackend {
    fn complete(&self, request: &InferenceRequest) -> Result<InferenceResponse> {
        let started = Instant::now();
        let text = self.summarize(&request.input)?;
        Ok(InferenceResponse {
            text,
            usage: Usage::default(),
            latency: started.elapsed(),
        })
    }
}

/// Prose sentences, whitespace-normalized, with Markdown structure removed.
fn sentences(text: &str) -> Vec<String> {
    let mut blocks: Vec<String> = Vec::new();
    let mut current = String::new();
    let mut in_fence = false;
    for line in text.lines() {
        let t = line.trim();
        if t.starts_with("```") || t.starts_with("~~~") {
            in_fence = !in_fence;
            flush(&mut blocks, &mut current);
            continue;
        }
        if in_fence || t.starts_with('|') || heading_text(line).is_some() {
            flush(&mut blocks, &mut current);
            continue;
        }
        if t.is_empty() {
            flush(&mut blocks, &mut current);
            continue;
        }
        let item = strip_list_marker(t);
        if item.len() != t.len() {
            // A list item is its own block.
            flush(&mut blocks, &mut current);
        }
        if !current.is_empty() {
            current.push(' ');
        }
        current.push_str(item);
    }
    flush(&mut blocks, &mut current);

    let mut out = Vec::new();
    for block in blocks {
        let mut sentence = String::new();
        let mut chars = block.chars().peekable();
        while let Some(c) = chars.next() {
            sentence.push(c);
            let ends = matches!(c, '.' | '!' | '?');
            if ends && chars.peek().is_none_or(|n| n.is_whitespace()) {
                push_sentence(&mut out, &sentence);
                sentence.clear();
            }
        }
        push_sentence(&mut out, &sentence);
    }
    out
}

fn flush(blocks: &mut Vec<String>, current: &mut String) {
    if !current.is_empty() {
        blocks.push(std::mem::take(current));
    }
}

fn push_sentence(out: &mut Vec<String>, s: &str) {
    let s = s.split_whitespace().collect::<Vec<_>>().join(" ");
    if !s.is_empty() {
        out.push(s);
    }
}

fn strip_list_marker(t: &str) -> &str {
    for marker in ["- ", "* ", "+ "] {
        if let Some(rest) = t.strip_prefix(marker) {
            return rest.trim_start();
        }
    }
    let digits = t.chars().take_while(char::is_ascii_digit).count();
    if digits > 0
        && let Some(rest) = t[digits..].strip_prefix(". ")
    {
        return rest.trim_start();
    }
    t
}

pub(crate) fn content_words(s: &str) -> Vec<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|w| w.chars().count() >= 3)
        .map(str::to_lowercase)
        .filter(|w| !STOPWORDS.contains(&w.as_str()))
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn extractive_picks_central_sentences_in_order() {
        let text = "# Title\n\nThe cache stores summaries by content hash. \
                    Weather was pleasant on that day overall.\n\n\
                    ```\nlet cache = 1;\n```\n\n\
                    A cache hit returns the stored summary by hash. \
                    Bananas are yellow fruit grown widely.\n";
        let b = ExtractiveBackend {
            max_sentences: 2,
            ranking: Ranking::TermFrequency,
        };
        assert_eq!(
            b.summarize(text).unwrap(),
            "The cache stores summaries by content hash. \
             A cache hit returns the stored summary by hash."
        );
        assert_eq!(b.summarize("# Only a heading\n").unwrap(), "");
    }

    /// Vectors by keyword: "cache" sentences point one way, others another.
    struct TopicEmbedder;

    impl Embedder for TopicEmbedder {
        fn name(&self) -> &str {
            "topic"
        }
        fn embed(&self, texts: &[&str]) -> Result<Vec<Vec<f32>>> {
            Ok(texts
                .iter()
                .map(|t| {
                    if t.contains("cache") {
                        vec![1.0, 0.0]
                    } else {
                        vec![0.0, 1.0]
                    }
                })
                .collect())
        }
    }

    #[test]
    fn embedding_ranking_prefers_central_sentences() {
        let text = "Bananas are yellow fruit grown widely. The cache keeps results by hash. \
                    A cache hit costs nothing at all. Stale cache entries are replaced quickly.";
        let b = ExtractiveBackend::with_embedder(2, Arc::new(TopicEmbedder));
        assert_eq!(
            b.summarize(text).unwrap(),
            "The cache keeps results by hash. A cache hit costs nothing at all."
        );
    }

    #[test]
    fn options_are_validated() {
        let ok: toml::Table = toml::from_str("max_sentences = 1").unwrap();
        assert_eq!(
            ExtractiveBackend::from_options(&ok).unwrap().max_sentences,
            1
        );
        for bad in ["max_sentences = 0", "colour = 1"] {
            let t: toml::Table = toml::from_str(bad).unwrap();
            assert!(ExtractiveBackend::from_options(&t).is_err(), "{bad}");
        }
    }
}
