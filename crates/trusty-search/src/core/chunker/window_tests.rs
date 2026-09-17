use super::*;
use crate::core::experiment::ExperimentConfig;

#[test]
fn experiment_windows_preserve_parent_and_source_mapping() {
    let source = (1..=250)
        .map(|n| format!("line_{n} é"))
        .collect::<Vec<_>>()
        .join("\n");
    let parent = RawChunk::generic(
        "large:20:269".into(),
        "large.rs".into(),
        20,
        269,
        source.clone(),
    );
    for window in [100, 64] {
        let chunks = split_oversized_with_config(
            vec![parent.clone()],
            &ExperimentConfig {
                context_words: 0,
                subchunk_window: window,
            },
        );
        let umbrella = chunks.last().unwrap();
        assert_eq!(umbrella.id, parent.id);
        assert_eq!(umbrella.content, source);
        assert_eq!(umbrella.child_chunk_ids.len(), chunks.len() - 1);
        for (ordinal, chunk) in chunks[..chunks.len() - 1].iter().enumerate() {
            assert_eq!(
                chunk.id,
                crate::core::chunk_id::make_sub(&parent.id, ordinal)
            );
            assert_eq!(chunk.parent_chunk_id.as_ref(), Some(&parent.id));
            assert!(chunk.end_line - chunk.start_line < window);
            assert_eq!(chunk.start_line, 20 + ordinal * window / 2);
            assert_eq!(
                chunk.content,
                source
                    .lines()
                    .skip(chunk.start_line - 20)
                    .take(chunk.end_line - chunk.start_line + 1)
                    .collect::<Vec<_>>()
                    .join("\n")
            );
        }
        assert_eq!(chunks[chunks.len() - 2].end_line, parent.end_line);
    }
    let small = RawChunk::generic(
        "small:1:2".into(),
        "small.rs".into(),
        1,
        2,
        "one\ntwo".into(),
    );
    for window in [100, 64] {
        let output = split_oversized_with_config(
            vec![small.clone()],
            &ExperimentConfig {
                context_words: 128,
                subchunk_window: window,
            },
        );
        assert_eq!(
            serde_json::to_value(output).unwrap(),
            serde_json::to_value(vec![small.clone()]).unwrap()
        );
    }
}

#[test]
#[should_panic(expected = "unsupported experiment window")]
fn experiment_invalid_window_is_rejected_before_chunking() {
    split_oversized_with_config(
        vec![],
        &ExperimentConfig {
            context_words: 0,
            subchunk_window: 0,
        },
    );
}
