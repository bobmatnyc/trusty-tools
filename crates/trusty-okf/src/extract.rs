//! Extraction: DOC-55's `Extractor` contract, type hints and the built-in
//! text tier (SPEC-OKF-01 §2.13, §2.16 "Tiers").
//!
//! Why: an import turns source bytes into concept text. §3 (o) A keeps UTF-8
//! text and Markdown in process and sends every other format to the
//! out-of-process worker, so trusty-okf never links an extraction backend
//! (B9). One contract serves both tiers; trusty-okf defines no other.
//! What: [`Extractor`] takes bytes and a [`TypeHint`] and returns
//! [`Extracted`] (`text`, `fields`, `notes`) or a per-item [`ExtractError`].
//! [`BuiltinText`] is the in-process tier: it handles the extensions in
//! [`TEXT_EXTENSIONS`], refuses binary content with `binary_no_extractor`,
//! and splits a Markdown file's own frontmatter into `fields`, which the
//! ingest engine checks against the protected set (§2.13.1). [`NoWorker`]
//! stands where no worker is configured: every item fails `worker_absent`.
//! [`fingerprint`] is the DOC-55 §4.5 tag, so an extractor change
//! re-extracts the items it produced. Slice S8 of #10746; the worker client
//! is `extract::worker` (feature `extract-worker`, slice S10).
//! Test: `builtin_tier_splits_markdown_and_refuses_binary`,
//! `c144_no_child_processes_skips_worker_items_and_refuses_connectors`.
//!
//! # Spec References
//! - SPEC-OKF-01 §2.13, §2.14 rule 8, §2.16, Appendix B error codes.

use serde_yaml::{Mapping, Value};
use sha2::{Digest, Sha256};

use crate::build::docstore::DEFAULT_EXTENSIONS;
use crate::format::frontmatter;

#[cfg(feature = "extract-worker")]
pub mod worker;

/// Extensions the built-in tier reads as UTF-8 text: today's doc-store
/// floor (DOC-55 §4.3).
pub const TEXT_EXTENSIONS: &[&str] = DEFAULT_EXTENSIONS;

/// Extensions whose leading YAML block is the file's own frontmatter.
const MARKDOWN_EXTENSIONS: &[&str] = &["md", "markdown", "mdx"];

/// What the source says an item is (Appendix B `type_hint`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TypeHint {
    /// The lowercase file extension, without the dot; empty when none.
    pub extension: String,
    /// A MIME type, when the source reports one.
    pub mime: Option<String>,
}

impl TypeHint {
    /// The hint for a source-relative path.
    pub fn for_path(rel: &str) -> TypeHint {
        let name = rel.rsplit('/').next().unwrap_or(rel);
        let extension = match name.rsplit_once('.') {
            Some((stem, ext)) if !stem.is_empty() => ext.to_lowercase(),
            _ => String::new(),
        };
        TypeHint {
            extension,
            mime: None,
        }
    }
}

/// An extractor's output for one item (DOC-55 §4.2).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Extracted {
    /// The concept body text.
    pub text: String,
    /// Structured fields; the engine quarantines protected keys (§2.13.1).
    pub fields: Mapping,
    /// Bounded notes for the report, never written into the bundle.
    pub notes: Vec<String>,
}

/// A per-item extraction failure; never ends the run (DOC-55 E3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ExtractError {
    /// A §2.14 rule 8 reason or an Appendix B code.
    pub code: String,
    /// A bounded message with no content excerpt (DOC-55 E4).
    pub message: String,
}

impl ExtractError {
    fn new(code: &str, message: &str) -> Self {
        Self {
            code: code.to_string(),
            message: message.to_string(),
        }
    }
}

/// DOC-55's `Extractor`: bytes and a type hint in, [`Extracted`] out.
pub trait Extractor {
    /// The extractor ID of the fingerprint tag and `extracted_by`.
    fn id(&self) -> &str;
    /// Its version.
    fn version(&self) -> &str;
    /// Extract one item. The bytes are the whole file (§2.14 rule 3).
    fn extract(&self, bytes: &[u8], hint: &TypeHint) -> Result<Extracted, ExtractError>;
}

/// The in-process tier: UTF-8 text and Markdown (§2.16 "Tiers", 3o A).
#[derive(Debug, Clone, Copy, Default)]
pub struct BuiltinText;

impl BuiltinText {
    /// Whether the built-in tier reads items of this type.
    pub fn handles(hint: &TypeHint) -> bool {
        TEXT_EXTENSIONS.contains(&hint.extension.as_str())
    }
}

