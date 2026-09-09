//! Unit tests for [`super`] — the transcript tokens-in/out fold (#7074).

use super::*;

/// One transcript line for an assistant message, in Claude Code's shape.
fn assistant_line(id: &str, input: u64, cache_read: u64, output: u64) -> String {
    format!(
        r#"{{"type":"assistant","message":{{"id":"{id}","role":"assistant","usage":{{"input_tokens":{input},"cache_creation_input_tokens":0,"cache_read_input_tokens":{cache_read},"output_tokens":{output}}}}}}}"#
    )
}

fn write_transcript(dir: &std::path::Path, lines: &[String]) -> std::path::PathBuf {
    let path = dir.join("session.jsonl");
    std::fs::write(&path, format!("{}\n", lines.join("\n"))).expect("write transcript");
    path
}

/// Why (#7074): Claude Code writes one line per content block and repeats the
/// turn's identical `usage` on each, so a fold that sums lines triple-counts a
/// three-block turn. This is the property the dedup exists for.
/// Test: itself.
#[test]
fn fold_sums_one_message_once_per_id() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write_transcript(
        dir.path(),
        &[
            r#"{"type":"user","message":{"role":"user","content":"hi"}}"#.to_string(),
            assistant_line("msg_a", 2, 100, 50),
            assistant_line("msg_a", 2, 100, 50),
            assistant_line("msg_a", 2, 100, 50),
            assistant_line("msg_b", 4, 200, 70),
        ],
    );

    let total = fold_transcript(&path);
    assert_eq!(total.messages, 2, "two distinct message ids");
    assert_eq!(total.tokens_out, 120, "50 + 70, each counted once");
    assert_eq!(total.tokens_in, 306, "(2+100) + (4+200)");
}

/// Why: `input_tokens` alone excludes the cached prefix, which is most of a
/// long session's input — a footer built from it would understate by an order
/// of magnitude.
/// Test: itself.
#[test]
fn fold_counts_cache_tokens_as_tokens_in() {
    let dir = tempfile::tempdir().expect("temp dir");
    let line = r#"{"type":"assistant","message":{"id":"m1","usage":{"input_tokens":2,"cache_creation_input_tokens":74685,"cache_read_input_tokens":27297,"output_tokens":288}}}"#;
    let path = write_transcript(dir.path(), &[line.to_string()]);

    let total = fold_transcript(&path);
    assert_eq!(total.tokens_in, 101_984);
    assert_eq!(total.tokens_out, 288);
}

/// Why: a session with no transcript on disk is the ordinary state before the
/// first assistant turn; it must fold to empty, never to a fabricated zero-ish
/// reading a caller would then render.
/// Test: itself.
#[test]
fn fold_of_a_missing_transcript_is_empty() {
    let dir = tempfile::tempdir().expect("temp dir");
    let total = fold_transcript(&dir.path().join("nothing-here.jsonl"));
    assert_eq!(total, TranscriptUsage::default());
    assert!(total.is_empty());
}

/// Why (#7074, fail-open check): a crash mid-write leaves one truncated line.
/// It must cost that one line and nothing else — the surrounding valid rows
/// still count.
/// Test: itself.
#[test]
fn fold_skips_a_malformed_line_and_keeps_the_valid_total() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write_transcript(
        dir.path(),
        &[
            assistant_line("m1", 10, 0, 5),
            r#"{"type":"assistant","message":{"usage":{"output_to"#.to_string(),
            assistant_line("m2", 20, 0, 7),
        ],
    );

    let total = fold_transcript(&path);
    assert_eq!(total.messages, 2);
    assert_eq!(total.tokens_in, 30);
    assert_eq!(total.tokens_out, 12);
}
