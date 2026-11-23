//! Rules and the trusty-assistants contract (#11747).
//!
//! The contract tests read trusty-assistants' sources at test time, the way
//! `analyze_uds::tests` reads trusty-analyze's: the console has no Cargo edge
//! on it (ruling Df0), so a changed declaration there fails here instead.

use std::path::{Path, PathBuf};

use serde_json::json;

use super::routes::{DEFAULT_TURN_LIMIT, MAX_TURN_LIMIT, parse_limit};
use super::turns::{TURN_LOG_FILE, TURN_RECORD_VERSION, read_tail, read_tail_window};
use super::*;

/// `crates/<rel>` in the running checkout.
fn source(rel: &str) -> (PathBuf, String) {
    // #9298: the running checkout, not the one that built this binary.
    let path = trusty_common::test_harness::test_repo_root()
        .expect("the running checkout's workspace root")
        .join("crates")
        .join(rel);
    let text =
        std::fs::read_to_string(&path).unwrap_or_else(|e| panic!("read {}: {e}", path.display()));
    (path, text)
}

/// Assert `needle` appears in the source at `rel`.
fn declares(rel: &str, needle: &str) {
    let (path, text) = source(rel);
    assert!(
        text.contains(needle),
        "`{needle}` is not in {} — trusty-assistants changed it; update the console mirror",
        path.display()
    );
}

/// The `pub <field>: <type>` lines of `pub struct <name> {…}` in `text`.
fn struct_fields(path: &Path, text: &str, name: &str) -> Vec<String> {
    let start = text
        .find(&format!("pub struct {name} {{"))
        .unwrap_or_else(|| panic!("no `pub struct {name}` in {}", path.display()));
    let body = &text[start..];
    let end = body.find("\n}").expect("struct closes");
    body[..end]
        .lines()
        .map(str::trim)
        .filter_map(|l| l.strip_prefix("pub "))
        .filter(|l| !l.starts_with("struct"))
        .map(|l| l.trim_end_matches(',').to_owned())
        .collect()
}

const DAEMON: &str = "trusty-assistants/src/ask/daemon/mod.rs";
const TURN_LOG: &str = "trusty-assistants/src/ask/turn_log.rs";
const HOME: &str = "trusty-assistants/src/assistants/home.rs";
const INSTANCE: &str = "trusty-assistants/src/assistants/instance.rs";
const HEALTH: &str = "trusty-agents-common/src/health.rs";

#[test]
fn the_mirrored_constants_match_the_source() {
    declares(
        DAEMON,
        &format!("pub const PROTOCOL_VERSION: u32 = {PROTOCOL_VERSION};"),
    );
    declares(
        HOME,
        &format!("pub const ASSISTANTS_DIR_ENV: &str = \"{ASSISTANTS_DIR_ENV}\";"),
    );
    declares(
        HOME,
        &format!("pub const LEGACY_ASSISTANTS_DIR_ENV: &str = \"{LEGACY_ASSISTANTS_DIR_ENV}\";"),
    );
    declares(
        HOME,
        &format!("pub const ASSISTANTS_DIR_NAME: &str = \"{ASSISTANTS_DIR_NAME}\";"),
    );
    declares(
        HOME,
        &format!("pub const INSTRUCTIONS_FILE: &str = \"{INSTRUCTIONS_FILE}\";"),
    );
    declares(
        HOME,
        &format!("pub const STORES_DIR: &str = \"{}\";", turns::STORES_DIR),
    );
    declares(
        TURN_LOG,
        &format!("pub const TURN_LOG_FILE: &str = \"{TURN_LOG_FILE}\";"),
    );
    declares(
        TURN_LOG,
        &format!("pub const TURN_RECORD_VERSION: u32 = {TURN_RECORD_VERSION};"),
    );
    declares(
        HEALTH,
        &format!("pub const AGENTS_APP_NAME: &str = \"{AGENTS_APP_NAME}\";"),
    );
    declares(
        INSTANCE,
        &format!("const MAX_ID_LEN: usize = {MAX_ID_LEN};"),
    );
    declares(
        INSTANCE,
        "c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_' | '.')",
    );
    // #11748: the reply cap is the daemon's own line cap, 1 MiB.
    assert_eq!(ask::MAX_REPLY_BYTES, 1024 * 1024);
    declares(DAEMON, "pub const MAX_LINE_BYTES: u64 = 1024 * 1024;");
    // #11748: the client keeps its write half open; EOF cancels the ask.
    declares(DAEMON, "keeps its write half OPEN");
}

