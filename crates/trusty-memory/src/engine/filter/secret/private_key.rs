//! PEM private-key block detection for the secret screen (#8623).
//!
//! Why: a PEM private key is the one credential that spans several lines, so
//! the whitespace tokenizer sees its armor and its body as unrelated tokens.
//! The body lines usually trip the base64 rule, but the armor alone, a short
//! body or a truncated paste does not. A key block is a credential as a whole.
//! What: [`private_key_blocks`] returns the byte range of every
//! `-----BEGIN …PRIVATE KEY` region, widened to whole delimiter-separated
//! chunks so that each token of the content lies wholly inside or wholly
//! outside a block.
//! Test: `private_key_block_is_one_span`, `unterminated_private_key_is_flagged`,
//! `private_key_near_misses_are_not_blocks`.

use std::ops::Range;

/// The armor line that opens a PEM block.
pub(crate) const PEM_BEGIN: &str = "-----BEGIN ";
/// The armor line that closes a PEM block.
pub(crate) const PEM_END: &str = "-----END ";
/// The label text that makes a PEM block a private key.
pub(crate) const PEM_PRIVATE_KEY: &str = "PRIVATE KEY";

/// True for a byte the tokenizer splits on: the same delimiters
/// `candidate_tokens` uses, so a block edge never cuts a token.
fn is_delimiter(c: char) -> bool {
    c.is_whitespace() || c == '`'
}

/// Length of the armor label at the start of `rest`: the leading run of
/// ASCII uppercase letters, digits and spaces (`RSA PRIVATE KEY`).
fn label_len(rest: &str) -> usize {
    rest.bytes()
        .take_while(|b| b.is_ascii_uppercase() || b.is_ascii_digit() || *b == b' ')
        .count()
}

/// Byte offset just past the `-----END …PRIVATE KEY-----` line that closes a
/// block opened before `from`, or `None` when no such line follows.
///
/// What: an END line whose label is not a private key does not close the
/// block; the search continues past it, so a mismatched END never shortens
/// the flagged region.
fn closing_end(content: &str, from: usize) -> Option<usize> {
    let mut at = from;
    while let Some(rel) = content[at..].find(PEM_END) {
        let label_start = at + rel + PEM_END.len();
        let label_end = label_start + label_len(&content[label_start..]);
        if content[label_start..label_end].contains(PEM_PRIVATE_KEY) {
            let dashes = content[label_end..]
                .bytes()
                .take_while(|b| *b == b'-')
                .count();
            return Some(label_end + dashes);
        }
        at = label_start;
    }
    None
}

/// Widen `start..end` outward to the delimiter-separated chunks they fall in.
///
/// Why: a block edge inside a chunk (`key=-----BEGIN`) would split a token in
/// two. Widening keeps every token wholly inside or outside the block, which
/// is what lets a caller drop the tokens a block covers without losing text.
fn widen_to_chunks(content: &str, start: usize, end: usize) -> Range<usize> {
    let start = content[..start]
        .char_indices()
        .rev()
        .find(|&(_, c)| is_delimiter(c))
        .map_or(0, |(i, c)| i + c.len_utf8());
    let end = content[end..]
        .char_indices()
        .find(|&(_, c)| is_delimiter(c))
        .map_or(content.len(), |(i, _)| end + i);
    start..end
}

/// Byte ranges of the PEM private-key blocks in `content`, ascending and
/// disjoint.
///
/// Why (#8623): see the module doc. An unterminated block runs to the end of
/// `content` — a truncated key is still a key, so it is flagged, not skipped.
/// What: finds each `-----BEGIN ` whose label contains `PRIVATE KEY` (the
/// trailing dashes are not required), extends it to the first following
/// `-----END …PRIVATE KEY` line or to the end of `content`, then widens the
/// range to whole chunks. A `BEGIN` for another label (`PUBLIC KEY`,
/// `CERTIFICATE`) is not a block. Every offset is a char boundary, because
/// each one sits on an ASCII byte, a delimiter or an end of `content`.
/// Test: `private_key_block_is_one_span`, `unterminated_private_key_is_flagged`,
/// `private_key_near_misses_are_not_blocks`.
pub(crate) fn private_key_blocks(content: &str) -> Vec<Range<usize>> {
    let mut blocks: Vec<Range<usize>> = Vec::new();
    let mut from = 0;
    while let Some(rel) = content[from..].find(PEM_BEGIN) {
        let begin = from + rel;
        let label_start = begin + PEM_BEGIN.len();
        let label_end = label_start + label_len(&content[label_start..]);
        if !content[label_start..label_end].contains(PEM_PRIVATE_KEY) {
            from = label_start;
            continue;
        }
        let end = closing_end(content, label_end).unwrap_or(content.len());
        let block = widen_to_chunks(content, begin, end);
        // #8623: each search resumes at the previous block's end, which is a
        // delimiter or the end of `content`, so blocks never overlap.
        debug_assert!(blocks.last().is_none_or(|b| b.end <= block.start));
        from = block.end;
        blocks.push(block);
    }
    blocks
}
