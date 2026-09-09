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

/// A line carrying no `usage` key, padded to `bytes`, used as the filler the
/// tail window is made to start inside.
fn filler_line(bytes: usize) -> String {
    let prefix = r#"{"type":"user","pad":""#;
    let suffix = r#""}"#;
    let pad = bytes.saturating_sub(prefix.len() + suffix.len());
    format!("{prefix}{}{suffix}", "a".repeat(pad))
}

/// Why (#7074 round-2 review): `git` blocks on the `prepare-commit-msg` hook
/// that reaches this fold, so an uncapped read of a hundreds-of-megabytes
/// transcript slows every commit for the rest of the session. The fold must
/// read the tail window and nothing before it — proven here by putting a
/// million output tokens ahead of the window and requiring them absent from
/// the total.
/// Test: itself.
#[test]
fn fold_reads_only_the_tail_of_an_oversized_transcript() {
    let dir = tempfile::tempdir().expect("temp dir");
    let tail = [
        assistant_line("t1", 10, 0, 5),
        assistant_line("t2", 20, 0, 7),
    ];
    let tail_bytes: u64 = tail.iter().map(|line| line.len() as u64 + 1).sum();

    let path = write_transcript(
        dir.path(),
        &[
            assistant_line("head", 1_000_000, 0, 1_000_000),
            filler_line(4096),
            tail[0].clone(),
            tail[1].clone(),
        ],
    );

    // One byte more than the tail puts the window's start on the filler's
    // trailing newline, so the resync discards exactly that and nothing else.
    let usage = fold_transcript_tail(&path, tail_bytes + 1);
    assert_eq!(usage.messages, 2, "only the two lines inside the window");
    assert_eq!(
        usage.tokens_in, 30,
        "the head's million is outside the window"
    );
    assert_eq!(usage.tokens_out, 12);
    assert!(usage.truncated, "the cap stopped the fold short");
}

/// Why: the parameterised cap above proves the mechanism; this proves the
/// public entry point actually applies `TRANSCRIPT_TAIL_BYTES`, against a
/// fixture genuinely larger than it. A regression that raised or dropped the
/// cap would leave the head's million tokens in the total.
/// Test: itself.
#[test]
fn fold_transcript_bounds_a_transcript_larger_than_the_cap() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write_transcript(
        dir.path(),
        &[
            assistant_line("head", 1_000_000, 0, 1_000_000),
            filler_line(TRANSCRIPT_TAIL_BYTES as usize),
            assistant_line("t1", 10, 0, 5),
            assistant_line("t2", 20, 0, 7),
        ],
    );

    let usage = fold_transcript(&path);
    assert_eq!(usage.messages, 2);
    assert_eq!(usage.tokens_in, 30);
    assert_eq!(usage.tokens_out, 12);
    assert!(usage.truncated);
}

/// Why: the ordinary session is far below the cap, and its footer must claim
/// whole-session totals rather than a window — so `truncated` has to stay false
/// for every transcript the cap does not actually cut.
/// Test: itself.
#[test]
fn fold_of_a_transcript_within_the_cap_is_not_truncated() {
    let dir = tempfile::tempdir().expect("temp dir");
    let path = write_transcript(dir.path(), &[assistant_line("m1", 10, 0, 5)]);

    let usage = fold_transcript(&path);
    assert!(!usage.truncated);
    assert_eq!(usage.messages, 1);
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
