//! The converter worker driven from outside, through the library's
//! `Summarizer` and `Converters` exactly as the CLI uses them.
//!
//! The real worker is this package's binary (`CARGO_BIN_EXE_*`). The crash
//! and hang cases use `examples/stand_in_worker.rs`, a separate process that
//! speaks the same protocol, so no hook exists in production code.

use std::path::{Path, PathBuf};

use trusty_summarizer::cache::Cache;
use trusty_summarizer::record::{ErrorRecord, Stage};
use trusty_summarizer::worker::locate_worker;
use trusty_summarizer::worker::proto::{BUILD_NAME, HOST_CONVERTER_NAME};
use trusty_summarizer::{
    BackendRegistry, Config, ConvertConfig, Converters, FileRecord, LineIndex, Summarizer,
    SummaryRecord,
};

const REAL: &str = env!("CARGO_BIN_EXE_trusty-summarizer-convert");

/// This package's sources, relative to its manifest.
const SOURCES: &str = ".";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(SOURCES)
        .join("tests/fixtures")
        .join(name)
}

/// The fixtures this build converts, each holding the survey's known text.
fn converted_fixtures() -> Vec<&'static str> {
    let mut names = vec![
        "survey.pdf",
        "survey.docx",
        "survey.pptx",
        "survey.html",
        "survey.eml",
    ];
    if cfg!(feature = "xberg14") {
        names.push("survey.xlsx");
    }
    names
}

/// `target/<profile>/examples/stand_in_worker`, built by `cargo test`.
fn stand_in() -> PathBuf {
    let exe = std::env::current_exe().unwrap();
    let profile_dir = exe.parent().and_then(Path::parent).unwrap();
    let path = profile_dir
        .join("examples")
        .join(format!("stand_in_worker{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "stand-in worker not built at {}",
        path.display()
    );
    path
}

fn convert_config(worker: &Path) -> ConvertConfig {
    ConvertConfig {
        worker: Some(worker.to_path_buf()),
        ..ConvertConfig::default()
    }
}

fn summarizer(convert: ConvertConfig, cache: Option<&Path>) -> Summarizer {
    let mut config = Config {
        convert,
        // One file at a time, so a failure case runs before the file that
        // proves the next worker works.
        concurrency: Some(1),
        ..Config::default()
    };
    match cache {
        Some(dir) => config.cache.dir = Some(dir.to_path_buf()),
        None => config.cache.enabled = false,
    }
    let converters = Converters::standard(&config);
    Summarizer::new(config, &BackendRegistry::with_builtins(), converters).unwrap()
}

fn batch(s: &Summarizer, files: &[PathBuf]) -> Vec<FileRecord> {
    let mut out = Vec::new();
    s.summarize_batch(files, |r| {
        out.push(r);
        Ok(())
    })
    .unwrap();
    out
}

fn ok(r: &FileRecord) -> &SummaryRecord {
    match r {
        FileRecord::Ok(r) => r,
        FileRecord::Error(e) => panic!("expected ok, got {e:?}"),
    }
}

fn err(r: &FileRecord) -> &ErrorRecord {
    match r {
        FileRecord::Error(e) => e,
        FileRecord::Ok(r) => panic!("expected an error record, got {r:?}"),
    }
}

/// Writes `name` with `bytes` into `dir`.
fn write(dir: &Path, name: &str, bytes: &[u8]) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, bytes).unwrap();
    path
}

/// HTML with `n` paragraphs, for a slow or a large conversion.
fn long_html(n: usize) -> Vec<u8> {
    let mut s = String::from("<html><body>");
    for i in 0..n {
        s.push_str(&format!(
            "<p>Paragraph {i} about the lights on the north pier.</p>"
        ));
    }
    s.push_str("</body></html>");
    s.into_bytes()
}

