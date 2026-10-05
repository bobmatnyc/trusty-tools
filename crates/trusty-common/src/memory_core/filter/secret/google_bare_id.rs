//! Bare Google document ids that the surrounding prose names as such.
//!
//! Why (issue #8589): the URL form of a Google Docs/Sheets/Drive id is exempt
//! by host and position (`is_google_doc_id`), but the closure also asks for
//! the bare id to store. A bare 44-character id is character-for-character a
//! mixed-case credential, so no shape test alone can admit it. Two signals
//! together can: the exact length-and-leading-character signature Google ids
//! carry, and a word in the same content that names a Google document.
//! What: [`is_bare_google_doc_id`] is the shape test and
//! [`names_google_document`] the context test. `find_secret_token` consults
//! both, only for a token that would otherwise be refused.
//! Test: `bare_google_doc_ids_named_by_context_after_8589` in
//! `filter_tests.rs`.

use super::is_provider_key;

/// Words that name a Google document in prose, matched whole and
/// case-insensitively, also with an `id`/`ids` suffix (`spreadsheetId`).
///
/// Why (issue #8589): the context half of the bare-id exemption. `file` is
/// absent on purpose: it names any file, so it would make the shape test the
/// only guard.
/// What: lowercase words compared by [`names_google_document`].
/// Test: `bare_google_doc_ids_named_by_context_after_8589`.
pub(crate) const GOOGLE_DOC_CONTEXT_WORDS: &[&str] = &[
    "google",
    "gdoc",
    "gsheet",
    "sheet",
    "sheets",
    "spreadsheet",
    "spreadsheets",
    "doc",
    "docs",
    "document",
    "drive",
    "slides",
    "presentation",
    "folder",
];

/// True when `content` carries a [`GOOGLE_DOC_CONTEXT_WORDS`] word.
///
/// Why (issue #8589): a bare id is admitted only when the writer said what it
/// is, so a credential of the same shape written without that context stays
/// refused.
/// What: splits on every non-alphabetic byte, lowercases each word, and
/// matches it whole or with a trailing `id`/`ids` removed.
/// Test: `bare_google_doc_ids_named_by_context_after_8589`.
pub(crate) fn names_google_document(content: &str) -> bool {
    content
        .split(|c: char| !c.is_ascii_alphabetic())
        .filter(|w| !w.is_empty())
        .any(|w| {
            let w = w.to_ascii_lowercase();
            let stem = w
                .strip_suffix("ids")
                .or_else(|| w.strip_suffix("id"))
                .unwrap_or(&w);
            GOOGLE_DOC_CONTEXT_WORDS.contains(&w.as_str())
                || GOOGLE_DOC_CONTEXT_WORDS.contains(&stem)
        })
}

/// True when `tok` has the exact shape of a bare Google document id.
///
/// Why (issue #8589): the shape half of the bare-id exemption, kept to the
/// ids Google issues at or above the detector's 20-character floor: a 44-char
/// Docs/Sheets/Slides id and a 33-char Drive id both open with `1`, and a
/// 28-char legacy Drive id opens with `0B` (see `GOOGLE_DOC_ID_LENS`). The
/// leading-character rule removes about 63 of 64 same-length base64url
/// credentials before the context test is asked.
/// What: exact length with its leading signature, a base64url charset, and no
/// provider-key prefix or AWS key-id shape.
/// Test: `bare_google_doc_ids_named_by_context_after_8589`.
pub(crate) fn is_bare_google_doc_id(tok: &str) -> bool {
    let signed = match tok.len() {
        44 | 33 => tok.starts_with('1'),
        28 => tok.starts_with("0B"),
        _ => false,
    };
    signed
        && tok
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_'))
        && !is_provider_key(tok)
}
