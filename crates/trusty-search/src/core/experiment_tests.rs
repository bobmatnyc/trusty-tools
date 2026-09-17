use super::*;
use crate::core::chunker::chunk_ast;

#[test]
fn configuration_grid_and_rejections() {
    assert_eq!(
        parse_experiment_config(None, None).unwrap(),
        ExperimentConfig {
            context_words: 0,
            subchunk_window: 100,
        }
    );
    for words in ["0", "64", "128"] {
        for window in ["64", "100"] {
            assert!(parse_experiment_config(Some(words), Some(window)).is_ok());
        }
    }
    for invalid in ["", "-1", "256", " 64", "banana"] {
        assert!(parse_experiment_config(Some(invalid), None).is_err());
        assert!(parse_experiment_config(None, Some(invalid)).is_err());
    }
}

#[test]
fn context_preserves_evidence_and_budget() {
    let docs = (0..200)
        .map(|n| format!("word{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let source = format!("/// {docs}\r\npub fn café() {{}}\r\n");
    let (original, _) = chunk_ast("src/session_refresh.rs", &source);
    for budget in [64, 128] {
        let mut chunks = original.clone();
        for chunk in &mut chunks {
            chunk.virtual_terms.push("existing".into());
        }
        enrich_chunk_context(
            &mut chunks,
            &source,
            &ExperimentConfig {
                context_words: budget,
                subchunk_window: 100,
            },
        );
        for (before, after) in original.iter().zip(&chunks) {
            assert_eq!(after.virtual_terms[0], "existing");
            assert!(after.virtual_terms.len() <= budget + 1);
            assert_eq!(
                after.virtual_terms.iter().collect::<HashSet<_>>().len(),
                after.virtual_terms.len()
            );
            let mut restored = after.clone();
            restored.virtual_terms.clear();
            assert_eq!(
                serde_json::to_value(before).unwrap(),
                serde_json::to_value(restored).unwrap()
            );
        }
        let mut repeated = original.clone();
        for chunk in &mut repeated {
            chunk.virtual_terms.push("existing".into());
        }
        enrich_chunk_context(
            &mut repeated,
            &source,
            &ExperimentConfig {
                context_words: budget,
                subchunk_window: 100,
            },
        );
        assert_eq!(
            serde_json::to_value(chunks).unwrap(),
            serde_json::to_value(repeated).unwrap()
        );
    }
}

#[test]
fn context_zero_and_missing_parent() {
    for (file, source) in [
        ("empty.rs", ""),
        ("readme.md", "# Café\nText"),
        ("unknown.xyz", "opaque\ncontent"),
        ("multiline.rs", "pub fn run(\n arg: i32\n) {}\n"),
    ] {
        let (mut chunks, _) = chunk_ast(file, source);
        let before = serde_json::to_value(&chunks).unwrap();
        enrich_chunk_context(
            &mut chunks,
            source,
            &ExperimentConfig {
                context_words: 0,
                subchunk_window: 100,
            },
        );
        assert_eq!(before, serde_json::to_value(&chunks).unwrap());
        for chunk in &mut chunks {
            chunk.parent_chunk_id = Some("missing".into());
        }
        enrich_chunk_context(
            &mut chunks,
            source,
            &ExperimentConfig {
                context_words: 64,
                subchunk_window: 100,
            },
        );
    }
}

#[test]
fn cards_preserve_scores_and_unicode_locations() {
    let mut chunk: CodeChunk = serde_json::from_value(serde_json::json!({
        "id":"source:10:14", "file":"/copy/source.rs", "path":"source.rs",
        "start_line":10,"end_line":14,"content":format!("\nfn café() {{\n{}\n}}", "é".repeat(200)),
        "function_name":"café","score":0.75,"compact_snippet":null,"match_reason":"bm25"
    }))
    .unwrap();
    let card = navigation_card("experiment", &chunk);
    assert_eq!(card.score, chunk.score);
    assert_eq!(card.path, "source.rs");
    assert_eq!(card.landmarks[0].line, 11);
    for landmark in &card.landmarks {
        assert!(landmark.text.chars().count() <= 120);
        assert!(chunk
            .content
            .lines()
            .nth(landmark.line - chunk.start_line)
            .unwrap()
            .trim()
            .starts_with(&landmark.text));
    }
    chunk.content = "é".repeat(200);
    assert_eq!(
        navigation_card("experiment", &chunk).landmarks[0]
            .text
            .chars()
            .count(),
        120
    );
    chunk.content.clear();
    let empty = navigation_card("experiment", &chunk);
    assert!(empty.description.is_empty() && empty.landmarks.is_empty());
}