impl Extractor for BuiltinText {
    fn id(&self) -> &str {
        "builtin-text"
    }

    fn version(&self) -> &str {
        "1"
    }

    /// What: refuses a type it does not handle (`unsupported_format`) and
    /// content with a NUL byte or invalid UTF-8 (`binary_no_extractor`). A
    /// Markdown file with a well-formed leading YAML mapping yields that
    /// mapping as `fields` and the rest as `text`; a block that does not
    /// parse stays in the text with a `malformed_frontmatter` note, so no
    /// byte is dropped.
    fn extract(&self, bytes: &[u8], hint: &TypeHint) -> Result<Extracted, ExtractError> {
        if !Self::handles(hint) {
            return Err(ExtractError::new(
                "unsupported_format",
                "not a built-in text type",
            ));
        }
        let text = match std::str::from_utf8(bytes) {
            Ok(t) if !bytes.contains(&0) => t,
            _ => {
                return Err(ExtractError::new(
                    "binary_no_extractor",
                    "content is not UTF-8 text",
                ));
            }
        };
        if !MARKDOWN_EXTENSIONS.contains(&hint.extension.as_str()) {
            return Ok(Extracted {
                text: text.to_string(),
                ..Extracted::default()
            });
        }
        let split = frontmatter::split(text);
        let Some(yaml) = split.frontmatter else {
            return Ok(Extracted {
                text: text.to_string(),
                ..Extracted::default()
            });
        };
        match frontmatter::parse(&yaml) {
            Ok(Value::Mapping(fields)) => Ok(Extracted {
                text: split.body,
                fields,
                notes: Vec::new(),
            }),
            _ => Ok(Extracted {
                text: text.to_string(),
                fields: Mapping::new(),
                notes: vec!["malformed_frontmatter".to_string()],
            }),
        }
    }
}

/// The extractor that stands where no worker is configured (§2.16 "Absent
/// worker"): every item fails `worker_absent` and is skipped.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoWorker;

impl Extractor for NoWorker {
    fn id(&self) -> &str {
        "xberg-worker"
    }

    fn version(&self) -> &str {
        "absent"
    }

    fn extract(&self, _bytes: &[u8], _hint: &TypeHint) -> Result<Extracted, ExtractError> {
        Err(ExtractError::new(
            "worker_absent",
            "no extraction worker is configured",
        ))
    }
}

/// The DOC-55 §4.5 fingerprint tag: `sha256:<hex>/<id>:<version>`.
pub fn fingerprint(bytes: &[u8], ex: &dyn Extractor) -> String {
    let hex: String = Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("sha256:{hex}/{}:{}", ex.id(), ex.version())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Why: the built-in tier is today's floor and must never pass binary
    /// content or lose a byte of a malformed frontmatter block.
    /// What: a Markdown file with frontmatter, one with a broken block, a
    /// text file, a NUL-bearing text file and a PDF.
    /// Test: self-contained.
    #[test]
    fn builtin_tier_splits_markdown_and_refuses_binary() {
        let md = TypeHint::for_path("notes/a.md");
        let got = BuiltinText
            .extract(b"---\ntitle: A\nverified: yes\n---\nBody\n", &md)
            .unwrap();
        assert_eq!(got.text, "Body\n");
        assert_eq!(got.fields.len(), 2);
        let broken = "---\n: : :\n---\nBody\n";
        let got = BuiltinText.extract(broken.as_bytes(), &md).unwrap();
        assert_eq!(got.text, broken);
        assert_eq!(got.notes, vec!["malformed_frontmatter".to_string()]);
        let txt = TypeHint::for_path("b.txt");
        assert_eq!(
            BuiltinText.extract(b"---\nx: 1\n---\n", &txt).unwrap().text,
            "---\nx: 1\n---\n"
        );
        let code = |r: Result<Extracted, ExtractError>| r.unwrap_err().code;
        assert_eq!(
            code(BuiltinText.extract(b"a\0b", &txt)),
            "binary_no_extractor"
        );
        let pdf = TypeHint::for_path("c.pdf");
        assert!(!BuiltinText::handles(&pdf));
        assert_eq!(
            code(BuiltinText.extract(b"%PDF", &pdf)),
            "unsupported_format"
        );
        assert_eq!(code(NoWorker.extract(b"%PDF", &pdf)), "worker_absent");
        assert_eq!(TypeHint::for_path(".profile").extension, "");
    }
}
