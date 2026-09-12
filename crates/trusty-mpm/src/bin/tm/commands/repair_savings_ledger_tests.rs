//! Behaviour tests for `tm repair savings-ledger` (#7569).
//!
//! Why: the command's safety claim is that the default run writes nothing and
//! that an `--apply` run is idempotent. Both are properties of the HANDLER —
//! the flag it reads and the order it does things in — not of the library
//! module the handler calls, so both need a test here.
//! What: a temp framework root holding a two-row ledger, one row matching the
//! predicate, driven through the handler exactly as the CLI drives it.
//! Test: this file.

use super::{repair_savings_ledger, resolve_root};

const FIXTURE_ROW: &str = r#"{"ts":"2026-09-10T04:00:00Z","session_id":"b","technique":"instruction-compression","tokens_saved":5636,"tokens_before":5639,"cost_saved_usd":0.0169,"basis":"sources 22559 B - compiled 13 B, at 4 B/token, priced at claude-sonnet-4-5 input $3/Mtok","model_source":"config-fallback"}"#;

const GENUINE_ROW: &str = r#"{"ts":"2026-09-10T03:41:19Z","session_id":"11111111-1111-4111-8111-111111111111","technique":"compress","tokens_saved":260,"tokens_before":269,"cost_saved_usd":0.0026,"basis":"output 269 tok - compressed 9 tok, at 4 B/token, via rtk_binary, priced at claude-fable-5-1 (statusline) input $10/Mtok","model_source":"statusline"}"#;

/// A framework root holding a ledger with one fixture row and one genuine row.
fn root_with_ledger() -> (tempfile::TempDir, std::path::PathBuf) {
    let root = tempfile::tempdir().expect("temp root");
    let ledger = trusty_mpm::core::savings::savings_log_in(root.path());
    std::fs::create_dir_all(ledger.parent().expect("parent")).expect("mkdir");
    std::fs::write(&ledger, format!("{GENUINE_ROW}\n{FIXTURE_ROW}\n")).expect("write");
    (root, ledger)
}

/// Why: `--root` is what an operator uses to rehearse the repair against a
/// copy, and the default must still be the real framework root.
/// Test: itself.
#[test]
fn resolve_root_honours_the_override() {
    assert_eq!(
        resolve_root(Some("/tmp/rehearsal".to_string())),
        std::path::PathBuf::from("/tmp/rehearsal")
    );
    assert_eq!(
        resolve_root(None),
        trusty_mpm::core::paths::FrameworkPaths::default().root
    );
}

/// Why: the dry run is the DEFAULT, and an operator inspecting a live ledger
/// must be certain nothing moved. A command that wrote on the default path
/// would have destroyed rows before anyone authorised it.
/// Test: itself.
#[test]
fn repair_savings_ledger_dry_run_writes_nothing() {
    let (root, ledger) = root_with_ledger();
    let before = std::fs::read_to_string(&ledger).expect("read");

    repair_savings_ledger(Some(root.path().display().to_string()), false, true).expect("dry run");

    assert_eq!(
        std::fs::read_to_string(&ledger).expect("read"),
        before,
        "the default run must leave the ledger byte-identical"
    );
    let sidecars: Vec<_> = std::fs::read_dir(ledger.parent().expect("parent"))
        .expect("read usage dir")
        .filter_map(|entry| entry.ok())
        .filter(|entry| entry.file_name() != std::ffi::OsStr::new("savings.jsonl"))
        .collect();
    assert!(
        sidecars.is_empty(),
        "the default run must create no sidecar, found {sidecars:?}"
    );
}

/// Why: the issue's closure condition is a repair that drives the polluted
/// count to zero and leaves every real row alone — and re-running it must be a
/// no-op, since an operator will.
/// Test: itself.
#[test]
fn repair_savings_ledger_applies_and_is_idempotent() {
    let (root, ledger) = root_with_ledger();
    let markers = trusty_mpm::core::savings_repair::marker_dir(root.path());
    std::fs::create_dir_all(&markers).expect("mkdir");
    std::fs::write(markers.join("0002e2d5001ed230"), "22559 24667").expect("marker");

    repair_savings_ledger(Some(root.path().display().to_string()), true, true).expect("apply");

    assert_eq!(
        std::fs::read_to_string(&ledger).expect("read"),
        format!("{GENUINE_ROW}\n"),
        "only the genuine row survives, byte-identical"
    );
    assert!(!markers.exists(), "--markers moves the directory aside");

    let after = std::fs::read_to_string(&ledger).expect("read");
    repair_savings_ledger(Some(root.path().display().to_string()), true, true)
        .expect("second apply");
    assert_eq!(
        std::fs::read_to_string(&ledger).expect("read"),
        after,
        "a second apply must change nothing"
    );
}
