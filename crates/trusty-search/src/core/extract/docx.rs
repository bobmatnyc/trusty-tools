//! DOCX (Word) text extraction (issue #2923).
//!
//! Why: `.docx` is a zip container around a handful of OOXML parts. Rather
//! than pull in a full-featured docx crate for a single-purpose need
//! (paragraph text extraction), this module owns a small streaming
//! `quick-xml` parse of `word/document.xml`, matching this workspace's
//! own-the-parsing convention (see the tree-sitter chunkers under
//! `core::chunker`).
//!
//! What: [`extract`] opens the zip, locates `word/document.xml`, and streams
//! it collecting `<w:t>` run-text content, emitting a paragraph break at each
//! `</w:p>`. Headers/footers (separate zip parts, e.g. `word/header1.xml`)
//! are intentionally out of scope for v1 — the body is the primary searchable
//! content.
//!
//! Test: `test_extracts_paragraphs_preserving_breaks`,
//! `test_missing_document_xml_errors`, `test_not_a_zip_errors`.

use std::io::Read;
use std::path::Path;

use quick_xml::events::Event;
use quick_xml::Reader;

use super::{ExtractError, Extracted};

/// The zip entry holding the document body, per the OOXML WordprocessingML
/// package convention.
const DOCUMENT_XML_PATH: &str = "word/document.xml";

/// Cap on the UNCOMPRESSED size of `word/document.xml` (bytes).
///
/// Why: `MAX_OFFICE_FILE_BYTES` caps only the compressed container on disk;
/// DEFLATE ratios can reach ~1000:1, so without a decompressed-size bound a
/// crafted ~10 MiB `.docx` (a zip bomb) could expand to multi-GB in memory
/// before the post-hoc `MAX_EXTRACTED_TEXT_BYTES` truncation in
/// `extract_text` ever runs — one hostile file in a watched directory would
/// OOM the daemon. 50 MiB of XML gives ~10x markup overhead headroom over
/// the 5 MiB extracted-text cap while keeping worst-case memory bounded.
/// What: enforced twice in [`extract`] — the zip entry's declared
/// uncompressed size is rejected up front, AND the reader is wrapped in
/// `Read::take` so a lying size field cannot bypass the bound.
/// Test: `test_oversized_document_xml_rejected_by_declared_size`,
/// `test_bounded_read_rejects_underdeclared_entry`.
const MAX_DOCUMENT_XML_BYTES: u64 = 50 * 1024 * 1024;

/// Extract paragraph text from a `.docx` file.
///
/// Why/What: see module docs. Decompression is bounded by
/// [`MAX_DOCUMENT_XML_BYTES`] (zip-bomb defence; see that constant's docs).
/// Test: `test_extracts_paragraphs_preserving_breaks`,
/// `test_oversized_document_xml_rejected_by_declared_size`.
pub fn extract(path: &Path) -> Result<Extracted, ExtractError> {
    let file = std::fs::File::open(path).map_err(|source| ExtractError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| ExtractError::Docx(e.to_string()))?;

    let entry = archive
        .by_name(DOCUMENT_XML_PATH)
        .map_err(|e| ExtractError::Docx(format!("{DOCUMENT_XML_PATH}: {e}")))?;
    let declared = entry.size();
    let xml = read_entry_bounded(entry, declared, MAX_DOCUMENT_XML_BYTES)?;

    Ok(Extracted {
        text: paragraphs_from_document_xml(&xml)?,
        warning: None,
    })
}

/// Read a zip entry to a `String`, refusing to decompress past `cap` bytes.
///
/// Why: the zip-bomb defence must hold even when the entry's central-directory
/// size field lies, so the declared-size check alone is not enough — the
/// actual decompressed byte stream is also hard-capped via `Read::take`.
/// What: rejects when the entry DECLARES (`declared`) more than `cap`
/// uncompressed bytes; otherwise reads at most `cap + 1` bytes and rejects if
/// the stream exceeds `cap` (i.e. the declared size was false). Content must
/// be valid UTF-8. Generic over `Read` so the lying-size path is unit-testable
/// without crafting a malicious zip.
/// Test: `test_oversized_document_xml_rejected_by_declared_size`,
/// `test_bounded_read_rejects_underdeclared_entry`.
fn read_entry_bounded<R: Read>(entry: R, declared: u64, cap: u64) -> Result<String, ExtractError> {
    if declared > cap {
        return Err(ExtractError::Docx(format!(
            "{DOCUMENT_XML_PATH} declares {declared} uncompressed bytes, over the {cap} byte cap"
        )));
    }
    let mut bytes = Vec::with_capacity(declared as usize);
    entry
        .take(cap + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| ExtractError::Docx(e.to_string()))?;
    if bytes.len() as u64 > cap {
        return Err(ExtractError::Docx(format!(
            "{DOCUMENT_XML_PATH} decompressed past the {cap} byte cap (declared {declared})"
        )));
    }
    String::from_utf8(bytes).map_err(|e| ExtractError::Docx(e.to_string()))
}