#[test]
fn the_wire_structs_match_the_source() {
    let (path, text) = source(DAEMON);
    assert_eq!(
        struct_fields(&path, &text, "AskRequest"),
        ["protocol: u32", "question: String"]
    );
    assert_eq!(
        struct_fields(&path, &text, "AskReply"),
        ["protocol: u32", "daemon_pid: u32", "outcome: ReplyOutcome"]
    );
    for needle in [
        "#[serde(tag = \"outcome\", rename_all = \"snake_case\")]\npub enum ReplyOutcome {",
        "Answer { answer: String },",
        "kind: ReplyErrorKind,\n        message: String,",
        "#[serde(rename_all = \"snake_case\")]\npub enum ReplyErrorKind {",
        "    ProtocolMismatch,\n",
        "    BadRequest,\n",
        "    Ask,\n}",
        "data_dir.join(format!(\"trusty-agents-{id}.sock\"))",
    ] {
        assert!(
            text.contains(needle),
            "`{needle}` is not in {}",
            path.display()
        );
    }
}

#[test]
fn the_turn_record_fields_match_the_source() {
    let (path, text) = source(TURN_LOG);
    assert!(text.contains("#[serde(deny_unknown_fields)]\npub struct TurnRecord {"));
    assert_eq!(
        struct_fields(&path, &text, "TurnRecord"),
        ["v: u32", "at: String", "question: String", "answer: String"]
    );
}

/// AC4: the JSON shape is pinned as literals; a change on either side fails.
#[test]
fn the_wire_shapes_are_pinned() {
    assert_eq!(PROTOCOL_VERSION, 1);
    let req = AskRequest {
        protocol: PROTOCOL_VERSION,
        question: "q".into(),
    };
    assert_eq!(
        serde_json::to_value(&req).expect("ser"),
        json!({"protocol": 1, "question": "q"})
    );
    let answer = AskReply {
        protocol: 1,
        daemon_pid: 7,
        outcome: ReplyOutcome::Answer { answer: "a".into() },
    };
    assert_eq!(
        serde_json::to_value(&answer).expect("ser"),
        json!({"protocol": 1, "daemon_pid": 7, "outcome": "answer", "answer": "a"})
    );
    let refused: AskReply = serde_json::from_value(json!({
        "protocol": 1, "daemon_pid": 7, "outcome": "error",
        "kind": "protocol_mismatch", "message": "m"
    }))
    .expect("de");
    assert_eq!(
        refused.outcome,
        ReplyOutcome::Error {
            kind: ReplyErrorKind::ProtocolMismatch,
            message: "m".into()
        }
    );
    for (kind, text) in [
        (ReplyErrorKind::BadRequest, "bad_request"),
        (ReplyErrorKind::Ask, "ask"),
    ] {
        assert_eq!(serde_json::to_value(kind).expect("ser"), json!(text));
    }
}

#[test]
fn the_socket_path_follows_the_ta_spelling() {
    let id = InstanceId::parse("izzie").expect("valid");
    assert_eq!(
        socket_path_in(Path::new("/data"), &id),
        Path::new("/data/trusty-agents-izzie.sock")
    );
}

#[test]
fn instance_ids_follow_the_ta_rule() {
    for ok in ["izzie", "bob-kb", "a.b_c-1", "x"] {
        assert!(InstanceId::parse(ok).is_ok(), "{ok}");
    }
    let long = "a".repeat(65);
    for bad in [
        "",
        ".",
        "..",
        "../etc",
        "a/b",
        "a\\b",
        "OKG",
        "Bob",
        " izzie",
        "a b",
        "---",
        "a%2f",
        long.as_str(),
    ] {
        assert!(InstanceId::parse(bad).is_err(), "{bad:?} must be refused");
    }
}

