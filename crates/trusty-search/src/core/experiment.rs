//! Opt-in, deterministic chunk-context treatments for the isolated search experiment.
//!
//! Why: compare lexical context without changing evidence or invoking a model.
//! What: frozen configuration, bounded persisted context, and source navigation cards.
//! Test: `configuration_grid_and_rejections`, `context_preserves_evidence_and_budget`.

use std::collections::{HashMap, HashSet};
use std::sync::OnceLock;

use serde::Serialize;

use super::{CodeChunk, RawChunk};

#[derive(Clone, Copy, Debug, Eq, PartialEq, Serialize)]
pub struct ExperimentConfig {
    pub context_words: usize,
    pub subchunk_window: usize,
}

#[derive(Debug, thiserror::Error)]
pub enum ExperimentConfigError {
    #[error("context words must be 0, 64, or 128; received {0:?}")]
    InvalidContextWords(String),
    #[error("subchunk window must be 100 or 64; received {0:?}")]
    InvalidSubchunkWindow(String),
}

/// Why: treatment selection must be reproducible and reject unsupported cells.
/// What: pure parser; omitted settings reproduce the existing behavior.
/// Test: `configuration_grid_and_rejections`.
pub fn parse_experiment_config(
    context_words: Option<&str>,
    subchunk_window: Option<&str>,
) -> Result<ExperimentConfig, ExperimentConfigError> {
    let context_words = match context_words.unwrap_or("0") {
        "0" => 0,
        "64" => 64,
        "128" => 128,
        other => return Err(ExperimentConfigError::InvalidContextWords(other.into())),
    };
    let subchunk_window = match subchunk_window.unwrap_or("100") {
        "100" => 100,
        "64" => 64,
        other => return Err(ExperimentConfigError::InvalidSubchunkWindow(other.into())),
    };
    Ok(ExperimentConfig {
        context_words,
        subchunk_window,
    })
}

/// Why: all indexing workers must use one immutable treatment.
/// What: resolves environment once; invalid explicit settings fail immediately.
/// Test: `configuration_grid_and_rejections` covers the pure parser.
pub fn experiment_config() -> &'static ExperimentConfig {
    static CONFIG: OnceLock<ExperimentConfig> = OnceLock::new();
    CONFIG.get_or_init(|| {
        let context = std::env::var("TRUSTY_SEARCH_EXPERIMENT_CONTEXT_WORDS").ok();
        let window = std::env::var("TRUSTY_SEARCH_EXPERIMENT_SUBCHUNK_WINDOW").ok();
        parse_experiment_config(context.as_deref(), window.as_deref())
            .expect("invalid trusty-search experiment configuration")
    })
}

fn declaration(line: &str) -> bool {
    let line = line.trim_start();
    [
        "pub ",
        "fn ",
        "async fn ",
        "impl ",
        "struct ",
        "enum ",
        "trait ",
        "class ",
        "def ",
        "function ",
        "export ",
        "#",
        "interface ",
    ]
    .iter()
    .any(|prefix| line.starts_with(prefix))
}

fn documentation(line: &str) -> bool {
    let line = line.trim();
    ["//", "/*", "*", "#", "\"\"\"", "'''", "<!--"]
        .iter()
        .any(|prefix| line.starts_with(prefix))
}

