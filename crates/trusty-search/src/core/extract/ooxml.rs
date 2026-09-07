//! Container and run-text helpers shared by the OOXML extractors (#6938).
//!
//! Why: `.docx` and `.pptx` are the same package shape — a zip of XML parts
//! whose visible text sits in run elements (`<w:t>`, `<a:t>`) — so the bounded
//! part read and the run-text decoding would otherwise exist once per format.
//! Each caller passes its own [`ExtractError`] constructor, so a failure still
//! names the format the caller was handed.
//!
//! What: [`read_entry_bounded`] reads one zip entry under a hard decompressed
//! byte cap; [`push_run_text`] and [`push_entity_ref`] append one text or
//! entity-reference event's resolved content to a paragraph buffer.
//!
//! Test: `test_oversized_document_xml_rejected_by_declared_size`,
//! `test_bounded_read_rejects_underdeclared_entry`,
//! `test_bounded_read_accepts_within_cap`; the run-text pair is exercised
//! through `docx::tests` and `pptx::tests`.

use std::io::Read;

use quick_xml::events::{BytesRef, BytesText};

use super::ExtractError;

/// Cap on the UNCOMPRESSED size of a single XML part (bytes).
///
/// Why: `MAX_OFFICE_FILE_BYTES` caps only the compressed container on disk;
/// DEFLATE ratios can reach ~1000:1, so without a decompressed-size bound a
/// crafted ~10 MiB package (a zip bomb) could expand to multi-GB in memory
/// before the post-hoc `MAX_EXTRACTED_TEXT_BYTES` truncation in `extract_text`
/// ever runs — one hostile file in a watched directory would OOM the daemon.
/// 50 MiB of XML gives ~10x markup overhead headroom over the 5 MiB
/// extracted-text cap while keeping worst-case memory bounded.
/// What: enforced twice by [`read_entry_bounded`] — the entry's declared
/// uncompressed size is rejected up front, AND the reader is wrapped in
/// `Read::take` so a lying size field cannot bypass the bound.
/// Test: `test_oversized_document_xml_rejected_by_declared_size`,
/// `test_bounded_read_rejects_underdeclared_entry`.
pub(super) const MAX_PART_BYTES: u64 = 50 * 1024 * 1024;

/// Read a zip entry to a `String`, refusing to decompress past `cap` bytes.
///
/// Why: the zip-bomb defence must hold even when the entry's central-directory
/// size field lies, so the declared-size check alone is not enough — the
/// actual decompressed byte stream is also hard-capped via `Read::take`.
/// What: rejects when the entry DECLARES (`declared`) more than `cap`
/// uncompressed bytes; otherwise reads at most `cap + 1` bytes and rejects if
/// the stream exceeds `cap` (i.e. the declared size was false). `name` is the
/// zip entry path, used only to name the offending part in the error; `wrap`
/// builds the caller's per-format error variant. Content must be valid UTF-8.
/// Generic over `Read` so the lying-size path is unit-testable without
/// crafting a malicious zip.
/// Test: `test_oversized_document_xml_rejected_by_declared_size`,
/// `test_bounded_read_rejects_underdeclared_entry`.
pub(super) fn read_entry_bounded<R: Read>(
    entry: R,
    declared: u64,
    name: &str,
    cap: u64,
    wrap: fn(String) -> ExtractError,
) -> Result<String, ExtractError> {
    if declared > cap {
        return Err(wrap(format!(
            "{name} declares {declared} uncompressed bytes, over the {cap} byte cap"
        )));
    }
    let mut bytes = Vec::with_capacity(declared as usize);
    entry
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| wrap(e.to_string()))?;
    if bytes.len() as u64 > cap {
        return Err(wrap(format!(
            "{name} decompressed past the {cap} byte cap (declared {declared})"
        )));
    }
    String::from_utf8(bytes).map_err(|e| wrap(e.to_string()))
}

