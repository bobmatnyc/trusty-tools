//! Tests for `tm pr watch` and the AC6 independence guard (#11673).

use std::path::{Path, PathBuf};

use super::*;
use crate::commands::pr::GhRun;

/// Answers the first route whose needle the joined argv contains.
struct Gh(Vec<(&'static str, String)>);

impl GhRunner for Gh {
    fn run(&self, args: &[String]) -> anyhow::Result<GhRun> {
        let joined = args.join(" ");
        let (_, out) = self
            .0
            .iter()
            .find(|(needle, _)| joined.contains(needle))
            .ok_or_else(|| anyhow::anyhow!("unscripted gh call: {joined}"))?;
        Ok(GhRun {
            success: true,
            stdout: out.clone(),
            stderr: String::new(),
        })
    }
}

fn gh(state: &str) -> Gh {
    Gh(vec![
        (
            "pr view",
            format!(r#"{{"state":"{state}","headRefOid":"abc123","baseRefName":"main","statusCheckRollup":[]}}"#),
        ),
        (
            "rules/branches/main",
            r#"[{"type":"required_status_checks","parameters":{"required_status_checks":[{"context":"lint"}]}}]"#.into(),
        ),
        (
            "branches/main",
            r#"{"protection":{"enabled":true,"required_status_checks":{"contexts":["ci"]}}}"#.into(),
        ),
    ])
}

#[test]
fn register_records_head_required_and_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let table = Table::at(dir.path());
    let line = register_with(
        &gh("OPEN"),
        &table,
        7,
        Some("o/r"),
        None,
        dir.path(),
        || Some("pm-a".into()),
    )
    .expect("registered");
    assert!(
        line.contains("added o/r#7 head=abc123 owner=pm-a required=ci, lint"),
        "{line}"
    );
    let rows = table.list().expect("list");
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].required, ["ci", "lint"]);
    assert_eq!(rows[0].verdict, None);
    let again = register_with(
        &gh("OPEN"),
        &table,
        7,
        Some("o/r"),
        Some("pm-b".into()),
        dir.path(),
        || None,
    )
    .expect("re-registered");
    assert!(again.contains("updated"), "{again}");
    assert_eq!(table.list().expect("list")[0].owner, "pm-b", "--owner wins");
}

#[test]
fn register_refuses_a_merged_pr_and_an_unknown_owner() {
    let dir = tempfile::tempdir().expect("tempdir");
    let table = Table::at(dir.path());
    let merged = register_with(
        &gh("MERGED"),
        &table,
        7,
        Some("o/r"),
        Some("pm".into()),
        dir.path(),
        || None,
    );
    assert!(merged.is_err());
    let ownerless = register_with(
        &gh("OPEN"),
        &table,
        7,
        Some("o/r"),
        None,
        dir.path(),
        || None,
    );
    assert!(format!("{:#}", ownerless.expect_err("no owner")).contains("--owner"));
    assert!(table.list().expect("list").is_empty());
}

/// Every `.rs` file under `dir`.
fn rust_files(dir: &Path, out: &mut Vec<PathBuf>) {
    for entry in std::fs::read_dir(dir).expect("readable src dir").flatten() {
        let path = entry.path();
        if path.is_dir() {
            rust_files(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

/// AC6: the merge pins, `tm pr queue-check`, `tm pr merge`, and the critic and
/// review gates never read the PR watch table.
///
/// What: every crate source using a `pr_watch::` path or naming the table file is
/// on a short list of the table's own readers and writers. A gate that starts
/// reading it fails here.
#[test]
fn only_the_watch_verbs_and_the_daemon_loop_read_the_table() {
    let src = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    rust_files(&src, &mut files);
    let allowed = [
        "core/mod.rs",
        "core/pr_watch/",
        "daemon/services/mod.rs",
        "daemon/services/pr_watch_",
        "daemon/mod.rs",
        "bin/tm/commands/pr/watch",
        "bin/tm/commands/pr/mod.rs",
        "bin/tm/commands/wait/",
    ];
    let mut readers = Vec::new();
    for path in &files {
        let rel = path
            .strip_prefix(&src)
            .expect("under src")
            .to_string_lossy()
            .replace('\\', "/");
        let text = std::fs::read_to_string(path).expect("readable source");
        let names = text.contains("pr_watch::") || text.contains("pr-watch.json");
        if names && !allowed.iter().any(|a| rel.starts_with(a)) {
            readers.push(rel);
        }
    }
    assert!(
        readers.is_empty(),
        "the PR watch table is read outside its owners: {readers:?}"
    );
    // The gates themselves, by name: none may mention the watch.
    for gate in [
        "queue_check.rs",
        "merge.rs",
        "check_gate.rs",
        "critic_verdict.rs",
    ] {
        let text = std::fs::read_to_string(src.join("bin/tm/commands/pr").join(gate))
            .expect("gate source");
        assert!(
            !text.contains("watch::") && !text.contains("pr_watch"),
            "{gate} reads the watch"
        );
    }
}

/// `tm pr watch add|ls|rm` parse with their flags (#11673).
#[test]
fn cli_parses_pr_watch() {
    use crate::cli::{Cli, Command, PrCmd};
    use clap::Parser as _;
    let parse = |argv: &[&str]| match Cli::try_parse_from(argv).expect("parses").command {
        Some(Command::Pr {
            cmd: PrCmd::Watch(args),
        }) => args.cmd,
        other => panic!("expected pr watch, got {other:?}"),
    };
    let PrWatchCmd::Add(a) = parse(&[
        "tm", "pr", "watch", "add", "7", "--repo", "o/r", "--owner", "pm",
    ]) else {
        panic!("expected add");
    };
    assert_eq!(
        (a.pr, a.repo.as_deref(), a.owner.as_deref()),
        (7, Some("o/r"), Some("pm"))
    );
    assert!(matches!(parse(&["tm", "pr", "watch", "ls", "--json"]), PrWatchCmd::Ls(l) if l.json));
    assert!(matches!(parse(&["tm", "pr", "watch", "rm", "7"]), PrWatchCmd::Rm(r) if r.pr == 7));
}
