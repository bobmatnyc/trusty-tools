//! SPEC-OKF-01 Appendix B fixtures against the real worker, through the S10
//! client (`trusty_okf::extract::worker::WorkerExtractor`). Criterion 105,
//! slice S10b of #10746.
//!
//! Why: S10 proved the client against test doubles; only the real binary can
//! prove that xberg gives each fixture its expected outcome, that output is
//! byte-identical across runs (DOC-55 E1), and that the worker's `hello`
//! passes every host check, the sandbox cross-check included.
//! What: each test builds its fixture with `support/docs.rs`, spawns this
//! crate's binary through `WorkerExtractor` with the default limits and the
//! platform sandbox (where its launcher is installed; the Off policy
//! otherwise), and asserts the Appendix B outcome. Each runs under `within`,
//! a timer of the deadlines it exercises plus 5 s.
//! Test: `cargo test --manifest-path workers/okf-extract/Cargo.toml`.

#[path = "support/docs.rs"]
mod docs;

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use serde_yaml::Value;
use sha2::{Digest, Sha256};
use trusty_okf::build::registry::{ConnectorParams, Locator, SourceSpec};
use trusty_okf::extract::worker::{Limits, SandboxPolicy, WorkerConfig, WorkerExtractor};
use trusty_okf::extract::{ExtractError, Extracted, Extractor, TypeHint};

const WORKER: &str = env!("CARGO_BIN_EXE_trusty-okf-xberg-worker");

/// The handshake and one item at the default deadlines, plus 5 s.
fn budget(items: u64) -> Duration {
    let d = Limits::default();
    d.handshake + d.item * items as u32 + d.shutdown + Duration::from_secs(5)
}

/// Run `f` on its own thread; panic if it outlasts `limit`.
fn within<T: Send + 'static>(limit: Duration, f: impl FnOnce() -> T + Send + 'static) -> T {
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || {
        let _ = tx.send(std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)));
    });
    match rx.recv_timeout(limit) {
        Ok(Ok(v)) => v,
        Ok(Err(p)) => std::panic::resume_unwind(p),
        Err(_) => panic!("did not finish within {limit:?}"),
    }
}

fn local() -> SourceSpec {
    let loc = Locator::DocStore {
        path: "/nonexistent".into(),
        extensions: Vec::new(),
        recursive: true,
    };
    SourceSpec::new("local", None, loc, "2026-11-05T00:00:00Z")
}

fn connector() -> SourceSpec {
    let loc = Locator::Connector {
        kind: "gdocs".into(),
        params: ConnectorParams::new(),
    };
    SourceSpec::new("docs", None, loc, "2026-11-05T00:00:00Z")
}

/// The platform launcher the S10 client applies, when it is installed.
fn launcher() -> Option<&'static str> {
    let l = if cfg!(target_os = "macos") {
        "/usr/bin/sandbox-exec"
    } else {
        "/usr/bin/bwrap"
    };
    Path::new(l).is_file().then_some(l)
}

/// A client for the real worker with working directories under `scratch`.
fn client(scratch: &Path, policy: SandboxPolicy, store: &SourceSpec) -> WorkerExtractor {
    let mut cfg = WorkerConfig::new(WORKER, scratch);
    cfg.sandbox = policy;
    let ex = WorkerExtractor::new(cfg).unwrap();
    ex.begin_run(store);
    ex
}

fn scratch() -> (tempfile::TempDir, PathBuf) {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().canonicalize().unwrap();
    (tmp, dir)
}