#[test]
fn every_format_converts_to_its_known_text() {
    let s = summarizer(convert_config(Path::new(REAL)), None);
    let names = converted_fixtures();
    let files: Vec<PathBuf> = names.iter().map(|n| fixture(n)).collect();
    let converters = Converters::standard(s.config());
    for (name, record) in names.iter().zip(batch(&s, &files)) {
        let r = ok(&record);
        assert_eq!(r.backends.converter, HOST_CONVERTER_NAME, "{name}");
        let shown = format!("{} {:?}", r.summary, r.outline);
        assert!(!shown.contains("style_name"), "{name}: {shown}");
        let text = converters.load_text(&fixture(name), 1 << 20).unwrap();
        for want in ["forty two lights", "three were dark"] {
            assert!(
                text.contains(want),
                "{name}: {want:?} missing from {text:?}"
            );
        }
    }
}

#[test]
fn scanned_pdf_is_no_text_extracted() {
    let s = summarizer(convert_config(Path::new(REAL)), None);
    let records = batch(&s, &[fixture("scanned.pdf"), fixture("survey.pdf")]);
    assert_eq!(err(&records[0]).error.kind, "no_text_extracted");
    ok(&records[1]);
}

#[cfg(not(feature = "xberg14"))]
#[test]
fn xlsx_is_unsupported_with_a_reason() {
    let s = summarizer(convert_config(Path::new(REAL)), None);
    let records = batch(&s, &[fixture("survey.xlsx")]);
    let e = err(&records[0]);
    assert_eq!(e.error.kind, "unsupported_format");
    for want in ["XLSX is off", "Xberg 1.4 build"] {
        assert!(e.error.message.contains(want), "{}", e.error.message);
    }
}

#[cfg(unix)]
#[test]
fn worker_from_the_other_build_is_refused_with_both_build_names() {
    use std::os::unix::fs::PermissionsExt;
    let other = if cfg!(feature = "xberg14") {
        ("xberg-1.3.10+w1", "Xberg 1.3.10 build")
    } else {
        ("xberg-1.4.0+w1", "Xberg 1.4 build")
    };
    let dir = tempfile::tempdir().unwrap();
    let script = dir.path().join("other-build-worker");
    std::fs::write(
        &script,
        format!(
            "#!/bin/sh\nexec '{}' --announce '{}'\n",
            stand_in().display(),
            other.0
        ),
    )
    .unwrap();
    std::fs::set_permissions(&script, std::fs::Permissions::from_mode(0o755)).unwrap();
    let files = [write(dir.path(), "a.html", b"hello")];
    let records = batch(&summarizer(convert_config(&script), None), &files);
    let e = err(&records[0]);
    assert_eq!(e.error.kind, "worker_protocol", "{}", e.error.message);
    for want in [other.0, other.1, BUILD_NAME] {
        assert!(
            e.error.message.contains(want),
            "{want}: {}",
            e.error.message
        );
    }
}

#[test]
fn one_worker_process_serves_the_whole_batch() {
    let dir = tempfile::tempdir().unwrap();
    let files: Vec<PathBuf> = (0..5)
        .map(|i| write(dir.path(), &format!("{i}.html"), b"pid"))
        .collect();
    let convert = ConvertConfig {
        workers: 1,
        ..convert_config(&stand_in())
    };
    let s = summarizer(convert, None);
    let summaries: Vec<String> = batch(&s, &files)
        .iter()
        .map(|r| ok(r).summary.clone())
        .collect();
    assert!(summaries[0].starts_with("pid "), "{summaries:?}");
    assert!(
        summaries.iter().all(|p| *p == summaries[0]),
        "{summaries:?}"
    );
}

#[test]
fn crash_is_one_error_record_and_the_batch_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        write(dir.path(), "a.html", b"first file"),
        write(dir.path(), "b.html", b"crash"),
        write(dir.path(), "c.html", b"after the crash"),
    ];
    let convert = ConvertConfig {
        workers: 1,
        ..convert_config(&stand_in())
    };
    let records = batch(&summarizer(convert, None), &files);
    ok(&records[0]);
    let e = err(&records[1]);
    assert_eq!(e.error.kind, "worker_crashed");
    assert!(
        e.stages.iter().any(|s| s.stage == Stage::Convert),
        "convert time is recorded on failure: {:?}",
        e.stages
    );
    assert!(
        e.error.message.contains("stand-in: crashing"),
        "{}",
        e.error.message
    );
    assert!(ok(&records[2]).summary.contains("after the crash"));
}

