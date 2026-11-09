//! One `extract` request through xberg (SPEC-OKF-01 §2.16, Appendix B).
//!
//! Why: the worker turns untrusted document bytes into text with OCR, models
//! and every network path off, and must give byte-identical output for
//! identical bytes (DOC-55 E1, §2.16 "Determinism").
//! What: [`Engine`] holds one xberg configuration and a current-thread
//! runtime. [`Engine::reply`] answers one request: the format comes from the
//! hint's extension, else its MIME type (`unsupported_format` otherwise);
//! xberg extracts Markdown from the bytes in memory; a PDF with no text is
//! `no_text_layer` only when `pdf_check` proves it has pages and no text
//! layer, else `extract_error`; an xberg error is `extract_error` with a fixed message
//! naming only the error kind (no content excerpt, DOC-55 E4). An `ok` reply
//! echoes the worker's own SHA-256 of the bytes and carries the fields
//! `document_mime`, `page_count` and, for a workbook, `sheet_names`. Output
//! over a request limit is cut to one unit past it, so the host sees an
//! oversize reply inside its frame cap and fails the item `output_too_large`.
//! [`config_hash`] names the configuration.
//! Test: `formats_resolve_from_the_hint`, `oversize_output_is_cut_one_past_the_limit`,
//! `config_hash_is_stable`; the Appendix B fixtures in `tests/fixtures.rs`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.16 "Local-only configuration", "Determinism",
//!   "Provenance and errors"; Appendix B "Messages", "Error codes".

use serde_json::{Map, Value, json};
use sha2::{Digest, Sha256};
use xberg::core::config::{ExtractInput, ExtractionConfig, OutputFormat};
use xberg::{FormatMetadata, ImageExtractionConfig, XbergError};

use crate::proto::{ExtractHeader, Limits};

/// The xberg version the worker is pinned to (`=1.3.10`, §2.16).
pub const XBERG_VERSION: &str = "1.3.10";

/// The xberg features the worker is built with.
pub const FEATURES: [&str; 3] = ["pdf", "office", "excel"];