/// Extract each `(name, bytes)` with one client on a local store; the
/// client's run must end with no worker error.
fn extract_all(items: Vec<(&'static str, Vec<u8>)>) -> Vec<Result<Extracted, ExtractError>> {
    let n = items.len() as u64;
    within(budget(n), move || {
        let (_tmp, dir) = scratch();
        let ex = client(&dir, SandboxPolicy::Platform, &local());
        let out: Vec<_> = items
            .iter()
            .map(|(name, b)| ex.extract(b, &TypeHint::for_path(name)))
            .collect();
        let run = ex.end_run();
        assert!(run.errors.is_empty(), "worker errors: {:?}", run.errors);
        out
    })
}

fn one(name: &'static str, bytes: Vec<u8>) -> Result<Extracted, ExtractError> {
    extract_all(vec![(name, bytes)]).remove(0)
}

fn text(r: Result<Extracted, ExtractError>) -> String {
    match r {
        Ok(e) => e.text,
        Err(e) => panic!("extract failed: {} {}", e.code, e.message),
    }
}

fn code(r: Result<Extracted, ExtractError>) -> String {
    match r {
        Ok(e) => panic!("expected an error, got text {:?}", e.text),
        Err(e) => e.code,
    }
}

/// Byte offsets of `words` in `t`; panics on a missing word.
fn positions(t: &str, words: &[&str]) -> Vec<usize> {
    words
        .iter()
        .map(|w| {
            t.find(w)
                .unwrap_or_else(|| panic!("{w:?} missing from {t:?}"))
        })
        .collect()
}

fn ascending(p: &[usize]) -> bool {
    p.windows(2).all(|w| w[0] < w[1])
}

/// Why: Appendix B "Text PDF, 5 pages" — text in page order, byte-identical
/// on a second run (DOC-55 E1, criterion 105); every item records the §2.16
/// provenance.
/// What: one client extracts the PDF twice and a second client (a fresh
/// worker process) once more; all three are byte-identical. The page words
/// appear in page order. The provenance names worker 0.1.0, protocol 1,
/// xberg 1.3.10, the configuration hash in the client's version and the
/// input SHA-256.
/// Test: self-contained.
#[test]
fn text_pdf_is_in_page_order_and_byte_identical_across_runs() {
    let pdf = docs::text_pdf();
    let sha = format!("sha256:{:x}", Sha256::digest(&pdf));
    let (a, b, version) = {
        let pdf = pdf.clone();
        within(budget(2), move || {
            let (_tmp, dir) = scratch();
            let ex = client(&dir, SandboxPolicy::Platform, &local());
            let hint = TypeHint::for_path("report.pdf");
            let a = ex.extract(&pdf, &hint).unwrap();
            let b = ex.extract(&pdf, &hint).unwrap();
            let version = ex.version().to_string();
            assert!(ex.end_run().errors.is_empty());
            (a, b, version)
        })
    };
    let c = one("again.pdf", pdf);
    assert_eq!(a.text.as_bytes(), b.text.as_bytes(), "same process");
    assert_eq!(
        a.text.as_bytes(),
        c.as_ref().unwrap().text.as_bytes(),
        "fresh process"
    );
    assert_eq!(a.fields, c.unwrap().fields);
    assert!(
        ascending(&positions(&a.text, &docs::PAGE_WORDS)),
        "{}",
        a.text
    );
    assert_eq!(a.fields.get("page_count"), Some(&Value::from(5)));

    let p = &a.provenance;
    let get = |k: &str| p.get(k).cloned().unwrap_or(Value::Null);
    assert_eq!(get("extracted_by"), Value::from("xberg-worker@0.1.0"));
    assert_eq!(get("extract_protocol"), Value::from(1));
    assert_eq!(get("xberg_version"), Value::from("1.3.10"));
    assert_eq!(get("extract_input_sha256"), Value::from(sha));
    let config = get("extract_config");
    let config = config.as_str().unwrap();
    assert_eq!(config.len(), 16);
    assert_eq!(version, format!("1.0.1.0.{config}"));
}

/// Why: Appendix B "PDF with a table" — the table text is present.
/// What: every cell of the 3x3 table appears, row by row in reading order.
/// Test: self-contained.
#[test]
fn table_pdf_keeps_every_cell() {
    let t = text(one("table.pdf", docs::table_pdf()));
    let cells: Vec<&str> = docs::TABLE.iter().flatten().copied().collect();
    assert!(ascending(&positions(&t, &cells)), "{t}");
}

/// Why: Appendix B "Scanned PDF, no text layer" — empty text and a
/// `no_text_layer` note, not a failure (§3 (p) A: OCR off).
/// What: the image-only PDF extracts to empty text with that one note.
/// Test: self-contained.
#[test]
fn scanned_pdf_is_empty_text_with_a_note() {
    let e = one("scan.pdf", docs::scanned_pdf()).unwrap();
    assert_eq!(
        (e.text.as_str(), e.notes.as_slice()),
        ("", &["no_text_layer".to_string()][..])
    );
}

/// Why: Appendix B "Corrupt PDF" — `extract_error`, other items continue.
/// What: on one worker, the corrupt PDF fails `extract_error` with a fixed
/// message, and the next item still extracts.
/// Test: self-contained.
#[test]
fn corrupt_pdf_is_an_extract_error_and_the_next_item_extracts() {
    let mut out = extract_all(vec![
        ("broken.pdf", docs::corrupt_pdf()),
        ("report.pdf", docs::text_pdf()),
    ]);
    let next = out.pop().unwrap();
    let bad = out.pop().unwrap().unwrap_err();
    assert_eq!(bad.code, "extract_error", "{}", bad.message);
    assert!(bad.message.starts_with("xberg: "), "{}", bad.message);
    assert!(text(next).contains("Aardvark"));
}

/// Why: Appendix B "DOCX with headings, a list and a table" — all three
/// survive as Markdown.
/// What: the headings are ATX headings at levels 1 and 2, both list items
/// are Markdown list lines, and the table is a pipe table.
/// Test: self-contained.
#[test]
fn docx_keeps_headings_list_and_table_as_markdown() {
    let t = text(one("report.docx", docs::docx()));
    let lines: Vec<&str> = t.lines().map(str::trim).collect();
    let has = |want: &dyn Fn(&str) -> bool| lines.iter().any(|l| want(l));
    assert!(has(&|l| l == "# Quarterly Report"), "{t}");
    assert!(has(&|l| l == "## Highlights"), "{t}");
    for item in ["First point", "Second point"] {
        assert!(
            has(&|l| ["- ", "* ", "+ "].iter().any(|m| l == format!("{m}{item}"))),
            "{item}: {t}"
        );
    }
    assert!(
        has(&|l| l.starts_with('|') && l.contains("Region") && l.contains("Revenue")),
        "{t}"
    );
    assert!(
        has(&|l| l.starts_with('|') && l.contains("North") && l.contains("4800")),
        "{t}"
    );
}

/// Markdown table delimiter rows (`| --- | --- |`).
fn delimiter_rows(t: &str) -> usize {
    t.lines()
        .map(str::trim)
        .filter(|l| {
            l.starts_with('|') && l.contains("---") && !l.chars().any(char::is_alphanumeric)
        })
        .count()
}

/// Why: Appendix B "XLSX with two sheets" — one table per sheet, sheet names
/// in `fields`.
/// What: two Markdown tables, the first sheet's cells before the second's,
/// and `fields.sheet_names` is the two names in workbook order.
/// Test: self-contained.
#[test]
fn xlsx_gives_one_table_per_sheet_and_sheet_names() {
    let e = one("book.xlsx", docs::xlsx()).unwrap();
    assert_eq!(delimiter_rows(&e.text), 2, "{}", e.text);
    assert!(
        ascending(&positions(
            &e.text,
            &["Widgets", "Gadgets", "Rent", "Power"]
        )),
        "{}",
        e.text
    );
    let names: Vec<Value> = docs::SHEETS.iter().map(|s| Value::from(*s)).collect();
    assert_eq!(e.fields.get("sheet_names"), Some(&Value::Sequence(names)));
}

/// Why: Appendix B "PPTX" — slide-ordered text.
/// What: the zip stores the slides out of order; the text follows the
/// presentation's slide list.
/// Test: self-contained.
#[test]
fn pptx_text_is_in_slide_order() {
    let t = text(one("deck.pptx", docs::pptx()));
    assert!(ascending(&positions(&t, &docs::SLIDES)), "{t}");
}

/// Why: Appendix B "Zip-bomb DOCX" — `extract_error` or `output_too_large`,
/// within the memory and time limits.
/// What: a 64 KiB DOCX that inflates to 256 MiB fails with one of the two
/// codes under the default limits (4 GiB `RLIMIT_AS` on Linux, 30 s per
/// item), well inside the item deadline, and the next item extracts.
/// Test: self-contained.
#[test]
fn zip_bomb_docx_fails_within_the_limits() {
    let bomb = docs::zip_bomb_docx(256);
    assert!(bomb.len() < 2 << 20, "the bomb is {} bytes", bomb.len());
    let started = Instant::now();
    let mut out = extract_all(vec![("bomb.docx", bomb), ("deck.pptx", docs::pptx())]);
    let next = out.pop().unwrap();
    let c = code(out.pop().unwrap());
    assert!(
        ["extract_error", "output_too_large"].contains(&c.as_str()),
        "{c}"
    );
    assert!(
        started.elapsed() < Limits::default().item,
        "{:?}",
        started.elapsed()
    );
    assert!(text(next).contains("Opening slide"));
}

/// Why: Appendix B "File over the input limit" — `too_large`, not sent.
/// What: 8 MiB + 1 byte fails `too_large` and starts no worker.
/// Test: self-contained.
#[test]
fn input_over_the_limit_is_too_large_and_not_sent() {
    within(Duration::from_secs(5), || {
        let (_tmp, dir) = scratch();
        let ex = client(&dir, SandboxPolicy::Platform, &local());
        let big = vec![b'x'; (8 << 20) + 1];
        let e = ex
            .extract(&big, &TypeHint::for_path("big.pdf"))
            .unwrap_err();
        assert_eq!(e.code, "too_large");
        assert!(ex.spawned_groups().is_empty(), "a worker was started");
    });
}

/// Why: §2.16 r7.1 — the host refuses a worker whose `sandbox` claim
/// disagrees with its spawn evidence. The real worker must claim exactly
/// what the host applied (criterion 145, worker half).
/// What: under the Off policy the worker claims `none` and a local item
/// extracts. Where the platform launcher exists, under the Platform policy
/// a connector-store item (sandbox required) extracts, so the claim matched
/// the evidence; with no launcher, the connector item is `sandbox_required`.
/// Test: self-contained.
#[test]
fn real_worker_sandbox_claim_matches_the_spawn_evidence() {
    let pdf = docs::text_pdf();
    within(budget(2), move || {
        let (_tmp, dir) = scratch();
        let hint = TypeHint::for_path("report.pdf");
        let off = client(&dir, SandboxPolicy::Off, &local());
        assert!(off.extract(&pdf, &hint).unwrap().text.contains("Aardvark"));
        assert!(off.end_run().errors.is_empty());

        let on = client(&dir, SandboxPolicy::Platform, &connector());
        let got = on.extract(&pdf, &hint);
        match launcher() {
            Some(l) => {
                eprintln!("sandboxed half runs under {l}");
                assert!(got.unwrap().text.contains("Aardvark"));
            }
            None => {
                eprintln!("no platform launcher; connector item must be refused");
                assert_eq!(got.unwrap_err().code, "sandbox_required");
            }
        }
        assert!(on.end_run().errors.is_empty());
    });
}

/// Why: Appendix B — a type with no xberg handler is `unsupported_format`
/// and does not cost the worker.
/// What: an `.exe` item fails `unsupported_format`; the next item extracts
/// on the same worker process.
/// Test: self-contained.
#[test]
fn unknown_type_is_unsupported_and_the_worker_stays_up() {
    let out = within(budget(2), || {
        let (_tmp, dir) = scratch();
        let ex = client(&dir, SandboxPolicy::Platform, &local());
        let a = ex.extract(b"MZ\x90\x00", &TypeHint::for_path("tool.exe"));
        let b = ex.extract(&docs::pptx(), &TypeHint::for_path("deck.pptx"));
        (
            a.unwrap_err().code,
            b.map(|e| e.text),
            ex.spawned_groups().len(),
        )
    });
    assert_eq!(out.0, "unsupported_format");
    assert!(out.1.unwrap().contains("Opening slide"));
    assert_eq!(out.2, 1, "one worker process served both items");
}