#[test]
fn hang_is_a_timeout_record_and_the_batch_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        write(dir.path(), "a.html", b"hang"),
        write(dir.path(), "b.html", b"after the hang"),
    ];
    let convert = ConvertConfig {
        workers: 1,
        timeout_ms: 300,
        ..convert_config(&stand_in())
    };
    let records = batch(&summarizer(convert, None), &files);
    let e = err(&records[0]);
    assert_eq!(e.error.kind, "worker_timeout");
    assert!(e.error.message.contains("300 ms"), "{}", e.error.message);
    assert!(ok(&records[1]).summary.contains("after the hang"));
}

#[test]
fn real_worker_over_the_time_limit_is_killed_and_replaced() {
    // About 6 MB of HTML takes the debug worker seconds; the e-mail takes
    // about a millisecond.
    let dir = tempfile::tempdir().unwrap();
    let files = [
        write(dir.path(), "slow.html", &long_html(100_000)),
        fixture("survey.eml"),
    ];
    let convert = ConvertConfig {
        workers: 1,
        timeout_ms: 500,
        ..convert_config(Path::new(REAL))
    };
    let records = batch(&summarizer(convert, None), &files);
    assert_eq!(err(&records[0]).error.kind, "worker_timeout");
    ok(&records[1]);
}

#[test]
fn real_worker_over_the_size_limit_is_killed_and_replaced() {
    let dir = tempfile::tempdir().unwrap();
    let files = [
        write(dir.path(), "big.html", &long_html(2_000)),
        fixture("survey.eml"),
    ];
    let convert = ConvertConfig {
        workers: 1,
        max_output_bytes: 4096,
        ..convert_config(Path::new(REAL))
    };
    let records = batch(&summarizer(convert, None), &files);
    let e = err(&records[0]);
    assert_eq!(e.error.kind, "worker_output_too_large");
    assert!(
        e.error.message.contains("max_output_bytes"),
        "{}",
        e.error.message
    );
    ok(&records[1]);
}

#[test]
fn missing_worker_is_an_error_record() {
    let s = summarizer(
        convert_config(Path::new("/nonexistent/trusty-summarizer-convert")),
        None,
    );
    let records = batch(&s, &[fixture("survey.pdf"), fixture("source.md")]);
    let e = err(&records[0]);
    assert_eq!(e.error.kind, "worker_missing");
    assert!(
        e.error.message.contains("/nonexistent/"),
        "{}",
        e.error.message
    );
    ok(&records[1]);
}

#[test]
fn worker_is_found_beside_the_executable() {
    // The test binary lives in `target/<profile>/deps`; the worker one up.
    if std::env::var_os(trusty_summarizer::worker::WORKER_ENV).is_some() {
        return;
    }
    let found = locate_worker(None).unwrap();
    assert_eq!(
        found.canonicalize().unwrap(),
        Path::new(REAL).canonicalize().unwrap()
    );
}

