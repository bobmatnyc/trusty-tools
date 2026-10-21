//! Tests for `tm dbatg`: parsing, the daemon-down fallback and its
//! own-store rule, and the prototype's stdout.

use std::path::{Path, PathBuf};

use clap::Parser;
use serde_json::Value;
use trusty_architect::ledger::init_store;

use super::{Ctx, Outcome, execute};
use crate::cli::{Cli, Command, DbatgArgs, DbatgVerb};

/// A project with store `pm-a` (code `th`) and goal `th-G01`.
fn project() -> (tempfile::TempDir, PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let root = std::fs::canonicalize(dir.path()).expect("canonical");
    init_store(
        &root.join("codes.json"),
        &root,
        "pm-a",
        "th",
        "2026-10-21T09:00Z",
        &|_| true,
    )
    .expect("init");
    (dir, root)
}

fn args(argv: &[&str]) -> DbatgArgs {
    let full: Vec<&str> = ["tm", "dbatg"].iter().chain(argv).copied().collect();
    match Cli::try_parse_from(full).expect("parse").command {
        Some(Command::Dbatg(a)) => a,
        other => panic!("not dbatg: {other:?}"),
    }
}

/// Runs `argv` with no daemon at the socket, as session `own`.
async fn offline(root: &Path, own: Option<&str>, argv: &[&str], stdin: &str) -> Outcome {
    let mut input = stdin.as_bytes();
    let ctx = Ctx {
        socket: root.join("no-daemon.sock"),
        cwd: root.to_path_buf(),
        own_session: own.map(str::to_owned),
        stdin: &mut input,
    };
    execute(args(argv), ctx).await
}

fn state(root: &Path) -> Value {
    let text =
        std::fs::read_to_string(root.join(".trusty-mpm/dbatg/pm-a/state.json")).expect("state");
    serde_json::from_str(&text).expect("json")
}

#[test]
fn cli_parses_every_dbatg_verb() {
    let cases: [&[&str]; 7] = [
        &["init", "--code", "th"],
        &["add", "T", "--title", "t", "--goal", "th-G01"],
        &["update", "th-T01", "--status", "running", "--rank", "2"],
        &["close", "th-T01", "--outcome", "merged"],
        &["--session", "pm-a", "batch", "--file", "ops.json"],
        &["prune", "--dry-run", "--answered-hours", "48"],
        &["show", "--brief"],
    ];
    for argv in cases {
        let a = args(argv);
        let ok = matches!(
            a.verb,
            DbatgVerb::Init { .. }
                | DbatgVerb::Add { .. }
                | DbatgVerb::Update { .. }
                | DbatgVerb::Close { .. }
                | DbatgVerb::Batch { .. }
                | DbatgVerb::Prune { .. }
                | DbatgVerb::Show { .. }
        );
        assert!(ok, "{argv:?}");
    }
}

/// The amendment: with no daemon, `add` writes once, in-process, and prints
/// the prototype's stdout; each verb keeps its exit code.
#[tokio::test]
async fn dbatg_cli_falls_back_in_process_when_the_daemon_is_down() {
    let (_dir, root) = project();
    let own = Some("pm-a");
    let out = offline(&root, own, &["add", "G", "--title", "lorem"], "").await;
    assert_eq!((out.stdout.as_str(), out.error), ("th-G01\n", None));
    let out = offline(
        &root,
        own,
        &[
            "add", "D", "--title", "q", "--goal", "th-G01", "--opts", "a*,b",
        ],
        "",
    )
    .await;
    assert_eq!(out.stdout, "th-D01\n");
    assert_eq!(
        state(&root)["items"][0]["opts"],
        serde_json::json!([{"l": "a", "rec": true}, {"l": "b", "rec": false}])
    );
    let out = offline(&root, own, &["batch"], r#"[{"op":"update","id":"th-D01","answer":"a"},{"op":"close","id":"th-D01","outcome":"done"}]"#).await;
    assert_eq!(out.stdout, "th-D01, th-D01\n");
    let out = offline(&root, own, &["prune"], "").await;
    assert_eq!(out.stdout, "nothing to prune\n");
    let out = offline(&root, own, &["show", "--brief"], "").await;
    assert_eq!(out.stdout, "TOP th-G01 lorem\n");
    let out = offline(
        &root,
        own,
        &["add", "T", "--title", "t", "--goal", "th-G09"],
        "",
    )
    .await;
    assert_eq!((out.stdout.as_str(), out.error.map(|e| e.0)), ("", Some(2)));
    let out = offline(&root, own, &["batch"], "not json").await;
    assert_eq!(out.error.map(|e| e.0), Some(2));
    assert_eq!(state(&root)["v"], 3);
}

/// The fallback writes only the caller's own store: another session's name
/// or another project is refused with exit 3 and nothing written, while a
/// read still answers.
#[tokio::test]
async fn dbatg_cli_fallback_refuses_another_sessions_store() {
    let (_dir, root) = project();
    let before = state(&root);
    let out = offline(
        &root,
        Some("pm-b"),
        &["--session", "pm-a", "add", "G", "--title", "x"],
        "",
    )
    .await;
    let (code, msg) = out.error.expect("refused");
    assert_eq!(code, 3, "{msg}");
    assert!(msg.contains("own store"), "{msg}");
    assert_eq!(state(&root), before);
    let out = offline(
        &root,
        Some("pm-b"),
        &["--session", "pm-a", "show", "--brief"],
        "",
    )
    .await;
    assert_eq!(out.stdout, "no open items\n");
    let out = offline(&root, None, &["add", "G", "--title", "x"], "").await;
    assert_eq!(out.error.map(|e| e.0), Some(2), "no session is refused");
}
