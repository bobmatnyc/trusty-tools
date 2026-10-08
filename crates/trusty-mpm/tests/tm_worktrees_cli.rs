//! `tm worktrees --json` through the built binary on a scratch HOME (#8994).
//!
//! Why: criterion 4 of the slice — the report's count and GiB per project are
//! asserted on the real binary's output, not on an in-process struct.
//! What: seeds `<home>/.trusty-mpm/worktrees.jsonl` and runs `tm worktrees`
//! with `HOME` pointed at the scratch directory; no registry exists, so the
//! backfill appends nothing.
//! Test: `cargo test -p trusty-mpm --test integration tm_worktrees_cli::`.

use std::path::{Path, PathBuf};

use serde_json::Value;
use trusty_mpm::core::worktree_ledger::fold::GIB;
use trusty_mpm::core::worktree_ledger::{EventKind, LedgerEvent, Origin, WorktreeLedger};

use crate::common;

fn created(path: &Path, repo: &str) -> LedgerEvent {
    LedgerEvent::now(
        path.to_path_buf(),
        EventKind::Created {
            repo: PathBuf::from(repo),
            branch: None,
            origin: Origin::TmDaemon,
            session: None,
        },
    )
}

fn worktrees_json(home: &Path, extra: &[&str]) -> Value {
    let out = common::tm_command_in(home)
        .arg("worktrees")
        .arg("--json")
        .args(extra)
        .output()
        .expect("spawn tm");
    assert!(
        out.status.success(),
        "tm worktrees failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    serde_json::from_slice(&out.stdout).expect("JSON report")
}

#[test]
fn tm_worktrees_json_reports_count_and_gib_per_project() {
    let home = tempfile::tempdir().unwrap();
    let ledger = WorktreeLedger::under_home(home.path());
    let gib = GIB as u64;
    let a = Path::new("/nonexistent-8994/alpha/.worktrees/a");
    let b = Path::new("/nonexistent-8994/alpha/.worktrees/b");
    let c = Path::new("/nonexistent-8994/beta/.worktrees/c");
    for e in [
        created(a, "/nonexistent-8994/alpha"),
        created(b, "/nonexistent-8994/alpha"),
        created(c, "/nonexistent-8994/beta"),
        LedgerEvent::now(a.to_path_buf(), EventKind::Measured { bytes: 2 * gib }),
        LedgerEvent::now(b.to_path_buf(), EventKind::Measured { bytes: gib / 4 }),
        LedgerEvent::now(c.to_path_buf(), EventKind::Measured { bytes: gib }),
        LedgerEvent::now(c.to_path_buf(), EventKind::Removed),
    ] {
        ledger.append(&e).unwrap();
    }

    let report = worktrees_json(home.path(), &["--no-size"]);

    let projects = report["projects"].as_array().expect("projects");
    assert_eq!(
        projects.len(),
        1,
        "beta's only tree was removed: {report:#}"
    );
    assert_eq!(projects[0]["repo"], "/nonexistent-8994/alpha");
    assert_eq!(projects[0]["count"], 2);
    assert_eq!(projects[0]["gib"], 2.25);
    assert_eq!(report["total"]["count"], 2);
    assert_eq!(report["total"]["gib"], 2.25);
    assert!(
        report["measured"].is_null(),
        "--no-size measures nothing: {report:#}"
    );
}

#[test]
fn tm_worktrees_measures_a_live_tree_unless_no_size() {
    let home = tempfile::tempdir().unwrap();
    let tree = home.path().join("tree");
    std::fs::create_dir_all(&tree).unwrap();
    std::fs::write(tree.join("blob"), vec![1u8; 256 * 1024]).unwrap();
    let key = std::fs::canonicalize(&tree).unwrap();
    WorktreeLedger::under_home(home.path())
        .append(&created(&key, "/nonexistent-8994/gamma"))
        .unwrap();

    let skipped = worktrees_json(home.path(), &["--no-size"]);
    assert_eq!(skipped["projects"][0]["unmeasured"], 1, "{skipped:#}");

    let measured = worktrees_json(home.path(), &[]);
    assert_eq!(measured["measured"], 1, "{measured:#}");
    assert_eq!(measured["projects"][0]["unmeasured"], 0, "{measured:#}");
    assert!(
        measured["projects"][0]["bytes"].as_u64().unwrap() >= 256 * 1024,
        "{measured:#}"
    );
}
