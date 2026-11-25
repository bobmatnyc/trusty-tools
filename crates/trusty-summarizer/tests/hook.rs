//! The `hooks/route-documents` Claude Code `PreToolUse` hook, driven with
//! sample hook JSON on stdin for each row of its decision table.
//!
//! The child's `PATH` holds only a link to the built binary (or nothing),
//! so the result never depends on what the developer has installed.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use serde_json::{Value, json};

/// `hooks/route-documents` in this crate's directory.
fn script() -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR")).join("hooks/route-documents");
    assert!(
        p.is_file(),
        "hooks/route-documents not found at {}",
        p.display()
    );
    p
}

fn python3() -> PathBuf {
    std::env::split_paths(&std::env::var_os("PATH").unwrap_or_default())
        .map(|d| d.join("python3"))
        .find(|p| p.is_file())
        .expect("python3 on PATH")
}

struct Fixture {
    tmp: tempfile::TempDir,
}

impl Fixture {
    fn new() -> Fixture {
        let tmp = tempfile::tempdir().unwrap();
        let with = tmp.path().join("bin-with");
        std::fs::create_dir(&with).unwrap();
        std::os::unix::fs::symlink(
            env!("CARGO_BIN_EXE_trusty-summarizer"),
            with.join("trusty-summarizer"),
        )
        .unwrap();
        std::fs::create_dir(tmp.path().join("bin-without")).unwrap();
        let docs = tmp.path().join("docs");
        std::fs::create_dir(&docs).unwrap();
        for name in [
            "a.pdf", "b.DOCX", "c.pptx", "d.html", "e.htm", "f.eml", "g.xlsx",
        ] {
            std::fs::write(docs.join(name), "x").unwrap();
        }
        // 15,000 tokens is 60,000 characters; one under, one at, the
        // threshold; multi-byte characters count once.
        let line = "é".repeat(99) + "\n"; // 100 characters, 199 bytes
        std::fs::write(docs.join("under.md"), line.repeat(599) + &"y".repeat(96)).unwrap();
        std::fs::write(docs.join("at.md"), line.repeat(600)).unwrap();
        std::fs::write(docs.join("at.txt"), "z".repeat(60_000)).unwrap();
        std::fs::write(docs.join("big.rs"), "// x\n".repeat(40_000)).unwrap();
        Fixture { tmp }
    }

    fn doc(&self, name: &str) -> String {
        self.tmp
            .path()
            .join("docs")
            .join(name)
            .display()
            .to_string()
    }

    fn run(&self, input: &Value, cli_on_path: bool) -> Output {
        let bin = if cli_on_path {
            "bin-with"
        } else {
            "bin-without"
        };
        let mut child = Command::new(python3())
            .arg("-I")
            .arg(script())
            .env("PATH", self.tmp.path().join(bin))
            .current_dir(self.tmp.path())
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .take()
            .unwrap()
            .write_all(input.to_string().as_bytes())
            .unwrap();
        child.wait_with_output().unwrap()
    }
}

/// Hook input as Claude Code sends it for a Read.
fn read_input(file_path: &str) -> Value {
    json!({
        "session_id": "s1",
        "transcript_path": "/tmp/t.jsonl",
        "cwd": "/",
        "hook_event_name": "PreToolUse",
        "tool_name": "Read",
        "tool_input": {"file_path": file_path},
    })
}

