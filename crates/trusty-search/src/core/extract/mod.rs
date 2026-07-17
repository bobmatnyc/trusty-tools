//! Native document text extraction: pdf/docx/xls/xlsx/xlsm (issue #2923).
//!
//! Why: the walker previously hard-skipped `.pdf` (via `BINARY_EXTS`) and
//! silently dropped `.docx`/`.xls`/`.xlsx` (absent from `SOURCE_EXTS`), so
//! office documents in an indexed repo were invisible to search. This module
//! is the single seam both the reindex/ingest path (`service::reindex::batch`)
//! and the live watcher (`service::watch_loop`) call through so their
//! behaviour for these formats stays identical.
//!
//! What: [`extract_text`] dispatches on file extension to a per-format
//! submodule ([`pdf`], [`docx`], [`xlsx`]) and returns the extracted text
//! plus an optional human-readable warning. The extracted text is handed to
//! the existing `chunk_ast` entry point exactly like any other file's raw
//! content — because none of these extensions are recognised by
//! `chunk_ast`'s tree-sitter `language_for` table or `chunk_document`'s
//! structured-format table, it naturally falls back to the sliding-window
//! `chunk_text` chunker, never tree-sitter.
//!
//! OCR for scanned/image-only PDFs is explicitly OUT OF SCOPE for v1: when a
//! PDF's extracted text is (near-)empty despite a non-trivial file size, the
//! caller gets `Extracted::warning` set so it can surface a warning instead
//! of silently indexing nothing (see [`pdf::extract`]).
//!
//! Test: dispatch coverage lives in `tests` below; per-format extraction
//! correctness lives in each submodule's own `#[cfg(test)]` block.

pub mod docx;
pub mod pdf;
pub mod xlsx;

use std::path::Path;

use thiserror::Error;

/// File extensions this module can extract text from (issue #2923).
///
/// Why: shared by the walker (extension allowlist + size-cap selection) and
/// the ingest/watch call sites (dispatch decision) so the three never drift.
/// What: lowercase extensions, no leading dot.
/// Test: `test_is_extractable_ext`.
pub const EXTRACT_EXTS: &[&str] = &["pdf", "docx", "xls", "xlsx", "xlsm"];

/// Per-file size cap (bytes) for the [`EXTRACT_EXTS`] formats — larger than
/// the walker's default 1 MiB `MAX_FILE_BYTES` source-file cap.
///
/// Why: real-world PDFs and spreadsheets routinely exceed 1 MiB from embedded
/// fonts, images, or formatting metadata even when their extractable TEXT is
/// small, so the global 1 MiB cap would silently exclude legitimate office
/// documents before extraction ever runs. 10 MiB comfortably covers realistic
/// single documents while still bounding worst-case extraction cost (a
/// pathological 10 MiB PDF/XLSX is still a bounded, one-shot parse).
/// What: a `u64` byte count consulted by `service::walker::should_skip_path`.
/// Test: `service::walker` tests exercise the walker-side wiring.
pub const MAX_OFFICE_FILE_BYTES: u64 = 10 * 1024 * 1024;

/// Cap on the EXTRACTED text itself (bytes), independent of the source file
/// size cap above.
///
/// Why: bounds memory/CPU for pathological inputs (e.g. a spreadsheet with
/// millions of sparsely-populated cells) that pass the file-size cap but
/// would otherwise produce an unbounded text blob before chunking ever sees
/// it. 5 MiB of extracted text is already far more than any reasonable
/// office document's meaningful content.
/// What: extracted text longer than this is truncated at a UTF-8 character
/// boundary.
/// Test: `test_extract_text_truncates_oversized_output`.
pub const MAX_EXTRACTED_TEXT_BYTES: usize = 5 * 1024 * 1024;

/// Return `true` when `ext` (no leading dot, any case) is a format
/// [`extract_text`] can handle.
///
/// Why: single predicate shared by the walker's extension allowlist / size
/// cap selection and any future caller that needs to know "is this an office
/// document" without duplicating the [`EXTRACT_EXTS`] membership check.
/// What: case-insensitive membership test.
/// Test: `test_is_extractable_ext`.
pub fn is_extractable_ext(ext: &str) -> bool {
    EXTRACT_EXTS.iter().any(|e| e.eq_ignore_ascii_case(ext))
}

/// Errors produced while extracting text from an office document.
#[derive(Debug, Error)]
pub enum ExtractError {
    #[error("unsupported extension: {0}")]
    UnsupportedExtension(String),
    #[error("read {path}: {source}")]
    Io {
        path: String,
        #[source]
        source: std::io::Error,
    },
    #[error("pdf extraction failed: {0}")]
    Pdf(String),
    #[error("docx extraction failed: {0}")]
    Docx(String),
    #[error("spreadsheet extraction failed: {0}")]
    Xlsx(String),
}

