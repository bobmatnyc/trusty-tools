//! Session history compression with pinned sliding window.
//!
//! Why: Long multi-turn conversations blow the context window; dropping the
//! lowest-signal middle turns keeps the anchor turn (turn 0) and the recent
//! working context intact while shrinking history.
//! What: `compress_history` pins turn 0 and the last N turns, scores middle
//! turns via a simple TF-IDF proxy, and drops lowest-scoring turns until the
//! token budget is met. Input is never mutated.
//! Test: `test_turn_zero_always_pinned` and `test_budget_drops_middle_turns`.

use std::collections::{HashMap, HashSet};

use crate::compress::{CompressConfig, compress, estimate_tokens};

/// A single turn in the conversation history.
#[derive(Debug, Clone)]
pub struct Turn {
    pub role: String,
    pub content: String,
}

/// Configuration for history compression.
#[derive(Debug, Clone)]
pub struct HistoryConfig {
    /// Number of recent turns to always keep (default: 6).
    pub keep_last_n: usize,
    /// Hard token budget for entire history (None = no limit).
    pub token_budget: Option<usize>,
    /// Whether to apply prompt compression to kept turns (default: false).
    pub compress_turns: bool,
    /// Compression config (used if `compress_turns` is true).
    pub compress_config: CompressConfig,
}

impl Default for HistoryConfig {
    fn default() -> Self {
        Self {
            keep_last_n: 6,
            token_budget: None,
            compress_turns: false,
            compress_config: CompressConfig::default(),
        }
    }
}

/// Apply sliding-window compression to a conversation history.
///
/// Why: Guarantees the original user intent (turn 0) and the most recent
/// context are preserved while older middle turns can be dropped.
/// What: Always keeps turn 0 and the last `keep_last_n`; drops the
/// lowest-scoring middle turns until under `token_budget` (if set).
/// Test: `test_turn_zero_always_pinned`.
pub fn compress_history(turns: &[Turn], config: &HistoryConfig) -> Vec<Turn> {
    if turns.is_empty() {
        return vec![];
    }
    if turns.len() <= 1 + config.keep_last_n {
        return maybe_compress_each(turns.to_vec(), config);
    }

    let tail_start = turns.len() - config.keep_last_n;
    let first = turns[0].clone();
    let tail: Vec<Turn> = turns[tail_start..].to_vec();
    let middle: Vec<(usize, Turn)> = turns[1..tail_start]
        .iter()
        .enumerate()
        .map(|(i, t)| (i + 1, t.clone()))
        .collect();

    // Score middle turns for potential eviction.
    let corpus: Vec<Turn> = turns.to_vec();
    let mut scored: Vec<(usize, f64, Turn)> = middle
        .into_iter()
        .map(|(i, t)| {
            let s = score_turn(&t, &corpus);
            (i, s, t)
        })
        .collect();

    // Compute current token budget if set.
    let mut kept_middle: Vec<(usize, Turn)> = if let Some(budget) = config.token_budget {
        // Sort by descending score and add as long as we fit.
        let fixed_cost = estimate_tokens(&first.content) + history_token_count(&tail);
        let remaining = budget.saturating_sub(fixed_cost);
        let mut by_score = scored.clone();
        by_score.sort_by(|a, b| b.1.partial_cmp(&a.1).unwrap_or(std::cmp::Ordering::Equal));
        let mut used = 0usize;
        let mut keep_idxs: HashSet<usize> = HashSet::new();
        for (idx, _, t) in &by_score {
            let cost = estimate_tokens(&t.content);
            if used + cost <= remaining {
                keep_idxs.insert(*idx);
                used += cost;
            }
        }
        scored.retain(|(i, _, _)| keep_idxs.contains(i));
        scored.sort_by_key(|(i, _, _)| *i);
        scored.into_iter().map(|(i, _, t)| (i, t)).collect()
    } else {
        // No budget — keep everything; preserve original order.
        scored.sort_by_key(|(i, _, _)| *i);
        scored.into_iter().map(|(i, _, t)| (i, t)).collect()
    };

    let mut out: Vec<Turn> = Vec::with_capacity(2 + kept_middle.len() + tail.len());
    out.push(first);
    kept_middle.sort_by_key(|(i, _)| *i);
    out.extend(kept_middle.into_iter().map(|(_, t)| t));
    out.extend(tail);

    maybe_compress_each(out, config)
}

fn maybe_compress_each(turns: Vec<Turn>, config: &HistoryConfig) -> Vec<Turn> {
    if !config.compress_turns {
        return turns;
    }
    turns
        .into_iter()
        .map(|t| {
            let compressed = compress(&t.content, &config.compress_config);
            Turn {
                role: t.role,
                content: compressed.text,
            }
        })
        .collect()
}

