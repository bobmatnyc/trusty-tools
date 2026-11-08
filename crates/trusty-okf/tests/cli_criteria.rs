//! The `trusty-okf` command line, core build (SPEC-OKF-01 §4.10, B7) — slice
//! S8 of #10746.
//!
//! Why: the CLI must refuse a bad call before any write (exit 2), report
//! each axis apart, never claim "searchable" without a feed pass, and exit
//! with the §4.10 code. These run in every feature lane; the feed paths
//! are in `import_feed_criteria.rs`.
//! What: calls `trusty_okf::cli::run` with argument vectors, and the built
//! binary once, over a source tree and a bundle from `s8_support`.
//! Test: `cargo test -p trusty-okf --test cli_criteria`.

mod s8_support;

use std::ffi::OsString;

use s8_support::{Fx, snapshot};

fn cli(args: &[&str]) -> (i32, String, String) {
    let args: Vec<OsString> = args.iter().map(OsString::from).collect();
    let (mut out, mut err) = (Vec::new(), Vec::new());
    let code = trusty_okf::cli::run(&args, &mut out, &mut err);
    (
        code,
        String::from_utf8(out).unwrap(),
        String::from_utf8(err).unwrap(),
    )
}

fn s(p: &std::path::Path) -> String {
    p.to_string_lossy().into_owned()
}

/// Why: §4.10 — a run with a skipped item exits 3; the report keeps each
/// axis on its own line and, with no feed pass, says "not fully
/// searchable" and never "fully searchable" alone (criterion 60).
/// What: `import run` over a tree with a PDF and no worker.
/// Test: self-contained.
#[test]
fn cli_import_run_reports_each_axis_and_exits_3_on_a_skip() {
    let fx = Fx::new(&[("a.md", "Alpha.\n"), ("c.pdf", "%PDF")]);
    let from = format!("dir:{}", s(&fx.src));
    let (code, out, err) = cli(&[
        "import",
        "run",
        &s(&fx.root),
        "--from",
        &from,
        "--now",
        "2026-11-04T12:00:00Z",
    ]);
    assert_eq!(code, 3, "{out}{err}");
    assert!(
        out.contains(
            "index: pending 1, acknowledged 0, searchable 0, withdrawn 0 (no feed pass ran)"
        ),
        "{out}"
    );
    assert!(
        out.contains("item c.pdf: read skipped, write none, reason worker_absent"),
        "{out}"
    );
    assert!(
        out.contains("not fully searchable (no feed pass ran)"),
        "{out}"
    );
    assert!(
        !out.replace("not fully searchable", "")
            .contains("fully searchable"),
        "{out}"
    );
    let (code, out, _) = cli(&["import", "run", &s(&fx.root), "--from", &from, "--json"]);
    assert_eq!(code, 3);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["unchanged"], 1);
    assert_eq!(v["exit_code"], 3);
}

/// Why: §4.10 code 2 — a bad call is refused before any write, with usage.
/// What: unknown options, an unsupported source, a missing root, `--feed`
/// without its arguments, and (#10746 S8 fix) a state directory inside the
/// bundle or an index ID that is not one path segment; the bundle and
/// source snapshots stay equal.
/// Test: self-contained.
#[test]
fn cli_refuses_bad_calls_with_2_before_any_write() {
    let fx = Fx::new(&[("a.md", "Alpha.\n")]);
    let (root, from) = (s(&fx.root), format!("dir:{}", s(&fx.src)));
    let before = (snapshot(&fx.root), snapshot(&fx.src));
    let missing = s(&fx.root.join("nope"));
    let in_bundle = s(&fx.root.join(".st"));
    let machine = s(&fx.tmp.path().join("machine"));
    let sock = "/nonexistent/okf.sock";
    for args in [
        vec![
            "import",
            "run",
            &root,
            "--from",
            &from,
            "--feed",
            "--index",
            "okf",
            "--state-dir",
            &in_bundle,
            "--socket",
            sock,
        ],
        vec![
            "import",
            "run",
            &root,
            "--from",
            &from,
            "--feed",
            "--index",
            "a/b",
            "--state-dir",
            &machine,
            "--socket",
            sock,
        ],
        vec!["import", "run", &root, "--from", &from, "--worker", "/x"],
        vec!["import", "run", &root, "--from", "gmail"],
        vec!["import", "run", &root],
        vec!["import", "run", &missing, "--from", &from],
        vec!["import", "run", &root, "--from", &from, "--feed"],
        vec!["import", "plan", &root, "--from", &from, "--feed"],
        vec!["import", "run", &root, "--from", &from, "--include", "["],
        vec!["frobnicate"],
    ] {
        let (code, out, err) = cli(&args);
        assert_eq!(code, 2, "{args:?}: {out}{err}");
    }
    assert_eq!((snapshot(&fx.root), snapshot(&fx.src)), before);
}

