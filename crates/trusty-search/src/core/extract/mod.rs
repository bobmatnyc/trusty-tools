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

/// Wall-clock budget for a single file's extraction on the blocking pool.
///
/// Why: a pathological input (e.g. a PDF that sends `pdf-extract` into a
/// quadratic parse) would otherwise pin a `spawn_blocking` pool thread
/// forever, and watch-triggered re-extraction of the same file compounds the
/// leak until the pool starves. 30s is far beyond any legitimate 10 MiB
/// office document while still bounding how long an ingest slot can stall.
/// What: enforced in [`read_content`] via `tokio::time::timeout`; on expiry
/// the file is treated exactly like an extraction failure (skip + log at the
/// call sites). NOTE: `spawn_blocking` tasks cannot be cancelled — the
/// abandoned thread runs to completion in the background; the timeout bounds
/// pipeline stalling, not the thread itself.
/// Test: `test_run_extract_with_timeout_times_out`,
/// `test_run_extract_with_timeout_passes_through_success`.
pub const EXTRACT_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(30);

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
    #[error("{path} is {size} bytes, over the {cap} byte office-document cap")]
    TooLarge { path: String, size: u64, cap: u64 },
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
/// watcher call instead of `read_to_string` for [`EXTRACT_EXTS`] files. The
/// [`MAX_OFFICE_FILE_BYTES`] check here is deliberately redundant with the
/// walker's `should_skip_path` size gate (issue #3367): the walker is the
/// primary defense for the ingest/watch paths, but this function is a public
/// library entry point any future caller could reach directly, and a
/// malformed/oversized document handed straight to a vulnerable parser is
/// exactly the DoS blast radius this cap exists to bound — the extraction
/// module must not depend on every caller remembering to gate it upstream.
/// What: dispatches to the per-format submodule, then enforces
/// [`MAX_EXTRACTED_TEXT_BYTES`] on the result.
/// Test: `test_dispatch_unsupported_extension_errors`,
/// `test_extract_text_truncates_oversized_output`,
/// `test_extract_text_rejects_oversized_input`, plus each submodule's own
/// tests for format-specific correctness.
pub fn extract_text(path: &Path) -> Result<Extracted, ExtractError> {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .unwrap_or("")
        .to_ascii_lowercase();
    if !is_extractable_ext(&ext) {
        return Err(ExtractError::UnsupportedExtension(ext));
    }

    let size = std::fs::metadata(path)
        .map_err(|source| ExtractError::Io {
            path: path.display().to_string(),
            source,
        })?
        .len();
    if size > MAX_OFFICE_FILE_BYTES {
        return Err(ExtractError::TooLarge {
            path: path.display().to_string(),
            size,
            cap: MAX_OFFICE_FILE_BYTES,
        });
    }

    let mut extracted = match ext.as_str() {
        "pdf" => pdf::extract(path)?,
        "docx" => docx::extract(path)?,
        "xls" | "xlsx" | "xlsm" => xlsx::extract(path)?,
        other => return Err(ExtractError::UnsupportedExtension(other.to_string())),
    };

    if extracted.text.len() > MAX_EXTRACTED_TEXT_BYTES {
        let cut = crate::truncate_at_char_boundary(&extracted.text, MAX_EXTRACTED_TEXT_BYTES).len();
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
/// since `pdf-extract`/`calamine`/`zip` are synchronous parsers, bounded by
/// [`EXTRACT_TIMEOUT`] (expiry is reported as an ordinary extraction error, so
/// both call sites skip + log it); a non-fatal
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
    let extracted = run_extract_with_timeout(move || extract_text(&owned), EXTRACT_TIMEOUT).await?;
    if let Some(warning) = extracted.warning {
        tracing::warn!(path = %path_str, %warning, "office document extraction warning");
    }
    Ok(extracted.text)
}

/// Run a synchronous extraction closure on the blocking pool with a wall-clock
/// timeout.
///
/// Why: isolates the timeout/panic-containment plumbing from [`read_content`]
/// so the timeout path is unit-testable with a deliberately slow closure
/// instead of needing a real pathological document. Preserves the
/// `JoinError -> Err(String)` panic-containment semantics: a panicking parser
/// (pdf-extract has known panic paths on malformed input) is contained as an
/// error, never propagated.
/// What: `tokio::time::timeout` around `spawn_blocking(f)`. Timeout expiry
/// and task panic both map to `Err(String)`, indistinguishable from an
/// extraction error to callers (skip + log). See [`EXTRACT_TIMEOUT`]'s docs
/// for the cannot-cancel-blocking-thread caveat.
/// Test: `test_run_extract_with_timeout_times_out`,
/// `test_run_extract_with_timeout_passes_through_success`.
async fn run_extract_with_timeout<F>(f: F, limit: std::time::Duration) -> Result<Extracted, String>
where
    F: FnOnce() -> Result<Extracted, ExtractError> + Send + 'static,
{
    match tokio::time::timeout(limit, tokio::task::spawn_blocking(f)).await {
        Err(_elapsed) => Err(format!("extraction timed out after {limit:?}")),
        Ok(join) => join
            .map_err(|e| format!("extract task panicked: {e}"))?
            .map_err(|e| e.to_string()),
    }
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
        // Drive a REAL oversized extraction end-to-end through extract_text
        // (not a re-implementation of the truncation loop): a docx whose
        // single run exceeds MAX_EXTRACTED_TEXT_BYTES of text. DEFLATE keeps
        // the on-disk container tiny (well under MAX_OFFICE_FILE_BYTES) and
        // the document.xml stays under docx's 50 MiB decompression cap, so
        // only the extracted-text cap can be the limiting factor here.
        use std::io::Write;
        let big_run = "x".repeat(MAX_EXTRACTED_TEXT_BYTES + 4096);
        let document_xml = format!(
            r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?><w:document xmlns:w="http://schemas.openxmlformats.org/wordprocessingml/2006/main"><w:body><w:p><w:r><w:t>{big_run}</w:t></w:r></w:p></w:body></w:document>"#
        );
        let mut buf = Vec::new();
        {
            let cursor = std::io::Cursor::new(&mut buf);
            let mut zip = zip::ZipWriter::new(cursor);
            let opts: zip::write::FileOptions<'_, ()> = zip::write::FileOptions::default();
            zip.start_file("word/document.xml", opts).unwrap();
            zip.write_all(document_xml.as_bytes()).unwrap();
            zip.finish().unwrap();
        }
        assert!(
            (buf.len() as u64) < MAX_OFFICE_FILE_BYTES,
            "fixture container must stay under the office file cap"
        );
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("huge.docx");
        std::fs::write(&path, buf).unwrap();

        let extracted = extract_text(&path).expect("extraction must succeed");
        assert!(
            extracted.text.len() <= MAX_EXTRACTED_TEXT_BYTES,
            "extracted text must be truncated to the cap (got {})",
            extracted.text.len()
        );
        assert!(
            extracted.text.starts_with("xxx"),
            "truncation must keep the leading content"
        );
    }

    #[test]
    fn test_extract_text_rejects_oversized_input() {
        // Defense in depth (issue #3367): extract_text must reject an
        // over-cap file itself, not merely rely on the walker's
        // should_skip_path gate — a future caller could reach this function
        // without going through the walker at all.
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join("huge.pdf");
        let oversized = vec![0u8; (MAX_OFFICE_FILE_BYTES + 1) as usize];
        std::fs::write(&path, &oversized).unwrap();

        let err = extract_text(&path).expect_err("oversized input must be rejected");
        assert!(
            matches!(err, ExtractError::TooLarge { cap, .. } if cap == MAX_OFFICE_FILE_BYTES),
            "expected TooLarge error, got: {err}"
        );
    }

    #[tokio::test]
    async fn test_run_extract_with_timeout_times_out() {
        let result = run_extract_with_timeout(
            || {
                std::thread::sleep(std::time::Duration::from_secs(5));
                Ok(Extracted {
                    text: "too late".to_string(),
                    warning: None,
                })
            },
            std::time::Duration::from_millis(20),
        )
        .await;
        let err = result.expect_err("a stalled extraction must time out");
        assert!(err.contains("timed out"), "{err}");
    }

    #[tokio::test]
    async fn test_run_extract_with_timeout_passes_through_success() {
        let result = run_extract_with_timeout(
            || {
                Ok(Extracted {
                    text: "ok".to_string(),
                    warning: None,
                })
            },
            std::time::Duration::from_secs(5),
        )
        .await;
        assert_eq!(result.expect("fast extraction must succeed").text, "ok");
    }
}
