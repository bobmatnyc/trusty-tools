//! Format detection and the converter interface.
//!
//! Why: PDF and Office files need an outside converter (plan section 3, the
//! worker process in [`crate::worker`]). Callers go through [`Converters`],
//! so registering that worker for its formats changes no caller.
//!
//! What: [`detect_format`] names a file's format from its extension, falling
//! back to a text sniff. [`Converters`] maps format names to [`Converter`]s;
//! [`Converters::with_builtins`] registers [`DirectReader`] for `text`,
//! `markdown` and `code`; [`Converters::standard`] adds the worker for PDF,
//! DOCX, PPTX, HTML and EML, plus XLSX in the Xberg 1.4 build. A format with no converter is an
//! [`Error::UnsupportedFormat`] naming it. A converter that opts in with
//! [`Converter::caches_text`] has its text cached by content hash and
//! converter name, so `read --lines` serves the outline's lines without
//! converting again.

use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;

use crate::cache::{Cache, content_hash};
use crate::config::Config;
use crate::error::{Error, Result};
use crate::worker::WorkerConverter;

/// Why a spreadsheet gets `unsupported_format` in this build.
pub const XLSX_OFF: &str = "XLSX is off in the Xberg 1.3.10 build (calamine RUSTSEC-2026-0436); \
     the Xberg 1.4 build (rustc 1.99) converts it";

/// Format name for plain text.
pub const TEXT: &str = "text";
/// Format name for Markdown.
pub const MARKDOWN: &str = "markdown";
/// Format name for source code and structured text read as-is.
pub const CODE: &str = "code";

const MARKDOWN_EXT: &[&str] = &["md", "markdown", "mdx", "mdown"];
const TEXT_EXT: &[&str] = &["txt", "text", "log", "rst", "adoc", "org", "csv", "tsv"];
const CODE_EXT: &[&str] = &[
    "rs", "py", "js", "mjs", "cjs", "ts", "tsx", "jsx", "go", "java", "kt", "swift", "c", "h",
    "cc", "cpp", "hpp", "cs", "rb", "php", "sh", "bash", "zsh", "fish", "sql", "toml", "yaml",
    "yml", "json", "jsonl", "xml", "css", "scss", "lua", "pl", "r", "scala", "zig", "ex", "exs",
    "erl", "hs", "ml", "nix", "proto", "graphql", "ini", "cfg", "conf", "mk", "cmake", "vue",
    "svelte",
];
/// Document formats a converter can claim; detected by extension so the
/// error names them even when no converter is registered.
const DOCUMENT_EXT: &[&str] = &[
    "pdf", "docx", "doc", "pptx", "ppt", "xlsx", "xls", "odt", "ods", "odp", "rtf", "epub", "html",
    "htm", "eml", "msg",
];
const SPECIAL_NAMES: &[(&str, &str)] = &[
    ("makefile", CODE),
    ("dockerfile", CODE),
    ("license", TEXT),
    ("readme", TEXT),
];

/// Names the format of `path`, using `bytes` only when the extension is
/// unknown or absent.
///
/// What: known extensions map to `markdown`, `text`, `code` or the document
/// extension itself (`pdf`, `docx`, ...). Otherwise UTF-8 bytes with no NUL
/// are `text`, and anything else is the lowercase extension, or `binary`.
/// Test: `formats_are_detected`
pub fn detect_format(path: &Path, bytes: &[u8]) -> String {
    let ext = path
        .extension()
        .and_then(|e| e.to_str())
        .map(str::to_ascii_lowercase);
    if let Some(ext) = ext.as_deref() {
        if MARKDOWN_EXT.contains(&ext) {
            return MARKDOWN.into();
        }
        if TEXT_EXT.contains(&ext) {
            return TEXT.into();
        }
        if CODE_EXT.contains(&ext) {
            return CODE.into();
        }
        if DOCUMENT_EXT.contains(&ext) {
            return ext.into();
        }
    }
    if ext.is_none() {
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .map(str::to_ascii_lowercase)
            .unwrap_or_default();
        if let Some((_, format)) = SPECIAL_NAMES.iter().find(|(n, _)| *n == name) {
            return (*format).into();
        }
    }
    if !bytes.contains(&0) && std::str::from_utf8(bytes).is_ok() {
        return TEXT.into();
    }
    ext.unwrap_or_else(|| "binary".into())
}