/// Append one run-text event's content to `buf`.
///
/// Why: quick-xml 0.41 removed `BytesText::unescape()` (dependency bump for
/// RUSTSEC-2026-0194/0195, issue #3367), so the raw bytes are decoded first
/// and unescaped through the free function. In 0.41 a `Text` event itself
/// never carries an escaped entity (see [`push_entity_ref`]), so the unescape
/// is a no-op in practice — kept in case that reader behavior changes.
/// Test: `docx::tests::test_paragraphs_from_document_xml_basic`,
/// `pptx::tests::test_slide_paragraphs_split_on_a_p`.
pub(super) fn push_run_text(
    buf: &mut String,
    t: &BytesText<'_>,
    wrap: fn(String) -> ExtractError,
) -> Result<(), ExtractError> {
    let decoded = t.decode().map_err(|e| wrap(e.to_string()))?;
    let unescaped = quick_xml::escape::unescape(&decoded).map_err(|e| wrap(e.to_string()))?;
    buf.push_str(&unescaped);
    Ok(())
}

/// Append one entity or character reference's resolved content to `buf`.
///
/// Why: quick-xml 0.41 stopped inlining references (`&amp;`, `&#233;`) into
/// the surrounding `Text` event; each is its own `GeneralRef` event, and
/// without resolving them `Tom &amp; Jerry` extracted as `Tom  Jerry`.
/// What: a numeric character reference resolves directly; a named one resolves
/// against the predefined set. Anything else is an error rather than a silent
/// drop.
/// Test: `docx::tests::test_paragraphs_from_document_xml_unescapes_entities`,
/// `pptx::tests::test_slide_text_resolves_entity_references`.
pub(super) fn push_entity_ref(
    buf: &mut String,
    r: &BytesRef<'_>,
    wrap: fn(String) -> ExtractError,
) -> Result<(), ExtractError> {
    if let Some(c) = r.resolve_char_ref().map_err(|e| wrap(e.to_string()))? {
        buf.push(c);
        return Ok(());
    }
    let name = r.decode().map_err(|e| wrap(e.to_string()))?;
    match quick_xml::escape::resolve_predefined_entity(&name) {
        Some(resolved) => {
            buf.push_str(resolved);
            Ok(())
        }
        None => Err(wrap(format!("unresolvable XML entity reference: &{name};"))),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const PART: &str = "word/document.xml";

    #[test]
    fn test_oversized_document_xml_rejected_by_declared_size() {
        // declared size over the cap must be rejected BEFORE any decompression.
        let data = b"whatever";
        let result = read_entry_bounded(
            std::io::Cursor::new(&data[..]),
            1000,
            PART,
            100,
            ExtractError::Docx,
        );
        let err = result.expect_err("declared size over cap must error");
        assert!(err.to_string().contains("over the 100 byte cap"), "{err}");
    }

    #[test]
    fn test_bounded_read_rejects_underdeclared_entry() {
        // A lying size field (declares under the cap, actually decompresses
        // past it) must still be stopped by the Read::take hard bound.
        let data = vec![b'x'; 200];
        let result = read_entry_bounded(
            std::io::Cursor::new(data),
            50,
            PART,
            100,
            ExtractError::Docx,
        );
        let err = result.expect_err("stream past cap must error");
        assert!(err.to_string().contains("decompressed past"), "{err}");
    }

    #[test]
    fn test_bounded_read_accepts_within_cap() {
        let data = b"hello world";
        let text = read_entry_bounded(
            std::io::Cursor::new(&data[..]),
            data.len() as u64,
            PART,
            100,
            ExtractError::Docx,
        )
        .unwrap();
        assert_eq!(text, "hello world");
    }

    /// #6938: the error constructor the caller passes must be the one the
    /// failure carries, so a `.pptx` never reports itself as a docx failure.
    #[test]
    fn test_wrap_names_the_callers_format() {
        let err = read_entry_bounded(
            std::io::Cursor::new(&b"x"[..]),
            1000,
            "ppt/slides/slide1.xml",
            100,
            ExtractError::Pptx,
        )
        .expect_err("declared size over cap must error");
        assert!(matches!(err, ExtractError::Pptx(_)), "{err}");
    }
}