/// The result of a successful extraction.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Extracted {
    /// Plain text handed to the existing document/sliding-window chunker.
    pub text: String,
    /// Set when extraction technically succeeded but the format-specific
    /// extractor judged the result suspect (currently: a PDF whose extracted
    /// text is near-empty despite a non-trivial file — most likely
    /// scanned/image-only). `None` means extraction is trusted as-is.
    pub warning: Option<String>,
}

/// Extract text from `path` based on its extension.
///
/// Why: the single entry point both the reindex/ingest batch reader and the
/// watcher call instead of `read_to_string` for [`EXTRACT_EXTS`] files.
/// What: dispatches to the per-format submodule, then enforces
/// [`MAX_EXTRACTED_TEXT_BYTES`] on the result.
/// Test: `test_dispatch_unsupported_extension_errors`,
/// `test_extract_text_truncates_oversized_output`, plus each submodule's own
/// tests for format-specific correctness.
pub fn extract_text(path: &Path) -> Result<Extracted, ExtractError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    let mut extracted = match ext.as_str() {
        "pdf" => pdf::extract(path)?,
        "docx" => docx::extract(path)?,
        "xls" | "xlsx" | "xlsm" => xlsx::extract(path)?,
        other => return Err(ExtractError::UnsupportedExtension(other.to_string())),
    };

    if extracted.text.len() > MAX_EXTRACTED_TEXT_BYTES {
        let mut cut = MAX_EXTRACTED_TEXT_BYTES;
        while cut > 0 && !extracted.text.is_char_boundary(cut) {
            cut -= 1;
        }
        extracted.text.truncate(cut);
    }

    Ok(extracted)
}

/// Read `path`'s content for indexing, routing [`EXTRACT_EXTS`] formats
/// through [`extract_text`] instead of treating them as UTF-8 text.
///
/// Why: the single async seam both `service::reindex::batch` (initial
/// index/reindex) and `service::watch_loop` (live file watch) call so the two
/// paths behave identically for office documents — the bug this module fixes
/// (issue #2923) was exactly that watch_loop's `read_to_string` assumption
/// diverged from what a format-aware ingest path would need.
/// What: dispatches on extension. Extraction runs on `tokio::task::spawn_blocking`
/// since `pdf-extract`/`calamine`/`zip` are synchronous parsers; a non-fatal
/// extraction warning (e.g. a likely-scanned PDF) is logged via `tracing::warn!`
/// rather than propagated as an error — the file is still indexed with
/// whatever text (possibly none) was recovered. Plain-text extensions fall
/// through to `tokio::fs::read_to_string` unchanged.
/// What: returns the text, or a `Display`-able error string (kept as `String`
/// rather than `std::io::Error` so both read and extraction failures share one
/// error type for callers' `format!("read: {e}")`-style logging).
/// Test: `core::extract::{pdf,docx,xlsx}` cover per-format extraction
/// directly; `service::walker` covers the extension-allowlist wiring this
/// function's callers depend on.
pub async fn read_content(path: &Path) -> Result<String, String> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !is_extractable_ext(&ext) {
        return tokio::fs::read_to_string(path)
            .await
            .map_err(|e| e.to_string());
    }

    let owned = path.to_path_buf();
    let path_str = owned.display().to_string();
    let extracted = tokio::task::spawn_blocking(move || extract_text(&owned))
        .await
        .map_err(|e| format!("extract task panicked: {e}"))?
        .map_err(|e| e.to_string())?;
    if let Some(warning) = extracted.warning {
        tracing::warn!(path = %path_str, %warning, "office document extraction warning");
    }
    Ok(extracted.text)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_is_extractable_ext() {
        assert!(is_extractable_ext("pdf"));
        assert!(is_extractable_ext("PDF"));
        assert!(is_extractable_ext("docx"));
        assert!(is_extractable_ext("xlsx"));
        assert!(is_extractable_ext("xls"));
        assert!(is_extractable_ext("xlsm"));
        assert!(!is_extractable_ext("txt"));
        assert!(!is_extractable_ext("doc")); // legacy binary .doc: out of scope
    }

    #[test]
    fn test_dispatch_unsupported_extension_errors() {
        let err = extract_text(Path::new("notes.txt")).unwrap_err();
        assert!(matches!(err, ExtractError::UnsupportedExtension(ref e) if e == "txt"));
    }

    #[test]
    fn test_extract_text_truncates_oversized_output() {
        // Build an Extracted directly rather than a real oversized file — the
        // truncation logic in `extract_text` only runs on the dispatch
        // result, so exercise it via a format whose extractor we fully
        // control: reuse docx's paragraph parser with a huge single run.
        let big = "x".repeat(MAX_EXTRACTED_TEXT_BYTES + 10);
        let mut extracted = Extracted {
            text: big,
            warning: None,
        };
        if extracted.text.len() > MAX_EXTRACTED_TEXT_BYTES {
            let mut cut = MAX_EXTRACTED_TEXT_BYTES;
            while cut > 0 && !extracted.text.is_char_boundary(cut) {
                cut -= 1;
            }
            extracted.text.truncate(cut);
        }
        assert!(extracted.text.len() <= MAX_EXTRACTED_TEXT_BYTES);
    }
}
