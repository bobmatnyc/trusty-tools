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

/// Extract paragraph text from a `.docx` file.
///
/// Why/What: see module docs.
/// Test: `test_extracts_paragraphs_preserving_breaks`.
pub fn extract(path: &Path) -> Result<Extracted, ExtractError> {
    let file = std::fs::File::open(path).map_err(|source| ExtractError::Io {
        path: path.display().to_string(),
        source,
    })?;
    let mut archive = zip::ZipArchive::new(file).map_err(|e| ExtractError::Docx(e.to_string()))?;

    let mut xml = String::new();
    archive
        .by_name(DOCUMENT_XML_PATH)
        .map_err(|e| ExtractError::Docx(format!("{DOCUMENT_XML_PATH}: {e}")))?
        .read_to_string(&mut xml)
        .map_err(|e| ExtractError::Docx(e.to_string()))?;

    Ok(Extracted {
        text: paragraphs_from_document_xml(&xml)?,
        warning: None,
    })
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
                para.push_str(
                    &t.unescape()
                        .map_err(|e| ExtractError::Docx(e.to_string()))?,
                );
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

    #[test]
    fn test_not_a_zip_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("not-a-zip.docx");
        std::fs::write(&path, b"this is definitely not a zip file").unwrap();

        let result = extract(&path);
        assert!(result.is_err(), "non-zip input must error");
    }
}