#[test]
fn outline_ranges_match_read_on_converted_documents() {
    let cache_dir = tempfile::tempdir().unwrap();
    let names = converted_fixtures();
    let files: Vec<PathBuf> = names.iter().map(|n| fixture(n)).collect();
    let s = summarizer(convert_config(Path::new(REAL)), Some(cache_dir.path()));
    let records = batch(&s, &files);

    // `read` with no worker at all: the text must come from the cache.
    let offline = Config {
        convert: convert_config(Path::new("/nonexistent/worker")),
        ..Config::default()
    };
    let converters = Converters::standard(&offline);
    let cache = Cache::open(cache_dir.path()).unwrap();
    for (name, record) in names.iter().zip(&records) {
        let r = ok(record);
        let text = converters
            .load_text_cached(&fixture(name), 1 << 20, Some(&cache))
            .unwrap_or_else(|e| panic!("{name}: {e}"));
        let index = LineIndex::new(&text);
        assert_eq!(index.line_count(), r.lines, "{name}");
        assert!(!r.outline.is_empty(), "{name}");
        let mut next = 1;
        for entry in &r.outline {
            let span = entry.lines.unwrap();
            assert_eq!(span.start, next, "{name}: ranges tile the text");
            let slice = index.slice(span).unwrap();
            let first = slice.lines().find(|l| !l.trim().is_empty()).unwrap_or("");
            assert!(
                first.contains(entry.title.trim_end_matches('…').trim()) || entry.title.is_empty(),
                "{name}: title {:?} not at lines {span:?}: {slice:?}",
                entry.title
            );
            next = span.end + 1;
        }
        assert_eq!(next, r.lines + 1, "{name}");
    }
}

/// The text of `survey.xlsx` as the Xberg 1.4 worker gives it.
#[cfg(feature = "xberg14")]
fn xlsx_text() -> String {
    let s = summarizer(convert_config(Path::new(REAL)), None);
    Converters::standard(s.config())
        .load_text(&fixture("survey.xlsx"), 1 << 20)
        .unwrap()
}

#[cfg(feature = "xberg14")]
#[test]
fn xlsx_converts_to_sheet_and_cell_text() {
    let s = summarizer(convert_config(Path::new(REAL)), None);
    let records = batch(&s, &[fixture("survey.xlsx")]);
    let r = ok(&records[0]);
    assert_eq!(r.format, "xlsx");
    assert_eq!(r.backends.converter, HOST_CONVERTER_NAME);
    let text = xlsx_text();
    eprintln!("survey.xlsx text:\n{text}");
    for want in [
        "Pier",
        "North",
        "42",
        "South",
        "18",
        "Dusk round",
        "18:30",
        "06:15",
        "forty two lights, three were dark",
        "replaced every dark lamp",
    ] {
        assert!(text.contains(want), "{want:?} missing from {text:?}");
    }
    // A row stays on one line, its cells in column order.
    let north = text.lines().find(|l| l.contains("North")).unwrap();
    let (a, b) = (north.find("North").unwrap(), north.find("42").unwrap());
    assert!(a < b, "{north:?}");
}

#[cfg(feature = "xberg14")]
#[test]
fn multi_sheet_xlsx_keeps_sheet_boundaries() {
    let text = xlsx_text();
    let lines: Vec<&str> = text.lines().collect();
    let at = |want: &str| {
        lines
            .iter()
            .position(|l| l.contains(want))
            .unwrap_or_else(|| panic!("{want:?} missing from {text:?}"))
    };
    let (lights, keepers) = (at("Lights"), at("Keepers"));
    // Each sheet name heads its own sheet: on a line of its own, before its
    // cells, and the second sheet starts after the first sheet's last row.
    for (i, name) in [(lights, "Lights"), (keepers, "Keepers")] {
        assert!(
            !lines[i].contains("Pier") && !lines[i].contains("Check"),
            "{name} shares a line with cells: {:?}",
            lines[i]
        );
    }
    assert!(lights < at("North") && at("South") < keepers, "{text:?}");
    assert!(keepers < at("Dusk round"), "{text:?}");
}