/// Total estimated tokens across a slice of turns.
pub fn history_token_count(turns: &[Turn]) -> usize {
    turns.iter().map(|t| estimate_tokens(&t.content)).sum()
}

/// Score a turn's informativeness via unique-word ratio weighted by IDF.
///
/// Why: Drop low-signal "ack" turns in the middle before dropping substantive
/// ones.
/// What: Counts unique alphanumeric tokens; multiplies by average IDF across
/// the corpus. Higher = more informative = keep.
/// Test: Covered indirectly by `test_budget_drops_middle_turns`.
fn score_turn(turn: &Turn, corpus: &[Turn]) -> f64 {
    let tokens: Vec<String> = turn
        .content
        .split_whitespace()
        .map(|w| {
            w.chars()
                .filter(|c| c.is_alphanumeric())
                .collect::<String>()
                .to_lowercase()
        })
        .filter(|w| !w.is_empty())
        .collect();
    if tokens.is_empty() {
        return 0.0;
    }
    let unique: HashSet<&String> = tokens.iter().collect();
    let unique_ratio = unique.len() as f64 / tokens.len() as f64;

    let n_docs = corpus.len() as f64;
    let mut df: HashMap<String, usize> = HashMap::new();
    for doc in corpus {
        let doc_set: HashSet<String> = doc
            .content
            .split_whitespace()
            .map(|w| {
                w.chars()
                    .filter(|c| c.is_alphanumeric())
                    .collect::<String>()
                    .to_lowercase()
            })
            .filter(|w| !w.is_empty())
            .collect();
        for term in doc_set {
            *df.entry(term).or_insert(0) += 1;
        }
    }

    let idf_sum: f64 = unique
        .iter()
        .map(|t| {
            let d = *df.get(*t).unwrap_or(&1) as f64;
            (n_docs / d).ln().max(0.0) + 1.0
        })
        .sum();
    let avg_idf = idf_sum / unique.len() as f64;

    // Turn length bonus (log-scale) so substantive turns outrank short acks.
    let len_bonus = ((turn.content.len() as f64).max(1.0)).ln();

    unique_ratio * avg_idf * len_bonus
}

#[cfg(test)]
mod tests {
    use super::*;

    fn make_turns(n: usize) -> Vec<Turn> {
        (0..n)
            .map(|i| Turn {
                role: if i % 2 == 0 {
                    "user".into()
                } else {
                    "assistant".into()
                },
                content: format!("Turn {i} content with some words for scoring purposes"),
            })
            .collect()
    }

    #[test]
    fn test_empty_history_returns_empty() {
        let out = compress_history(&[], &HistoryConfig::default());
        assert!(out.is_empty());
    }

    #[test]
    fn test_short_history_returns_all_turns() {
        let turns = make_turns(5);
        let out = compress_history(&turns, &HistoryConfig::default());
        assert_eq!(out.len(), 5);
    }

    #[test]
    fn test_turn_zero_always_pinned() {
        let turns = make_turns(20);
        let cfg = HistoryConfig {
            keep_last_n: 6,
            token_budget: Some(50),
            ..HistoryConfig::default()
        };
        let out = compress_history(&turns, &cfg);
        assert!(!out.is_empty());
        assert_eq!(out[0].content, turns[0].content);
    }

    #[test]
    fn test_last_n_turns_always_pinned() {
        let turns = make_turns(20);
        let cfg = HistoryConfig {
            keep_last_n: 6,
            token_budget: Some(50),
            ..HistoryConfig::default()
        };
        let out = compress_history(&turns, &cfg);
        let tail_start = out.len().saturating_sub(6);
        for (i, t) in out[tail_start..].iter().enumerate() {
            assert_eq!(t.content, turns[20 - 6 + i].content);
        }
    }

    #[test]
    fn test_budget_drops_middle_turns() {
        let turns = make_turns(20);
        let cfg = HistoryConfig {
            keep_last_n: 6,
            token_budget: Some(100),
            ..HistoryConfig::default()
        };
        let out = compress_history(&turns, &cfg);
        assert!(
            out.len() < turns.len(),
            "expected drop, got {} turns",
            out.len()
        );
        assert_eq!(out[0].content, turns[0].content);
    }

    #[test]
    fn test_no_budget_keeps_all_turns() {
        let turns = make_turns(20);
        let cfg = HistoryConfig {
            keep_last_n: 6,
            token_budget: None,
            ..HistoryConfig::default()
        };
        let out = compress_history(&turns, &cfg);
        assert_eq!(out.len(), turns.len());
    }

    #[test]
    fn test_history_token_count_nonzero() {
        let turns = make_turns(5);
        let count = history_token_count(&turns);
        assert!(count > 0);
    }
}
