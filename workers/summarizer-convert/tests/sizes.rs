//! `text_chars` and `tokens_est` on a converted document, through the
//! `trusty-summarizer` binary warm and `--no-warm`, fresh and cached, and
//! from the `size` and `miss` commands. No network, no model.
//!
//! The binary is `trusty-summarizer`, built into this worker workspace's
//! target directory by `cargo build -p trusty-summarizer` (the path
//! dependency's binary); it finds the worker next to itself.

use std::path::{Path, PathBuf};
use std::process::{Command, Output};

use serde_json::Value;
use trusty_summarizer::{Config, ConvertConfig, Converters};

const REAL: &str = env!("CARGO_BIN_EXE_trusty-summarizer-convert");

const SOURCES: &str = ".";

fn fixture(name: &str) -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join(SOURCES)
        .join("tests/fixtures")
        .join(name)
}

fn summarizer_bin() -> PathBuf {
    let path = Path::new(REAL)
        .with_file_name(format!("trusty-summarizer{}", std::env::consts::EXE_SUFFIX));
    assert!(
        path.is_file(),
        "trusty-summarizer not built at {}: run `cargo build -p trusty-summarizer` in workers/summarizer-convert",
        path.display()
    );
    path
}

fn lines(out: &Output) -> Vec<Value> {
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout.clone())
        .unwrap()
        .lines()
        .map(|l| serde_json::from_str(l).unwrap())
        .collect()
}

#[test]
fn size_fields_match_warm_and_no_warm_on_converted_documents() {
    let tmp = tempfile::tempdir().unwrap();
    let cache = tmp.path().join("cache");
    let config = tmp.path().join("config.toml");
    std::fs::write(
        &config,
        format!(
            "[cache]\ndir = {:?}\n[convert]\nworker = {REAL:?}\n",
            cache.display().to_string()
        ),
    )
    .unwrap();
    let run = |args: &[&str]| {
        Command::new(summarizer_bin())
            .current_dir(tmp.path())
            .env_remove("TRUSTY_SUMMARIZER_CONFIG")
            .env_remove("TRUSTY_SUMMARIZER_CACHE_DIR")
            .env("TRUSTY_SUMMARIZER_RUNTIME_DIR", tmp.path().join("run"))
            .env(
                "TRUSTY_SUMMARIZER_MISS_LOG",
                tmp.path().join("misses.jsonl"),
            )
            .arg("--config")
            .arg(&config)
            .args(args)
            .output()
            .unwrap()
    };

    for name in ["survey.pdf", "survey.docx"] {
        let doc = fixture(name);
        let doc_s = doc.to_str().unwrap();
        let text = Converters::standard(&Config {
            convert: ConvertConfig {
                worker: Some(PathBuf::from(REAL)),
                ..ConvertConfig::default()
            },
            ..Config::default()
        })
        .load_text(&doc, 1 << 20)
        .unwrap();
        let chars = text.chars().count() as u64;
        let bytes = std::fs::metadata(&doc).unwrap().len();
        assert_ne!(chars, bytes, "{name}: text size differs from file size");

        // A miss before any summarize: the size is not known yet.
        lines(&run(&["miss", doc_s, "--question", "q"]));

        let mut caches = Vec::new();
        for args in [
            &["summarize", "--no-warm", "--stats", doc_s][..],
            &["summarize", "--no-warm", "--stats", doc_s],
            &["summarize", "--stats", doc_s],
            &["summarize", "--no-cache", "--stats", doc_s],
        ] {
            let r = lines(&run(args)).remove(0);
            assert_eq!(r["status"], "ok", "{name} {args:?}: {r}");
            assert_eq!(r["text_chars"], chars, "{name} {args:?}");
            assert_eq!(r["tokens_est"], chars.div_ceil(4), "{name} {args:?}");
            caches.push(r["cache"].as_str().unwrap().to_string());
        }
        assert_eq!(caches, ["miss", "hit", "hit", "disabled"], "{name}");

        let s = lines(&run(&["size", doc_s])).remove(0);
        assert_eq!(s["text_chars"], chars, "{name}");
        assert_eq!(s["tokens_est"], chars.div_ceil(4), "{name}");
        assert_eq!(s["format"], name.rsplit('.').next().unwrap());

        // After a summarize, the cached text gives the miss its size.
        lines(&run(&["miss", doc_s, "--question", "q"]));
        let log = std::fs::read_to_string(tmp.path().join("misses.jsonl")).unwrap();
        let entries: Vec<Value> = log
            .lines()
            .map(|l| serde_json::from_str(l).unwrap())
            .collect();
        let (before, after) = (&entries[entries.len() - 2], &entries[entries.len() - 1]);
        assert_eq!(before["text_chars"], Value::Null, "{name}");
        assert_eq!(after["text_chars"], chars, "{name}");
        assert_eq!(after["tokens_est"], chars.div_ceil(4), "{name}");
        assert_eq!(after["format"], s["format"]);
        assert_eq!(after["converter"], s["converter"]);
    }
    let _ = run(&["server", "stop"]);
}