#[cfg(feature = "xberg14")]
#[test]
fn malformed_xlsx_is_an_error_record_and_the_pool_goes_on() {
    let dir = tempfile::tempdir().unwrap();
    let good = std::fs::read(fixture("survey.xlsx")).unwrap();
    // Same length, but the middle of the archive overwritten: entries whose
    // compressed data and checksums no longer match.
    let mut corrupt = good.clone();
    let mid = corrupt.len() / 3;
    corrupt[mid..mid + 200].fill(0xA5);
    let files = [
        write(dir.path(), "truncated.xlsx", &good[..good.len() / 2]),
        write(dir.path(), "corrupt.xlsx", &corrupt),
        write(
            dir.path(),
            "not-a-zip.xlsx",
            b"plain words, not a workbook\n",
        ),
        fixture("survey.xlsx"),
        fixture("survey.eml"),
    ];
    let convert = ConvertConfig {
        workers: 1,
        ..convert_config(Path::new(REAL))
    };
    let records = batch(&summarizer(convert, None), &files);
    for (i, name) in ["truncated", "corrupt", "not-a-zip"].iter().enumerate() {
        let e = err(&records[i]);
        assert!(
            e.path.ends_with(&format!("{name}.xlsx")),
            "{name}: {}",
            e.path
        );
        assert_ne!(
            e.error.kind, "unsupported_format",
            "{name}: {}",
            e.error.message
        );
        assert!(!e.error.message.is_empty(), "{name}");
    }
    assert!(!ok(&records[3]).summary.is_empty());
    ok(&records[4]);
}

/// Step 7: `find` answers from the converted text of every format this
/// build converts, plus plain text and Markdown, with a phrase across
/// inflection, a number, and an explicit no-match.
#[test]
fn find_matches_across_formats() {
    use trusty_summarizer::find::{FindOptions, MatchMode, find};
    let s = summarizer(convert_config(Path::new(REAL)), None);
    let converters = Converters::standard(s.config());
    let dir = tempfile::tempdir().unwrap();
    let plain = std::fs::read_to_string(fixture("source.md")).unwrap();
    let mut files: Vec<PathBuf> = converted_fixtures().iter().map(|n| fixture(n)).collect();
    files.push(write(dir.path(), "survey.txt", plain.as_bytes()));
    files.push(fixture("source.md"));
    let opts = FindOptions {
        max_hits: 5,
        context: 1,
        max_line_chars: 120,
    };
    for file in &files {
        let text = converters.load_text(file, 1 << 20).unwrap();
        let r = find(&text, "dark lamps", opts);
        assert_eq!(r.mode, MatchMode::Phrase, "{}: {r:?}", file.display());
        assert!(
            r.hits[0].text.contains("dark lamp"),
            "{}: {r:?}",
            file.display()
        );
        let r = find(&text, "forty-two Lights", opts);
        assert_eq!(r.mode, MatchMode::Phrase, "{}: {r:?}", file.display());
        let r = find(&text, "lighthouse keeper's cat", opts);
        assert_eq!(r.mode, MatchMode::None, "{}: {r:?}", file.display());
        assert!(r.hits.is_empty());
    }
}

/// Converted text of `name` through the real worker.
fn converted(name: &str) -> String {
    let s = summarizer(convert_config(Path::new(REAL)), None);
    Converters::standard(s.config())
        .load_text(&fixture(name), 1 << 20)
        .unwrap()
}

/// A PDF with no table converts to the same text as before table blocks.
#[test]
fn pdf_without_tables_is_unchanged() {
    assert_eq!(
        converted("survey.pdf"),
        "Harbor Lights Survey\n\n\
         The harbor survey counted forty two lights along the north pier.\n\
         Each light was checked at dusk and again at dawn.\n\n\
         Seven lights were dim and three were dark.\n\
         The keeper replaced every dark lamp within one week."
    );
}

/// `tables.pdf` (from `make_table_pdf.py`): a ruled 3x2 grid on page 2 of 3.
#[test]
fn pdf_tables_follow_their_page() {
    assert_eq!(
        converted("tables.pdf"),
        "Pier report opens here.\n\n\
         Lamp counts follow.\n\nLabel Value\n\nLights 42\n\nDark 3\n\n\
         [table page=2 rows=3 cols=2]\n\
         | Label | Value |\n| --- | --- |\n| Lights | 42 |\n| Dark | 3 |\n\
         [/table]\n\n\
         Closing notes end here."
    );
}