#[test]
fn the_root_resolves_like_ta() {
    let env = |pairs: &'static [(&'static str, &'static str)]| {
        move |k: &str| {
            pairs
                .iter()
                .find(|(key, _)| *key == k)
                .map(|(_, v)| std::ffi::OsString::from(v))
        }
    };
    assert_eq!(
        root_from(env(&[
            ("TA_ASSISTANTS_DIR", "/a"),
            ("TAGENT_ASSISTANTS_DIR", "/b"),
            ("HOME", "/h")
        ])),
        Ok(PathBuf::from("/a"))
    );
    assert_eq!(
        root_from(env(&[
            ("TA_ASSISTANTS_DIR", ""),
            ("TAGENT_ASSISTANTS_DIR", "/b")
        ])),
        Ok(PathBuf::from("/b"))
    );
    assert_eq!(
        root_from(env(&[("HOME", "/h")])),
        Ok(PathBuf::from("/h/trusty-agents"))
    );
    assert!(root_from(env(&[])).is_err());
}

#[test]
fn limit_defaults_clamps_and_refuses() {
    assert_eq!(parse_limit(None), Ok(DEFAULT_TURN_LIMIT));
    assert_eq!(parse_limit(Some("5")), Ok(5));
    assert_eq!(parse_limit(Some("100000")), Ok(MAX_TURN_LIMIT));
    assert!(parse_limit(Some("0")).is_err());
    assert!(parse_limit(Some("-1")).is_err());
    assert!(parse_limit(Some("ten")).is_err());
}

fn line(n: usize) -> String {
    format!(r#"{{"v":1,"at":"t{n}","question":"q{n}","answer":"a{n}"}}"#)
}

#[test]
fn the_tail_is_the_newest_records() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join(TURN_LOG_FILE);
    let body: String = (1..=5).map(|n| line(n) + "\n").collect();
    std::fs::write(&log, body).expect("write");
    let tail = read_tail(&log, 2).expect("read");
    let ats: Vec<_> = tail.turns.iter().map(|t| t.at.as_str()).collect();
    assert_eq!(ats, ["t4", "t5"]);
    assert_eq!((tail.total, tail.skipped), (5, 0));
    let missing = read_tail(&dir.path().join("absent.jsonl"), 2).expect("read");
    assert_eq!(
        (missing.turns.len(), missing.total, missing.skipped),
        (0, 0, 0)
    );
}

#[test]
fn a_bad_line_is_counted_not_fatal() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join(TURN_LOG_FILE);
    let body = format!(
        "{}\nnot json\n\n{}\n{}\n{}",
        line(1),
        r#"{"v":2,"at":"x","question":"q","answer":"a"}"#,
        r#"{"v":1,"at":"x","question":"q","answer":"a","extra":1}"#,
        line(2),
    );
    let mut bytes = body.into_bytes();
    bytes.extend_from_slice(b"\n\xff\xfe\n");
    std::fs::write(&log, bytes).expect("write");
    let tail = read_tail(&log, 10).expect("read");
    let ats: Vec<_> = tail.turns.iter().map(|t| t.at.as_str()).collect();
    assert_eq!(ats, ["t1", "t2"]);
    assert_eq!((tail.total, tail.skipped), (2, 5));
}

/// #11747: a cut window drops its first, partial line uncounted, and a window
/// starting exactly on a line boundary keeps that line.
#[test]
fn the_window_drops_its_first_partial_line() {
    let dir = tempfile::tempdir().expect("tempdir");
    let log = dir.path().join(TURN_LOG_FILE);
    let body: String = (1..=9).map(|n| line(n) + "\n").collect();
    let width = (line(1).len() + 1) as u64;
    std::fs::write(&log, &body).expect("write");

    let ats = |t: &super::turns::TurnTail| -> Vec<String> {
        t.turns.iter().map(|t| t.at.clone()).collect()
    };
    let mid = read_tail_window(&log, 10, 2 * width + 5).expect("read");
    assert_eq!(ats(&mid), ["t8", "t9"]);
    assert_eq!((mid.total, mid.skipped, mid.truncated), (2, 0, true));

    let exact = read_tail_window(&log, 10, 3 * width).expect("read");
    assert_eq!(ats(&exact), ["t7", "t8", "t9"]);
    assert_eq!((exact.total, exact.skipped, exact.truncated), (3, 0, true));

    let tiny = read_tail_window(&log, 10, 4).expect("read");
    assert_eq!((tiny.total, tiny.skipped, tiny.truncated), (0, 0, true));

    let whole = read_tail_window(&log, 10, 9 * width).expect("read");
    assert_eq!((whole.total, whole.skipped, whole.truncated), (9, 0, false));
}