/// `None` for allow (exit 0, empty stdout), else the deny reason.
fn decision(out: &Output) -> Option<String> {
    assert_eq!(
        out.status.code(),
        Some(0),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
    if out.stdout.is_empty() {
        return None;
    }
    let v: Value = serde_json::from_slice(&out.stdout).unwrap();
    let h = &v["hookSpecificOutput"];
    assert_eq!(h["hookEventName"], "PreToolUse");
    assert_eq!(h["permissionDecision"], "deny");
    Some(h["permissionDecisionReason"].as_str().unwrap().to_string())
}

#[test]
fn documents_are_routed_and_everything_else_is_read() {
    let f = Fixture::new();
    for name in [
        "a.pdf", "b.DOCX", "c.pptx", "d.html", "e.htm", "f.eml", "at.md", "at.txt",
    ] {
        let why = decision(&f.run(&read_input(&f.doc(name)), true))
            .unwrap_or_else(|| panic!("{name} was allowed"));
        for cmd in ["size", "summarize", "find", "read"] {
            assert!(
                why.contains(&format!("trusty-summarizer {cmd} {}", f.doc(name))),
                "{name}: {why}"
            );
        }
    }
    for name in ["g.xlsx", "under.md", "big.rs", "absent.pdf"] {
        assert_eq!(
            decision(&f.run(&read_input(&f.doc(name)), true)),
            None,
            "{name}"
        );
    }
    // A relative path resolves against the input's `cwd`.
    let mut rel = read_input("docs/a.pdf");
    rel["cwd"] = f.tmp.path().display().to_string().into();
    assert!(decision(&f.run(&rel, true)).is_some());
}

#[test]
fn hook_fails_open() {
    let f = Fixture::new();
    // No CLI on PATH: allowed, with a note on stderr.
    let out = f.run(&read_input(&f.doc("a.pdf")), false);
    assert_eq!(decision(&out), None);
    assert!(String::from_utf8_lossy(&out.stderr).contains("not on PATH"));
    // Another tool, bad JSON, missing fields: allowed.
    let mut other = read_input(&f.doc("a.pdf"));
    other["tool_name"] = "Write".into();
    assert_eq!(decision(&f.run(&other, true)), None);
    assert_eq!(decision(&f.run(&json!({"tool_name": "Read"}), true)), None);
    assert_eq!(decision(&f.run(&json!("not an object"), true)), None);
    let mut child = Command::new(python3())
        .arg("-I")
        .arg(script())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    child.stdin.take().unwrap().write_all(b"{not json").unwrap();
    let out = child.wait_with_output().unwrap();
    assert_eq!(decision(&out), None);
    assert!(String::from_utf8_lossy(&out.stderr).contains("allowing Read"));
}

#[test]
fn huge_text_is_denied_without_reading_it() {
    let f = Fixture::new();
    // A sparse 32 GiB file: no data on disk, so a whole-file read stalls or
    // exhausts memory. The child gets a deadline and is killed after it.
    let huge = f.tmp.path().join("docs/huge.md");
    std::fs::File::create(&huge)
        .unwrap()
        .set_len(32 << 30)
        .unwrap();
    let mut child = Command::new(python3())
        .arg("-I")
        .arg(script())
        .env("PATH", f.tmp.path().join("bin-with"))
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let input = read_input(&huge.display().to_string()).to_string();
    child
        .stdin
        .take()
        .unwrap()
        .write_all(input.as_bytes())
        .unwrap();
    let start = std::time::Instant::now();
    while child.try_wait().unwrap().is_none() {
        if start.elapsed().as_secs() >= 3 {
            child.kill().unwrap();
            child.wait().unwrap();
            panic!("hook still running after 3 s: it is reading the file");
        }
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    assert!(decision(&child.wait_with_output().unwrap()).is_some());
}

#[test]
fn symlink_is_judged_by_its_target() {
    let f = Fixture::new();
    let docs = f.tmp.path().join("docs");
    for (link, target) in [("l.rs", "a.pdf"), ("l.txt", "at.md"), ("l2.rs", "at.txt")] {
        std::os::unix::fs::symlink(docs.join(target), docs.join(link)).unwrap();
        assert!(
            decision(&f.run(&read_input(&f.doc(link)), true)).is_some(),
            "{link} -> {target} was allowed"
        );
    }
    std::os::unix::fs::symlink(docs.join("g.xlsx"), docs.join("ok.md")).unwrap();
    assert_eq!(decision(&f.run(&read_input(&f.doc("ok.md")), true)), None);
}

/// A Read with `offset`/`limit`/`pages` of a routed document is denied too,
/// and the reason gives the exact `read --lines` command for the requested
/// range (pilot finding).
#[test]
fn partial_reads_are_denied_with_the_exact_range() {
    let f = Fixture::new();
    let md = f.doc("at.md");
    for (extra, range) in [
        (json!({"offset": 24, "limit": 210}), Some("24-233")),
        (json!({"limit": 50}), Some("1-50")),
        (json!({"offset": 100}), None),
    ] {
        let mut input = read_input(&md);
        for (k, v) in extra.as_object().unwrap() {
            input["tool_input"][k] = v.clone();
        }
        let why = decision(&f.run(&input, true)).unwrap_or_else(|| panic!("{extra} allowed"));
        if let Some(r) = range {
            assert!(
                why.contains(&format!("trusty-summarizer read {md} --lines {r}")),
                "{extra}: {why}"
            );
        }
    }
    let mut pdf = read_input(&f.doc("a.pdf"));
    pdf["tool_input"]["pages"] = "1-3".into();
    let why = decision(&f.run(&pdf, true)).expect("pdf with pages allowed");
    assert!(why.contains("trusty-summarizer size"), "{why}");
}
