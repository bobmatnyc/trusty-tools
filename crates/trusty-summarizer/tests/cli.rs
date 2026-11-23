//! End-to-end tests of the built `trusty-summarizer` binary, in-process
//! (`--no-warm`); the warm server has its own tests in `warm.rs`.

use std::path::Path;
use std::process::{Command, Output};

use serde_json::Value;

fn bin(config: &Path) -> Command {
    let mut c = Command::new(env!("CARGO_BIN_EXE_trusty-summarizer"));
    c.env_remove("TRUSTY_SUMMARIZER_CONFIG")
        .env_remove("TRUSTY_SUMMARIZER_CACHE_DIR")
        // Keep the developer's real usage log clean.
        .env(
            "TRUSTY_SUMMARIZER_USAGE_LOG",
            config.with_file_name("usage.jsonl"),
        )
        .arg("--config")
        .arg(config);
    c
}

fn records(out: &Output) -> Vec<Value> {
    String::from_utf8(out.stdout.clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).expect("each line is JSON"))
        .collect()
}

struct Fixture {
    _tmp: tempfile::TempDir,
    config: std::path::PathBuf,
    cache: std::path::PathBuf,
    docs: std::path::PathBuf,
}

fn fixture() -> Fixture {
    let tmp = tempfile::tempdir().unwrap();
    let config = tmp.path().join("config.toml");
    std::fs::write(&config, "[outline]\nmax_section_lines = 4\n").unwrap();
    let docs = tmp.path().join("docs");
    std::fs::create_dir_all(docs.join("nested")).unwrap();
    let mut md = String::from("# Guide\n\nThe guide explains the summarizer cache.\n\n");
    for i in 1..=6 {
        md.push_str(&format!(
            "## Part {i}\n\nPart {i} covers topic number {i} in detail.\n\n"
        ));
    }
    std::fs::write(docs.join("guide.md"), md).unwrap();
    std::fs::write(
        docs.join("nested/notes.txt"),
        "line one\nline two\n\nline four\n",
    )
    .unwrap();
    Fixture {
        cache: tmp.path().join("cache"),
        _tmp: tmp,
        config,
        docs,
    }
}

#[test]
fn summarize_writes_one_record_per_file_and_read_returns_ranges() {
    let f = fixture();
    let out = bin(&f.config)
        .args(["summarize", "--no-warm", "--stats"])
        .arg("--cache-dir")
        .arg(&f.cache)
        .arg(&f.docs)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let recs = records(&out);
    assert_eq!(recs.len(), 2);

    let guide = &recs[0];
    assert!(guide["path"].as_str().unwrap().ends_with("guide.md"));
    for field in [
        "hash", "bytes", "lines", "class", "summary", "outline", "backends", "stages",
    ] {
        assert!(!guide[field].is_null(), "missing {field}");
    }
    assert_eq!(guide["status"], "ok");
    assert_eq!(guide["cache"], "miss");
    assert_eq!(guide["class"]["status"], "no_classifier_configured");
    assert_eq!(guide["backends"]["summary"], "extractive-tf");

    // Ranges tile the document, and `read` prints exactly those lines.
    let path = guide["path"].as_str().unwrap();
    let text = std::fs::read_to_string(path).unwrap();
    let all: Vec<&str> = text.split_inclusive('\n').collect();
    let outline = guide["outline"].as_array().unwrap();
    assert!(outline.len() > 1);
    let mut next = 1;
    for topic in outline {
        let (a, b) = (
            topic["lines"][0].as_u64().unwrap() as usize,
            topic["lines"][1].as_u64().unwrap() as usize,
        );
        assert_eq!(a, next);
        next = b + 1;
        let read = bin(&f.config)
            .args(["read", "--lines", &format!("{a}-{b}"), path])
            .output()
            .unwrap();
        assert!(read.status.success());
        assert_eq!(
            String::from_utf8(read.stdout).unwrap(),
            all[a - 1..b].concat()
        );
    }
    assert_eq!(next, all.len() + 1);

    // Second run: everything from cache.
    let again = bin(&f.config)
        .args(["summarize", "--no-warm", "--stats"])
        .arg("--cache-dir")
        .arg(&f.cache)
        .arg(&f.docs)
        .output()
        .unwrap();
    assert!(records(&again).iter().all(|r| r["cache"] == "hit"));
}

#[test]
fn no_references_drops_ranges() {
    let f = fixture();
    let out = bin(&f.config)
        .args([
            "summarize",
            "--no-warm",
            "--no-cache",
            "--no-references",
            "--stats",
        ])
        .arg(&f.docs)
        .output()
        .unwrap();
    assert!(out.status.success());
    for rec in records(&out) {
        assert_eq!(rec["cache"], "disabled");
        for topic in rec["outline"].as_array().unwrap() {
            assert!(topic.get("lines").is_none() && topic.get("paragraphs").is_none());
            assert!(topic["title"].is_string());
        }
    }
}