/// What a converter is given.
#[derive(Debug, Clone, Copy)]
pub struct ConvertInput<'a> {
    /// The source path, for messages and for converters that need a file.
    pub path: &'a Path,
    /// The file's bytes, already read and size-checked.
    pub bytes: &'a [u8],
    /// The detected format name.
    pub format: &'a str,
}

/// Turns a file's bytes into plain text.
///
/// Why: the seam where the later converter worker plugs in (plan section 3).
/// What: `convert` returns the document's text, whose lines are the ones the
/// outline and `read --lines` refer to. It must be deterministic for the same
/// bytes: results are cached by content hash plus the converter's
/// [`Converter::name`], so a converter whose output changes must change its
/// name (for example, by including a version). Failures are
/// [`Error::Conversion`] or [`Error::InvalidText`]; they become a per-file
/// error record and never stop a batch. Implementations are shared across
/// worker threads.
/// Test: `registered_converter_handles_its_format`
pub trait Converter: Send + Sync {
    /// A stable name, recorded in output and in the cache key.
    fn name(&self) -> &str;
    /// Converts one file to text.
    fn convert(&self, input: &ConvertInput<'_>) -> Result<String>;
    /// Whether the converted text is worth caching (conversion is costly).
    /// Default `false`: a direct read is cheaper than a cache read.
    fn caches_text(&self) -> bool {
        false
    }
}

/// Converts with `converter`, reading and writing the converted-text cache
/// when `cache` is given and the converter opts in.
///
/// Errors: the converter's error, or [`Error::Cache`] when the text cannot
/// be stored; a failed store is never reported as a success.
/// Test: `workers/summarizer-convert/tests/worker.rs` (outline_ranges_match_read_on_converted_documents)
pub fn convert_cached(
    converter: &dyn Converter,
    input: &ConvertInput<'_>,
    cache: Option<&Cache>,
) -> Result<String> {
    let Some(cache) = cache.filter(|_| converter.caches_text()) else {
        return converter.convert(input);
    };
    let key = converted_text_key(&content_hash(input.bytes), converter.name());
    if let Some(text) = cache.get_text(&key) {
        return Ok(text);
    }
    let text = converter.convert(input)?;
    cache.put_text(&key, &text)?;
    Ok(text)
}

/// The converted-text cache key for content `hash` and `converter`.
///
/// #2: the key carries the cache schema, so text a converter stored before
/// a schema bump (e.g. PDFs without `[table ...]` blocks) is never served.
pub(crate) fn converted_text_key(hash: &str, converter: &str) -> String {
    let settings = format!("converted-text/{}", crate::pipeline::CACHE_SCHEMA);
    Cache::key(hash, converter, &settings)
}

/// Reads UTF-8 text as-is, dropping a leading byte-order mark.
#[derive(Debug, Default, Clone, Copy)]
pub struct DirectReader;

impl Converter for DirectReader {
    fn name(&self) -> &str {
        "direct"
    }

    fn convert(&self, input: &ConvertInput<'_>) -> Result<String> {
        let bytes = input
            .bytes
            .strip_prefix(b"\xEF\xBB\xBF")
            .unwrap_or(input.bytes);
        String::from_utf8(bytes.to_vec()).map_err(|e| {
            Error::InvalidText(format!(
                "{} at byte {}",
                input.path.display(),
                e.utf8_error().valid_up_to()
            ))
        })
    }
}

/// Format name to converter.
#[derive(Clone, Default)]
pub struct Converters {
    by_format: BTreeMap<String, Arc<dyn Converter>>,
}

impl std::fmt::Debug for Converters {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let map: BTreeMap<&str, &str> = self
            .by_format
            .iter()
            .map(|(k, v)| (k.as_str(), v.name()))
            .collect();
        f.debug_struct("Converters")
            .field("by_format", &map)
            .finish()
    }
}

impl Converters {
    /// No converters at all.
    pub fn empty() -> Self {
        Converters::default()
    }

    /// [`DirectReader`] for `text`, `markdown` and `code`.
    pub fn with_builtins() -> Self {
        let mut c = Converters::empty();
        let direct: Arc<dyn Converter> = Arc::new(DirectReader);
        for format in [TEXT, MARKDOWN, CODE] {
            c.register(format, Arc::clone(&direct));
        }
        c
    }