/// Why: criterion 72 through the CLI, and `--dry-run` equals `import plan`.
/// What: both print the plan and leave the bundle byte-identical.
/// Test: self-contained.
#[test]
fn cli_plan_and_dry_run_write_nothing() {
    let fx = Fx::new(&[("a.md", "Alpha.\n"), ("n/b.txt", "Beta.\n")]);
    let (root, from) = (s(&fx.root), format!("dir:{}", s(&fx.src)));
    let before = snapshot(&fx.root);
    let (c1, plan, _) = cli(&["import", "plan", &root, "--from", &from]);
    let (c2, dry, _) = cli(&["import", "run", &root, "--from", &from, "--dry-run"]);
    assert_eq!((c1, c2), (0, 0));
    assert!(
        plan.contains("Create a.md -> a extractor=builtin-text@1"),
        "{plan}"
    );
    assert_eq!(plan, dry);
    assert_eq!(snapshot(&fx.root), before);
}

/// Why: criterion 83 (CLI half) and §4.10 `import status` — per-axis
/// counts per store, and each attempt listed with its state.
/// What: one run, then `import status`, text and JSON.
/// Test: self-contained.
#[test]
fn cli_status_lists_stores_axes_and_runs() {
    let fx = Fx::new(&[("a.md", "Alpha.\n")]);
    let (root, from) = (s(&fx.root), format!("dir:{}", s(&fx.src)));
    assert_eq!(cli(&["import", "run", &root, "--from", &from]).0, 0);
    let (code, out, _) = cli(&["import", "status", &root]);
    assert_eq!(code, 0);
    assert!(
        out.contains("save: saved 1, saved_external 0, absent 0"),
        "{out}"
    );
    assert!(out.contains("index: not read"), "{out}");
    assert!(out.contains("finished"), "{out}");
    let (_, out, _) = cli(&["import", "status", &root, "--json"]);
    let v: serde_json::Value = serde_json::from_str(&out).unwrap();
    assert_eq!(v["runs"][0]["interrupted"], false);
}

/// Why: B7 and the main.rs wiring — the built binary runs the same verbs and
/// exits with the §4.10 code; it has no stdio server mode.
/// What: the binary's `import plan`, then a `serve --stdio` call refused 2.
/// Test: self-contained.
#[test]
fn cli_binary_runs_the_verbs_and_has_no_stdio_server() {
    let fx = Fx::new(&[("a.md", "Alpha.\n")]);
    let bin = env!("CARGO_BIN_EXE_trusty-okf");
    let out = std::process::Command::new(bin)
        .args([
            "import",
            "plan",
            &s(&fx.root),
            "--from",
            &format!("dir:{}", s(&fx.src)),
        ])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(0), "{out:?}");
    let out = std::process::Command::new(bin)
        .args(["serve", "--stdio"])
        .output()
        .unwrap();
    assert_eq!(out.status.code(), Some(2), "{out:?}");
}
