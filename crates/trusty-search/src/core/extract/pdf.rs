//! PDF text extraction (issue #2923).
//!
//! Why: PDFs are common in real repos (specs, design docs, vendor manuals)
//! but were hard-skipped via `BINARY_EXTS` before this feature. `pdf-extract`
//! is a pure-Rust PDF parser + text extractor with no system dependency (no
//! poppler/pdfium shell-out), matching this workspace's preference for
//! portable, dependency-light builds.
//!
//! What: [`extract`] reads the file and runs `pdf_extract::extract_text_from_mem`.
//! A scanned/image-only PDF typically parses successfully but yields
//! (near-)empty text — OCR is explicitly out of scope for v1 (see the issue),
//! so that case is surfaced via [`super::Extracted::warning`] rather than
//! silently producing an empty index entry.
//!
//! Test: `test_extracts_simple_pdf_text`, `test_scanned_pdf_like_warns`,
//! `test_corrupt_pdf_errors`.

use std::path::Path;

use super::{ExtractError, Extracted};

/// Extract text from a PDF file.
///
/// Why/What: see module docs. The "no extractable text" warning fires only
/// when the extracted text is entirely empty (after trimming whitespace) —
/// not merely short — because a real single-word or short-heading PDF is
/// legitimate and must not be misclassified as scanned/image-only.
/// Test: `test_extracts_simple_pdf_text`, `test_scanned_pdf_like_warns`.
pub fn extract(path: &Path) -> Result<Extracted, ExtractError> {
    let bytes = std::fs::read(path).map_err(|source| ExtractError::Io {
        path: path.display().to_string(),
        source,
    })?;

    let text =
        pdf_extract::extract_text_from_mem(&bytes).map_err(|e| ExtractError::Pdf(e.to_string()))?;

    let warning = if text.trim().is_empty() {
        Some("no extractable text (scanned/image PDF? OCR not supported)".to_string())
    } else {
        None
    };

    Ok(Extracted { text, warning })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Build a minimal, spec-valid single-page PDF containing one text-showing
    /// operator, with an accurate `xref` offset table computed as the buffer
    /// is assembled.
    ///
    /// Why: pdf-extract (via lopdf) parses the `xref` table to locate objects;
    /// a hand-written table with wrong offsets fails to parse. Building it
    /// incrementally and recording each object's start offset keeps the
    /// fixture both tiny and byte-accurate.
    fn build_minimal_pdf(text: &str) -> Vec<u8> {
        let mut buf: Vec<u8> = Vec::new();
        let mut offsets: Vec<usize> = Vec::with_capacity(5);

        buf.extend_from_slice(b"%PDF-1.4\n");

        offsets.push(buf.len());
        buf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");

        offsets.push(buf.len());
        buf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");

        offsets.push(buf.len());
        buf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /Resources << /Font << /F1 4 0 R >> >> \
              /MediaBox [0 0 200 200] /Contents 5 0 R >>\nendobj\n",
        );

        offsets.push(buf.len());
        buf.extend_from_slice(
            b"4 0 obj\n<< /Type /Font /Subtype /Type1 /BaseFont /Helvetica >>\nendobj\n",
        );

        offsets.push(buf.len());
        let stream = format!("BT /F1 24 Tf 10 100 Td ({text}) Tj ET");
        let mut obj5 = Vec::new();
        write!(
            obj5,
            "5 0 obj\n<< /Length {} >>\nstream\n{}\nendstream\nendobj\n",
            stream.len(),
            stream
        )
        .unwrap();
        buf.extend_from_slice(&obj5);

        let xref_start = buf.len();
        let mut xref = String::from("xref\n0 6\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        buf.extend_from_slice(xref.as_bytes());

        write!(
            buf,
            "trailer\n<< /Size 6 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF"
        )
        .unwrap();

        buf
    }

    #[test]
    fn test_extracts_simple_pdf_text() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("simple.pdf");
        std::fs::write(&path, build_minimal_pdf("Hello World")).unwrap();

        let extracted = extract(&path).expect("extraction must succeed");
        assert!(
            extracted.text.contains("Hello World"),
            "expected extracted text to contain the drawn string, got: {:?}",
            extracted.text
        );
        assert!(extracted.warning.is_none());
    }

    #[test]
    fn test_scanned_pdf_like_warns() {
        // A page with no /Contents text-showing operator at all — no /Font,
        // empty content stream — models a scanned/image-only PDF: it parses
        // fine but yields no extractable text.
        let mut buf: Vec<u8> = Vec::new();
        let mut offsets: Vec<usize> = Vec::with_capacity(4);
        buf.extend_from_slice(b"%PDF-1.4\n");
        offsets.push(buf.len());
        buf.extend_from_slice(b"1 0 obj\n<< /Type /Catalog /Pages 2 0 R >>\nendobj\n");
        offsets.push(buf.len());
        buf.extend_from_slice(b"2 0 obj\n<< /Type /Pages /Kids [3 0 R] /Count 1 >>\nendobj\n");
        offsets.push(buf.len());
        buf.extend_from_slice(
            b"3 0 obj\n<< /Type /Page /Parent 2 0 R /MediaBox [0 0 200 200] \
              /Contents 4 0 R >>\nendobj\n",
        );
        offsets.push(buf.len());
        buf.extend_from_slice(b"4 0 obj\n<< /Length 0 >>\nstream\n\nendstream\nendobj\n");

        let xref_start = buf.len();
        let mut xref = String::from("xref\n0 5\n0000000000 65535 f \n");
        for off in &offsets {
            xref.push_str(&format!("{off:010} 00000 n \n"));
        }
        buf.extend_from_slice(xref.as_bytes());
        write!(
            buf,
            "trailer\n<< /Size 5 /Root 1 0 R >>\nstartxref\n{xref_start}\n%%EOF"
        )
        .unwrap();

        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("scanned.pdf");
        std::fs::write(&path, &buf).unwrap();

        let extracted = extract(&path).expect("extraction must succeed even with no text");
        assert!(
            extracted.text.trim().is_empty(),
            "expected no extractable text, got: {:?}",
            extracted.text
        );
        assert!(
            extracted.warning.is_some(),
            "expected a scanned/image-PDF warning"
        );
    }

    #[test]
    fn test_corrupt_pdf_errors() {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("corrupt.pdf");
        std::fs::write(&path, b"not a pdf at all").unwrap();

        let result = extract(&path);
        assert!(result.is_err(), "corrupt input must return an error");
    }
}