    /// [`Converters::with_builtins`] plus the converter worker for
    /// [`crate::worker::WORKER_FORMATS`], with `config.convert`'s limits.
    pub fn standard(config: &Config) -> Self {
        let mut c = Converters::with_builtins();
        WorkerConverter::register(&mut c, &config.convert);
        c
    }

    /// Registers `converter` for `format`, replacing any earlier one.
    pub fn register(&mut self, format: &str, converter: Arc<dyn Converter>) {
        self.by_format
            .insert(format.to_ascii_lowercase(), converter);
    }

    /// The converter for `format`, or [`Error::UnsupportedFormat`].
    pub fn get(&self, format: &str) -> Result<&Arc<dyn Converter>> {
        self.by_format
            .get(format)
            .ok_or_else(|| Error::UnsupportedFormat {
                format: format.to_string(),
                note: (format == "xlsx" && !cfg!(feature = "xberg14")).then_some(XLSX_OFF),
            })
    }

    /// Reads, size-checks, detects and converts one file.
    ///
    /// Used by `read --lines`, so the lines it prints are the lines the
    /// outline referred to.
    pub fn load_text(&self, path: &Path, max_bytes: u64) -> Result<String> {
        self.load_text_cached(path, max_bytes, None)
    }

    /// [`Converters::load_text`] through the converted-text cache, so a
    /// converted document's lines come from the cache when present.
    pub fn load_text_cached(
        &self,
        path: &Path,
        max_bytes: u64,
        cache: Option<&Cache>,
    ) -> Result<String> {
        let bytes = read_limited(path, max_bytes)?;
        let format = detect_format(path, &bytes);
        let converter = self.get(&format)?;
        let input = ConvertInput {
            path,
            bytes: &bytes,
            format: &format,
        };
        convert_cached(converter.as_ref(), &input, cache)
    }
}

/// Reads a file, refusing one over `max_bytes` before reading it.
pub(crate) fn read_limited(path: &Path, max_bytes: u64) -> Result<Vec<u8>> {
    let meta = std::fs::metadata(path).map_err(|e| Error::io(path, e))?;
    if meta.len() > max_bytes {
        return Err(Error::TooLarge {
            size: meta.len(),
            limit: max_bytes,
        });
    }
    std::fs::read(path).map_err(|e| Error::io(path, e))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_are_detected() {
        let cases = [
            ("a.md", &b"# x"[..], MARKDOWN),
            ("a.TXT", b"x", TEXT),
            ("lib.rs", b"fn x(){}", CODE),
            ("Makefile", b"all:", CODE),
            ("report.pdf", b"%PDF-1.7", "pdf"),
            ("notes", b"plain words", TEXT),
            ("blob.bin", b"\x00\x01", "bin"),
            ("blob", b"\xff\x00", "binary"),
        ];
        for (name, bytes, want) in cases {
            assert_eq!(detect_format(Path::new(name), bytes), want, "{name}");
        }
    }

    #[test]
    fn direct_reader_rejects_invalid_utf8() {
        let input = ConvertInput {
            path: Path::new("x.txt"),
            bytes: b"ok\xff",
            format: TEXT,
        };
        assert_eq!(
            DirectReader.convert(&input).unwrap_err().kind(),
            "invalid_text"
        );
    }

    #[test]
    fn missing_converter_names_the_format() {
        let err = Converters::with_builtins().get("pdf").err().unwrap();
        assert_eq!(err.format(), Some("pdf"));
    }

    #[cfg(not(feature = "xberg14"))]
    #[test]
    fn xlsx_is_unsupported_and_names_the_xberg_14_build() {
        let xlsx = Converters::standard(&Config::default())
            .get("xlsx")
            .err()
            .unwrap();
        assert_eq!(xlsx.kind(), "unsupported_format");
        assert!(xlsx.to_string().contains("XLSX is off"), "{xlsx}");
        assert!(xlsx.to_string().contains("Xberg 1.4 build"), "{xlsx}");
    }

    #[cfg(feature = "xberg14")]
    #[test]
    fn xlsx_goes_to_the_worker_in_the_xberg_14_build() {
        let c = Converters::standard(&Config::default());
        assert!(c.get("xlsx").is_ok());
        assert_eq!(c.get("xls").err().unwrap().kind(), "unsupported_format");
    }
}
