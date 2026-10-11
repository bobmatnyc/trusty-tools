//! Flag commit-SHA-like tokens in PM free text before it reaches a snapshot (#9390).
//!
//! Why: two of 13 relaunches on 2026-10-07 resumed from pause snapshots that
//! carried wrong PR head SHAs. Every one was typed by the PM into the free-text
//! sections; a head moves within minutes, so a recorded head is a claim the
//! next session must not trust. The snapshot keeps identifiers (branch, PR
//! number) and the resume re-reads the head.
//! What: [`scrub_sha_like`] replaces each SHA-like word with [`SHA_PLACEHOLDER`].
//! A word is a maximal run of ASCII letters, digits, `_` and `-`; it is SHA-like
//! when it is 7-40 characters, all hex digits, and holds at least one digit and
//! at least one letter. So `d3963848ab` and both ends of `abc1234..def5678` are
//! flagged, while `added`, `facade`, `deadbeef` (no digit), `20261010` (no
//! letter), `xdeadbeef1x` (not all hex), UUIDs and `feat/fix-1a2b3c4d` (the
//! hyphen joins them into one non-hex word) are left alone.
//! Test: `a_snapshot_contains_no_head_sha` (catchup::pause),
//! `the_token_rule_flags_shas_and_keeps_words_dates_and_ids`.

use std::borrow::Cow;

/// What a flagged token is replaced with.
pub const SHA_PLACEHOLDER: &str = "<sha?>";

/// Shortest token treated as a SHA: git's default abbreviation.
const MIN_SHA_LEN: usize = 7;

/// Longest token treated as a SHA: a full SHA-1 object id.
const MAX_SHA_LEN: usize = 40;

/// Is `c` part of a word for the token rule?
fn is_word_byte(c: u8) -> bool {
    c.is_ascii_alphanumeric() || c == b'_' || c == b'-'
}

/// Is `word` (one whole word) SHA-like under the module's rule?
pub fn is_sha_like(word: &str) -> bool {
    let bytes = word.as_bytes();
    (MIN_SHA_LEN..=MAX_SHA_LEN).contains(&bytes.len())
        && bytes.iter().all(u8::is_ascii_hexdigit)
        && bytes.iter().any(u8::is_ascii_digit)
        && bytes.iter().any(u8::is_ascii_alphabetic)
}

/// `text` with every SHA-like word replaced by [`SHA_PLACEHOLDER`].
///
/// Why/What: see the module doc. Borrowed when nothing matched, so text with
/// no SHA is passed through unchanged.
/// Test: `the_token_rule_flags_shas_and_keeps_words_dates_and_ids`.
pub fn scrub_sha_like(text: &str) -> Cow<'_, str> {
    let bytes = text.as_bytes();
    let mut out: Option<String> = None;
    let mut copied = 0;
    let mut i = 0;
    while i < bytes.len() {
        if !is_word_byte(bytes[i]) {
            i += 1;
            continue;
        }
        let start = i;
        while i < bytes.len() && is_word_byte(bytes[i]) {
            i += 1;
        }
        // Word bytes are ASCII, so `start..i` is a char boundary on both ends.
        if is_sha_like(&text[start..i]) {
            let buf = out.get_or_insert_with(|| String::with_capacity(text.len()));
            buf.push_str(&text[copied..start]);
            buf.push_str(SHA_PLACEHOLDER);
            copied = i;
        }
    }
    match out {
        Some(mut buf) => {
            buf.push_str(&text[copied..]);
            Cow::Owned(buf)
        }
        None => Cow::Borrowed(text),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_token_rule_flags_shas_and_keeps_words_dates_and_ids() {
        let cases = [
            ("PR #12 head d3963848ab.", "PR #12 head <sha?>."),
            ("compare abc1234..def5678", "compare <sha?>..<sha?>"),
            ("`0123456789abcdef0123456789abcdef01234567`", "`<sha?>`"),
            ("see commit/ABC1234F", "see commit/<sha?>"),
            ("added the facade, deadbeef", "added the facade, deadbeef"),
            ("xdeadbeef1x and 0x1a2b3c4d5", "xdeadbeef1x and 0x1a2b3c4d5"),
            ("2026-10-10 21:46 20261010", "2026-10-10 21:46 20261010"),
            ("feat/9390-x #9390 feat/fix-1a2b3c4d", "feat/9390-x #9390 feat/fix-1a2b3c4d"),
            (
                "session 7bd5c27a-475b-41df-9e9f-a6f630801717",
                "session 7bd5c27a-475b-41df-9e9f-a6f630801717",
            ),
            ("abc123 is six", "abc123 is six"),
            ("é d3963848ab é", "é <sha?> é"),
        ];
        for (input, want) in cases {
            assert_eq!(scrub_sha_like(input), want, "input {input:?}");
        }
        // 41 hex characters is longer than any SHA-1 id.
        let long = "a1".repeat(20) + "a";
        assert_eq!(scrub_sha_like(&long), long);
        assert!(matches!(scrub_sha_like("no ids here"), Cow::Borrowed(_)));
    }
}