#[test]
fn bad_file_gives_error_record_and_exit_2() {
    let f = fixture();
    // ODT has no converter in either build (XLSX has one in the Xberg 1.4 build).
    std::fs::write(f.docs.join("report.odt"), b"PK\x03\x04").unwrap();
    let out = bin(&f.config)
        .args(["summarize", "--no-warm", "--no-cache"])
        .arg(&f.docs)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let recs = records(&out);
    assert_eq!(recs.len(), 3);
    let err = recs.iter().find(|r| r["status"] == "error").unwrap();
    assert_eq!(err["error"]["kind"], "unsupported_format");
    assert_eq!(err["error"]["format"], "odt");
}

#[test]
fn missing_worker_gives_worker_missing_record() {
    let f = fixture();
    std::fs::write(f.docs.join("report.pdf"), b"%PDF-1.4\n").unwrap();
    let out = bin(&f.config)
        .env(
            "TRUSTY_SUMMARIZER_CONVERT",
            "/nonexistent/trusty-summarizer-convert",
        )
        .args(["summarize", "--no-warm", "--no-cache"])
        .arg(&f.docs)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2));
    let recs = records(&out);
    assert_eq!(recs.len(), 3);
    let err = recs.iter().find(|r| r["status"] == "error").unwrap();
    assert_eq!(err["error"]["kind"], "worker_missing");
    assert!(
        err["error"]["message"]
            .as_str()
            .unwrap()
            .contains("/nonexistent/")
    );
}

/// An end past the last line is served to the last line, with a note on
/// stderr; the 21 `invalid_range` errors in the 2026-11-18 pilot log were all
/// this case.
#[test]
fn read_clamps_an_end_past_the_last_line() {
    let f = fixture();
    let file = f.docs.join("nested/notes.txt");
    let total = std::fs::read_to_string(&file).unwrap().lines().count();
    let out = bin(&f.config)
        .args(["read", "--lines", &format!("3-{}", total + 96)])
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let exact = bin(&f.config)
        .args(["read", "--lines", &format!("3-{total}")])
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(out.stdout, exact.stdout);
    assert!(String::from_utf8_lossy(&out.stderr).contains(&format!("served 3-{total}")));
}

#[test]
fn read_rejects_out_of_range() {
    let f = fixture();
    let out = bin(&f.config)
        .args(["read", "--lines", "98-99"])
        .arg(f.docs.join("nested/notes.txt"))
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
    assert!(String::from_utf8_lossy(&out.stderr).contains("invalid line range"));
}

/// Step 7: `find` prints one JSON object with bounded hits, and `find`
/// can be turned off.
#[test]
fn find_prints_hits_and_can_be_turned_off() {
    let f = fixture();
    let file = f.docs.join("guide.md");
    let out = bin(&f.config)
        .args(["find", "--query", "topic numbers 4", "--max-hits", "1"])
        .arg(&file)
        .output()
        .unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    assert_eq!(v["mode"], "phrase");
    // B2: the one phrase hit first; lines with some of the words follow.
    assert!(v["total"].as_u64().unwrap() > 1);
    assert_eq!(v["hits"][0]["line"], 19);
    assert_eq!(v["hits"][0]["match"], "phrase");
    let off = f.config.with_file_name("off.toml");
    std::fs::write(&off, "[find]\nenabled = false\n").unwrap();
    let out = bin(&off)
        .args(["find", "--query", "topic"])
        .arg(&file)
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(1));
}

#[test]
fn outline_max_caps_the_outline_and_reports_the_omitted_count() {
    let f = fixture();
    let guide = f.docs.join("guide.md");
    let run = |extra: &[&str]| {
        let out = bin(&f.config)
            .args(["summarize", "--no-warm", "--no-cache"])
            .args(extra)
            .arg(&guide)
            .output()
            .unwrap();
        assert!(out.status.success());
        records(&out).remove(0)
    };
    let full = run(&[]);
    let n = full["outline"].as_array().unwrap().len();
    assert!(n > 2);
    assert!(full.get("outline_omitted").is_none());
    let capped = run(&["--outline-max", "2"]);
    assert_eq!(capped["outline"].as_array().unwrap().len(), 2);
    assert_eq!(capped["outline_omitted"]["entries"], n - 2);
    let last = full["outline"][n - 1]["lines"][1].clone();
    assert_eq!(capped["outline_omitted"]["lines"][1], last);
}

/// B5: without `--stats`, fresh and cached output are the same bytes.
#[test]
fn fresh_and_cached_output_are_byte_identical() {
    let f = fixture();
    let run = || {
        let out = bin(&f.config)
            .args(["summarize", "--no-warm"])
            .arg("--cache-dir")
            .arg(&f.cache)
            .arg(&f.docs)
            .output()
            .unwrap();
        assert!(out.status.success());
        out.stdout
    };
    let fresh = run();
    let cached = run();
    assert!(!fresh.is_empty());
    assert_eq!(
        String::from_utf8(fresh).unwrap(),
        String::from_utf8(cached).unwrap()
    );
}
