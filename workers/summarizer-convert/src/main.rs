//! `trusty-summarizer-convert`: the converter worker.
//!
//! Why: Xberg and its parsers run in their own process (plan section 3), so a
//! parser crash or hang costs one file, and the library never links Xberg.
//!
//! What: speaks `trusty_summarizer::worker::proto` on stdin and stdout. It
//! writes a hello line, then answers one request per document until stdin
//! closes. Formats: `pdf`, `docx`, `pptx`, `html`/`htm` and `eml`; the
//! `xberg14` feature (a former Xberg 1.4 build, not carried here) adds `xlsx`. Xberg runs
//! with OCR off, no cache, no image extraction and no file or URL inputs,
//! and returns plain text. A PDF's detected tables follow their page's text
//! as `[table ...]` blocks (`tables`). An empty text is returned as such; the host turns
//! it into a `no_text_extracted` record.
//! Test: `workers/summarizer-convert/tests/worker.rs`

mod tables;

use std::io::{self, BufRead, BufWriter, Read, Write};
use std::process::ExitCode;

use trusty_summarizer::worker::proto::{Hello, Request, Response};
use xberg::core::config::{ExtractInput, ExtractionConfig, OutputFormat};
use xberg::{ImageExtractionConfig, XbergError};

/// Format name to MIME type, for the formats this worker converts.
const FORMATS: &[(&str, &str)] = &[
    ("pdf", "application/pdf"),
    (
        "docx",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document",
    ),
    (
        "pptx",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation",
    ),
    ("html", "text/html"),
    ("htm", "text/html"),
    ("eml", "message/rfc822"),
    #[cfg(feature = "xberg14")]
    (
        "xlsx",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet",
    ),
];

fn config() -> ExtractionConfig {
    let mut cfg = ExtractionConfig {
        use_cache: false,
        disable_ocr: true,
        output_format: OutputFormat::Plain,
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

fn error_kind(e: &XbergError) -> &'static str {
    match e {
        XbergError::Io(_) => "io",
        XbergError::Parsing { .. } => "parse",
        XbergError::Validation { .. } => "validation",
        XbergError::Security { .. } => "security_limit",
        XbergError::Timeout { .. } => "timeout",
        XbergError::UnsupportedFormat(_) => "unsupported_format",
        _ => "extract",
    }
}

fn convert(
    rt: &tokio::runtime::Runtime,
    cfg: &ExtractionConfig,
    req: &Request,
    bytes: Vec<u8>,
) -> Response {
    let id = req.id;
    let fail = |kind: &str, message: String| Response::Error {
        id,
        kind: kind.into(),
        message,
    };
    let Some((ext, mime)) = FORMATS.iter().find(|(f, _)| *f == req.format) else {
        return fail(
            "unsupported_format",
            format!("the worker does not convert `{}`", req.format),
        );
    };
    let input = ExtractInput::from_bytes(bytes, *mime, Some(format!("input.{ext}")));
    // Spec PR #2: a PDF's page markers place its tables; see `tables`.
    let pdf_cfg;
    let cfg = if *ext == "pdf" {
        pdf_cfg = ExtractionConfig {
            pages: Some(tables::page_markers()),
            ..cfg.clone()
        };
        &pdf_cfg
    } else {
        cfg
    };
    match rt.block_on(xberg::extract(input, cfg)) {
        Err(e) => fail(error_kind(&e), e.to_string()),
        Ok(result) => match result.results.into_iter().next() {
            Some(doc) => Response::Ok {
                id,
                text: tables::assemble(ext, doc.content, &doc.tables),
            },
            None => fail("extract", "xberg returned no document".into()),
        },
    }
}

fn serve() -> io::Result<()> {
    let rt = tokio::runtime::Builder::new_current_thread().build()?;
    let cfg = config();
    let mut input = io::stdin().lock();
    let mut out = BufWriter::new(io::stdout().lock());
    write_line(&mut out, &Hello::current())?;
    let mut header = String::new();
    loop {
        header.clear();
        if input.read_line(&mut header)? == 0 {
            return Ok(());
        }
        let req: Request = serde_json::from_str(header.trim_end())
            .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("bad request: {e}")))?;
        let len = usize::try_from(req.len)
            .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "request too long"))?;
        let mut bytes = vec![0; len];
        input.read_exact(&mut bytes)?;
        let response = convert(&rt, &cfg, &req, bytes);
        write_line(&mut out, &response)?;
    }
}

/// Writes `value` as one JSON line and flushes it.
fn write_line<T: serde::Serialize>(out: &mut impl Write, value: &T) -> io::Result<()> {
    serde_json::to_writer(&mut *out, value).map_err(io::Error::other)?;
    out.write_all(b"\n")?;
    out.flush()
}

fn main() -> ExitCode {
    match serve() {
        Ok(()) => ExitCode::SUCCESS,
        Err(e) => {
            eprintln!("trusty-summarizer-convert: error: {e}");
            ExitCode::from(1)
        }
    }
}