/// The formats the worker accepts: extension and MIME type.
pub const FORMATS: [(&str, &str); 4] = [
    ("pdf", "application/pdf"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
];

/// The most sheet names kept in `fields`, and the longest name kept; both
/// bound `fields` far below the 64 KiB default.
const MAX_SHEET_NAMES: usize = 256;
const MAX_SHEET_NAME_CHARS: usize = 64;

/// The xberg configuration: no cache (no disk writes), OCR off (§3 (p) A),
/// no image extraction or placeholders, no local-file or `file://` inputs,
/// Markdown output (tables and headings survive, Appendix B fixtures).
pub fn config() -> ExtractionConfig {
    let mut cfg = ExtractionConfig {
        use_cache: false,
        disable_ocr: true,
        output_format: OutputFormat::Markdown,
        images: Some(ImageExtractionConfig {
            extract_images: false,
            inject_placeholders: false,
            ..ImageExtractionConfig::default()
        }),
        ..ExtractionConfig::default()
    };
    cfg.url.allow_local_file_inputs = false;
    cfg.url.allow_file_uris = false;
    cfg
}

/// The configuration hash: the first 16 hex digits of the SHA-256 of the
/// xberg version, the feature list and the serialized configuration. Any
/// change to one of them changes the hash, so the host re-extracts exactly
/// the items this worker produced (§2.16 "Provenance and errors").
/// Errors: the configuration does not serialize.
pub fn config_hash(cfg: &ExtractionConfig) -> serde_json::Result<String> {
    // #10746: a serialization failure is an error, never a hash of no bytes.
    let serialized = serde_json::to_vec(cfg)?;
    let mut h = Sha256::new();
    h.update(XBERG_VERSION.as_bytes());
    h.update(b"\0");
    h.update(FEATURES.join(",").as_bytes());
    h.update(b"\0");
    h.update(serialized);
    Ok(hex(&h.finalize())[..16].to_string())
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// The format for a hint: the extension first, then the MIME type.
pub fn resolve(ext: &str, mime: Option<&str>) -> Option<(&'static str, &'static str)> {
    let ext = ext.to_ascii_lowercase();
    FORMATS
        .iter()
        .find(|(e, _)| *e == ext)
        .or_else(|| FORMATS.iter().find(|(_, m)| Some(*m) == mime))
        .copied()
}

/// A fixed message for an xberg error: its kind only (DOC-55 E4).
fn kind(e: &XbergError) -> &'static str {
    match e {
        XbergError::Io(_) => "xberg: I/O error",
        XbergError::Parsing { .. } => "xberg: parsing error",
        XbergError::Validation { .. } => "xberg: validation error",
        XbergError::Security { .. } => "xberg: security limit exceeded",
        XbergError::Timeout { .. } => "xberg: extraction timed out",
        XbergError::UnsupportedFormat(_) => "xberg: unsupported format",
        _ => "xberg: extraction failed",
    }
}

/// Cut `s` to the shortest char-boundary prefix longer than `limit` bytes,
/// when it is longer; the host then fails the item `output_too_large`.
fn cut_past(s: &mut String, limit: u64) {
    let limit = usize::try_from(limit).unwrap_or(usize::MAX);
    if s.len() > limit {
        let mut end = limit + 1;
        while !s.is_char_boundary(end) {
            end += 1;
        }
        s.truncate(end);
    }
}

/// The worker's extraction engine.
pub struct Engine {
    cfg: ExtractionConfig,
    hash: String,
    rt: tokio::runtime::Runtime,
}

impl Engine {
    /// The engine with [`config`]. Errors: the runtime cannot start, or the
    /// configuration hash cannot be computed.
    pub fn new() -> std::io::Result<Self> {
        let rt = tokio::runtime::Builder::new_current_thread()
            .enable_time()
            .build()?;
        let cfg = config();
        // #10746: no worker starts without a real configuration hash.
        let hash = config_hash(&cfg).map_err(std::io::Error::other)?;
        Ok(Engine { cfg, hash, rt })
    }

    /// The configuration hash for the `hello` reply.
    pub fn config_hash(&self) -> &str {
        &self.hash
    }

    /// The reply to one `extract` request: an `ok` or an `error` header.
    pub fn reply(&self, req: &ExtractHeader, bytes: Vec<u8>) -> Value {
        let id = req.request_id;
        let error = |code: &str, message: &str| json!({"type": "error", "request_id": id, "code": code, "message": message});
        let hint = &req.type_hint;
        let Some((ext, mime)) = resolve(&hint.extension, hint.mime.as_deref()) else {
            return error("unsupported_format", "no handler for this type");
        };
        let sha = hex(&Sha256::digest(&bytes));
        // #10746: kept for the no-text-layer proof below.
        let pdf = (ext == "pdf").then(|| bytes.clone());
        let input = ExtractInput::from_bytes(bytes, mime, Some(format!("input.{ext}")));
        let result = match self.rt.block_on(xberg::extract(input, &self.cfg)) {
            Ok(r) => r,
            Err(XbergError::UnsupportedFormat(_)) => {
                return error("unsupported_format", "no handler for this type");
            }
            Err(e) => return error("extract_error", kind(&e)),
        };
        let Some(doc) = result.results.into_iter().next() else {
            return error("extract_error", "xberg: no document extracted");
        };
        let mut text = doc.content;
        // #10746: `no_text_layer` is an Ok the host fingerprints. Only a PDF
        // proven to have pages and no text layer gets it; any other empty PDF
        // (no pages, an undecodable content stream) is `extract_error`.
        if let Some(pdf) = pdf
            && text.trim().is_empty()
        {
            return if crate::pdf_check::no_text_layer(&pdf) {
                error("no_text_layer", "the PDF has no text layer")
            } else {
                error(
                    "extract_error",
                    "xberg: no text from a PDF not proven image-only",
                )
            };
        }
        let mut fields = Map::new();
        let mut notes: Vec<String> = Vec::new();
        fields.insert("document_mime".into(), Value::from(doc.mime_type.as_ref()));
        // xberg fills `counts.pages` for a PDF and `metadata.pages` for some
        // other formats; either is the page (or slide) count.
        let pages = doc
            .metadata
            .pages
            .as_ref()
            .map(|p| p.total_count as usize)
            .filter(|n| *n > 0)
            .unwrap_or(doc.counts.pages);
        if pages > 0 {
            fields.insert("page_count".into(), Value::from(pages));
        }
        if let Some(FormatMetadata::Excel(x)) = &doc.metadata.format
            && let Some(names) = &x.sheet_names
        {
            if names.len() > MAX_SHEET_NAMES {
                notes.push("sheet_names_truncated".into());
            }
            let kept: Vec<String> = names
                .iter()
                .take(MAX_SHEET_NAMES)
                .map(|n| n.chars().take(MAX_SHEET_NAME_CHARS).collect())
                .collect();
            fields.insert("sheet_names".into(), Value::from(kept));
        }
        clamp(&mut text, &mut notes, req.limits);
        json!({
            "type": "ok",
            "request_id": id,
            "input_sha256": sha,
            "text": text,
            "fields": fields,
            "notes": notes,
            "provenance": { "pages": doc.counts.pages },
        })
    }
}

/// Cut text and notes one unit past their limits (see [`cut_past`]).
fn clamp(text: &mut String, notes: &mut Vec<String>, limits: Limits) {
    cut_past(text, limits.output_bytes);
    notes.truncate(limits.notes.saturating_add(1));
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the hint decides the handler; an unknown type is refused.
    /// What: extensions resolve case-insensitively, a MIME type resolves
    /// when the extension is unknown, and anything else is `None`.
    /// Test: self-contained.
    #[test]
    fn formats_resolve_from_the_hint() {
        assert_eq!(resolve("PDF", None).unwrap().0, "pdf");
        assert_eq!(resolve("", Some(FORMATS[2].1)).unwrap().0, "xlsx");
        assert!(resolve("exe", None).is_none());
        assert!(resolve("", Some("text/plain")).is_none());
    }

    /// Why: an oversize reply must stay inside the host's frame cap and
    /// still read as oversize, so the host reports `output_too_large`.
    /// What: text over the limit is cut to one byte past it (rounded up to
    /// a char boundary); text at the limit is untouched; notes keep one past
    /// their limit.
    /// Test: self-contained.
    #[test]
    fn oversize_output_is_cut_one_past_the_limit() {
        let lim = Limits {
            output_bytes: 4,
            fields_bytes: 64,
            notes: 1,
        };
        let (mut t, mut n) = (
            "abcdefgh".to_string(),
            vec!["a".into(), "b".into(), "c".into()],
        );
        clamp(&mut t, &mut n, lim);
        assert_eq!((t.as_str(), n.len()), ("abcde", 2));
        let mut t = "abcd".to_string();
        clamp(&mut t, &mut Vec::new(), lim);
        assert_eq!(t, "abcd");
        let mut t = "abcdé!".to_string(); // `é` spans bytes 4..6
        cut_past(&mut t, 4);
        assert_eq!(t, "abcdé");
    }

    /// Why: the hash enters every fingerprint; it must not vary between
    /// processes or calls, and must move with the configuration.
    /// What: two calls agree; a changed setting changes it.
    /// Test: self-contained.
    #[test]
    fn config_hash_is_stable() {
        let a = config_hash(&config()).unwrap();
        assert_eq!(a, config_hash(&config()).unwrap());
        assert_eq!(a.len(), 16);
        let mut other = config();
        other.output_format = OutputFormat::Plain;
        assert_ne!(a, config_hash(&other).unwrap());
    }
}