/// Parse `word/document.xml` content into paragraph text, separated by a
/// blank line between paragraphs (matching the plaintext/document chunker's
/// paragraph-break convention).
///
/// Why: isolated from `extract` so it can be unit tested against literal XML
/// fixtures without a real zip container.
/// What: streams `<w:t>` run text, appending it to the current paragraph
/// buffer; on `</w:p>` the buffer is flushed with a trailing blank line.
/// Test: `test_paragraphs_from_document_xml_basic`,
/// `test_paragraphs_from_document_xml_multiple_runs_per_paragraph`.
fn paragraphs_from_document_xml(xml: &str) -> Result<String, ExtractError> {
    let mut reader = Reader::from_str(xml);
    reader.config_mut().trim_text(false);
    let mut out = String::new();
    let mut para = String::new();
    let mut in_text = false;

    loop {
        match reader.read_event() {
            Ok(Event::Start(e)) if e.local_name().as_ref() == b"t" => in_text = true,
            Ok(Event::End(e)) if e.local_name().as_ref() == b"t" => in_text = false,
            Ok(Event::Text(t)) if in_text => {
                // quick-xml 0.41 removed `BytesText::unescape()` (dependency
                // bump for RUSTSEC-2026-0194/0195, issue #3367): decode the
                // raw bytes first, then unescape XML entities via the
                // free-function equivalent.
                let decoded = t.decode().map_err(|e| ExtractError::Docx(e.to_string()))?;
                let unescaped = quick_xml::escape::unescape(&decoded)
                    .map_err(|e| ExtractError::Docx(e.to_string()))?;
                para.push_str(&unescaped);
            }
            Ok(Event::End(e)) if e.local_name().as_ref() == b"p" => {
                out.push_str(para.trim_end());
                out.push_str("\n\n");
                para.clear();
            }
            Ok(Event::Eof) => break,
            Err(e) => return Err(ExtractError::Docx(e.to_string())),
            _ => {}
        }
    }
    // A trailing run outside any `</w:p>` (malformed/truncated input) is
    // still surfaced rather than silently dropped.
    if !para.trim().is_empty() {
        out.push_str(para.trim_end());
        out.push('\n');
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    const NS: &str = r#"xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main""#;

    /// Zip up a minimal but structurally valid `.docx` containing the given
    /// `word/document.xml` body XML (the `<w:body>...</w:body>` inner
    /// content only — this helper wraps it in the document envelope).
    fn build_minimal_docx(body_xml: &str) -> Vec<u8> {
        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document {NS}><w:body>{body_xml}</w:body></w:document>"#
        );
        let content_types = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Types xmlns="http://schemas.openxmlformats.org/package/2006/content-types"><Default Extension="xml" ContentType="application/xml"/><Override PartName="/word/document.xml" ContentType="application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml"/></Types>"#;
        let rels = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><Relationships xmlns="http://schemas.openxmlformats.org/package/2006/relationships"><Relationship Id="rId1" Type="http://schemas.openxmlformats.org/officeDocument/2006/relationships/officeDocument" Target="word/document.xml"/></Relationships>"#;

        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("[Content_Types].xml", opts).unwrap();
            zip.write_all(content_types.as_bytes()).unwrap();
            zip.start_file("_rels/.rels", opts).unwrap();
            zip.write_all(rels.as_bytes()).unwrap();
            zip.start_file(DOCUMENT_XML_PATH, opts).unwrap();
            zip.write_all(document_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        buf
    }

    #[test]
    fn test_extracts_paragraphs_preserving_breaks() {
        let body = "<w:p><w:r><w:t>Hello world.</w:t></w:r></w:p>\
                     <w:p><w:r><w:t>Second paragraph.</w:t></w:r></w:p>";
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("doc.docx");
        std::fs::write(&path, build_minimal_docx(body)).unwrap();

        let extracted = extract(&path).expect("extraction must succeed");
        assert!(extracted.text.contains("Hello world."));
        assert!(extracted.text.contains("Second paragraph."));
        // Paragraph break: the two paragraphs are separated by a blank line.
        assert!(extracted.text.contains("Hello world.\n\nSecond paragraph."));
        assert!(extracted.warning.is_none());
    }

    #[test]
    fn test_paragraphs_from_document_xml_basic() {
        let xml = format!(
            r#"<w:document {NS}><w:body><w:p><w:r><w:t>Just one paragraph.</w:t></w:r></w:p></w:body></w:document>"#
        );
        let text = paragraphs_from_document_xml(&xml).unwrap();
        assert_eq!(text.trim(), "Just one paragraph.");
    }

    #[test]
    fn test_oversized_document_xml_rejected_by_declared_size() {
        // declared size over the cap must be rejected BEFORE any decompression.
        let data = b"whatever";
        let result = read_entry_bounded(std::io::Cursor::new(&data[..]), 1000, 100);
        let err = result.expect_err("declared size over cap must error");
        assert!(err.to_string().contains("over the 100 byte cap"), "{err}");
    }

    #[test]
    fn test_bounded_read_rejects_underdeclared_entry() {
        // A lying size field (declares under the cap, actually decompresses
        // past it) must still be stopped by the Read::take hard bound.
        let data = vec![b'x'; 200];
        let result = read_entry_bounded(std::io::Cursor::new(data), 50, 100);
        let err = result.expect_err("stream past cap must error");
        assert!(err.to_string().contains("decompressed past"), "{err}");
    }

    #[test]
    fn test_bounded_read_accepts_within_cap() {
        let data = b"hello world";
        let text =
            read_entry_bounded(std::io::Cursor::new(&data[..]), data.len() as u64, 100).unwrap();
        assert_eq!(text, "hello world");
    }

    #[test]
    fn test_paragraphs_from_document_xml_multiple_runs_per_paragraph() {
        // A single paragraph split across multiple <w:r> runs (e.g. bold +
        // plain text) must be concatenated without an artificial break.
        let xml = format!(
            r#"<w:document {NS}><w:body><w:p><w:r><w:t>Hello</w:t></w:r><w:r><w:t xml:space="preserve"> world</w:t></w:r></w:p></w:body></w:document>"#
        );
        let text = paragraphs_from_document_xml(&xml).unwrap();
        assert_eq!(text.trim(), "Hello world");
    }

    #[test]
    fn test_missing_document_xml_errors() {
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("readme.txt", opts).unwrap();
            zip.write_all(b"not a real docx").unwrap();
            zip.finish().unwrap();
        }
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("bad.docx");
        std::fs::write(&path, buf).unwrap();

        let result = extract(&path);
        assert!(
            result.is_err(),
            "a zip without word/document.xml must error"
        );
    }

    /// Regression test for RUSTSEC-2026-0194 (issue #3367): pre-patch
    /// quick-xml checked start-tag attributes for duplicate names in
    /// quadratic time, so a tag with a large number of attributes (duplicate
    /// or not) could pin CPU well within the existing size caps. Bounded
    /// join turns a reintroduced quadratic-time regression into a
    /// deterministic test failure instead of a slow/hung CI job.
    #[test]
    fn test_pathological_attribute_count_does_not_hang() {
        let n = 20_000;
        let attrs: String = (0..n).map(|i| format!(r#" a{i}="v""#)).collect();
        let xml = format!(
            r#"<w:document {NS}><w:body><w:p><w:r><w:t{attrs}>hi</w:t></w:r></w:p></w:body></w:document>"#
        );

        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let result = paragraphs_from_document_xml(&xml);
            let _ = tx.send(result.is_ok());
        });
        let completed = rx.recv_timeout(std::time::Duration::from_secs(10));
        assert!(
            completed.is_ok(),
            "a tag with many attributes must parse (Ok or Err) in bounded time, not hang"
        );
    }

    #[test]
    fn test_not_a_zip_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("not-a-zip.docx");
        std::fs::write(&path, b"this is definitely not a zip file").unwrap();

        let result = extract(&path);
        assert!(result.is_err(), "non-zip input must error");
    }
}