/// Why: index-time context can improve lexical recall without replacing source.
/// What: appends deduplicated words from a relative path, symbol, adjacent docs,
/// and the immediate parent signature. Zero budget is a strict no-op. The caller
/// supplies chunks from one file after entity terms have been populated.
/// Test: `context_preserves_evidence_and_budget`, `context_zero_and_missing_parent`.
pub fn enrich_chunk_context(
    chunks: &mut [RawChunk],
    file_content: &str,
    config: &ExperimentConfig,
) {
    if config.context_words == 0 {
        return;
    }
    let lines: Vec<_> = file_content.lines().collect();
    let parents: HashMap<_, _> = chunks
        .iter()
        .map(|chunk| {
            (
                chunk.id.clone(),
                format!(
                    "{} {}",
                    chunk.function_name.as_deref().unwrap_or(""),
                    chunk.content.lines().take(4).collect::<Vec<_>>().join(" ")
                ),
            )
        })
        .collect();
    for chunk in chunks {
        let mut parts = Vec::new();
        // Absolute host paths are deliberately excluded from lexical context.
        if !std::path::Path::new(&chunk.file).is_absolute() {
            parts.push(chunk.file.replace(['/', '.', '_', '-'], " "));
        }
        if let Some(name) = &chunk.function_name {
            parts.push(name.clone());
        }
        let start = chunk.start_line.saturating_sub(1).min(lines.len());
        let mut doc_start = start;
        while doc_start > start.saturating_sub(64) && documentation(lines[doc_start - 1]) {
            doc_start -= 1;
        }
        parts.push(lines[doc_start..start].join(" "));
        parts.push(
            chunk
                .content
                .lines()
                .take(8)
                .take_while(|line| {
                    documentation(line) || declaration(line) || line.trim().is_empty()
                })
                .collect::<Vec<_>>()
                .join(" "),
        );
        if let Some(parent) = chunk
            .parent_chunk_id
            .as_ref()
            .and_then(|id| parents.get(id))
        {
            parts.push(parent.clone());
        }
        let mut seen: HashSet<String> = chunk
            .virtual_terms
            .iter()
            .flat_map(|term| term.split_whitespace())
            .map(str::to_owned)
            .collect();
        let words = parts
            .iter()
            .flat_map(|part| part.split_whitespace())
            .filter(|word| seen.insert((*word).to_owned()))
            .take(config.context_words)
            .map(str::to_owned)
            .collect::<Vec<_>>();
        chunk.virtual_terms.extend(words);
    }
}

#[derive(Clone, Debug, Serialize)]
pub struct ChunkLandmark {
    pub line: usize,
    pub text: String,
}

#[derive(Clone, Debug, Serialize)]
pub struct NavigationCard {
    pub index_id: String,
    pub path: String,
    pub start_line: usize,
    pub end_line: usize,
    pub symbol: Option<String>,
    pub kind: String,
    pub description: String,
    pub landmarks: Vec<ChunkLandmark>,
    pub score: f32,
    pub match_reason: String,
}

/// Why: callers can inspect source locations with a bounded response.
/// What: extracts at most 40 description words and four real source landmarks;
/// never reads files, changes ranking, or invents source boundaries.
/// Test: `cards_preserve_scores_and_unicode_locations`.
pub fn navigation_card(index_id: &str, chunk: &CodeChunk) -> NavigationCard {
    let lines: Vec<_> = chunk
        .content
        .lines()
        .enumerate()
        .filter(|(offset, line)| {
            !line.trim().is_empty() && chunk.start_line.saturating_add(*offset) <= chunk.end_line
        })
        .collect();
    let mut landmarks: Vec<_> = lines
        .iter()
        .filter(|(_, line)| declaration(line))
        .take(4)
        .map(|(offset, line)| ChunkLandmark {
            line: chunk.start_line + offset,
            text: line.trim().chars().take(120).collect(),
        })
        .collect();
    if landmarks.is_empty() {
        if let Some((offset, line)) = lines.first() {
            landmarks.push(ChunkLandmark {
                line: chunk.start_line + offset,
                text: line.trim().chars().take(120).collect(),
            });
        }
    }
    NavigationCard {
        index_id: index_id.into(),
        path: chunk.path.clone().unwrap_or_else(|| chunk.file.clone()),
        start_line: chunk.start_line,
        end_line: chunk.end_line,
        symbol: chunk.function_name.clone(),
        kind: format!("{:?}", chunk.chunk_type),
        description: chunk
            .content
            .split_whitespace()
            .take(40)
            .collect::<Vec<_>>()
            .join(" "),
        landmarks,
        score: chunk.score,
        match_reason: chunk.match_reason.clone(),
    }
}

#[cfg(test)]
#[path = "experiment_tests.rs"]
mod tests;
