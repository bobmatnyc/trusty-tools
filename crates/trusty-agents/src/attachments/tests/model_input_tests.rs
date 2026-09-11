//! What an attached turn hands the model, and reading its markers back (#7370).

use super::super::model_input::{
    MAX_INLINE_TEXT_BYTES, augment_user_turn, marker_for, parse_markers, render,
};
use super::fixture;

const SESSION: &str = "persona-izzie";

#[test]
fn markers_round_trip() {
    let id = "c".repeat(32);
    let other = "d".repeat(32);
    let content = format!(
        "look at these\n\n{} then {}\nand {} again",
        marker_for(&id),
        marker_for(&other),
        marker_for(&id)
    );
    assert_eq!(parse_markers(&content), vec![id, other]);
}

#[test]
fn parse_ignores_malformed_markers() {
    assert!(parse_markers("plain prose, no markers").is_empty());
    assert!(parse_markers("[[attachment:not-an-id]]").is_empty());
    assert!(parse_markers("[[attachment:unclosed").is_empty());
    assert!(parse_markers(&format!("[[attachment:{}]]", "Z".repeat(32))).is_empty());
}

#[test]
fn text_is_inlined_under_a_fence() {
    let (_temp, store) = fixture();
    let row = store
        .store(SESSION, "data.csv", None, b"a,b\n1,2\n")
        .unwrap();
    let block = render(&row, b"a,b\n1,2\n");

    assert!(block.starts_with(&marker_for(&row.id)), "{block}");
    assert!(block.contains("data.csv (text/csv, 8 bytes)"), "{block}");
    assert!(block.contains("```csv\na,b\n1,2\n\n```"), "{block}");
    assert_eq!(parse_markers(&block), vec![row.id]);
}

#[test]
fn binary_is_one_reference_line() {
    let (_temp, store) = fixture();
    let row = store
        .store(SESSION, "shot.png", None, b"\x89PNG\r\n\x1a\n")
        .unwrap();
    let block = render(&row, b"\x89PNG\r\n\x1a\n");

    assert_eq!(block.lines().count(), 1, "{block}");
    assert!(block.contains("shot.png (image/png, 8 bytes)"), "{block}");
    assert!(block.contains("binary attachment"), "{block}");
    assert!(!block.contains("```"), "{block}");
    assert_eq!(parse_markers(&block), vec![row.id]);
}

#[test]
fn text_is_capped_with_a_truncation_note() {
    let (_temp, store) = fixture();
    let big = "x".repeat(MAX_INLINE_TEXT_BYTES + 500);
    let row = store
        .store(SESSION, "big.txt", None, big.as_bytes())
        .unwrap();
    let block = render(&row, big.as_bytes());

    assert!(block.contains("[truncated: the first"), "note missing");
    assert!(
        block.len() < big.len(),
        "the cap did not shorten the rendered block"
    );
    assert!(
        block.contains(&"x".repeat(MAX_INLINE_TEXT_BYTES)),
        "the cap cut too much"
    );
    assert!(
        !block.contains(&"x".repeat(MAX_INLINE_TEXT_BYTES + 1)),
        "the cap let more than {MAX_INLINE_TEXT_BYTES} bytes through"
    );
}

#[test]
fn no_attachments_leaves_the_turn_untouched() {
    assert_eq!(augment_user_turn("what is this?", &[]), "what is this?");
}

#[test]
fn blocks_follow_the_users_text() {
    let joined = augment_user_turn("summarise", &["ONE".to_string(), "TWO".to_string()]);
    assert_eq!(joined, "summarise\n\nONE\n\nTWO");
    assert_eq!(augment_user_turn("  ", &["ONE".to_string()]), "ONE");
}
