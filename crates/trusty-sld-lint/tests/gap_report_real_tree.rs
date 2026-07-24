//! Integration test: the gap report must run cleanly over the real
//! trusty-tools tree (issue #595 slice 1).
//!
//! Mirrors `lint_real_tree.rs`'s "run over the live tree" acid test: DOC-38's
//! own body quotes many fenced `# Spec References` / `{#SPEC-…}` examples that
//! a conforming scanner must not pick up as real units or sections, and the
//! real tree exercises every language branch of `detect_units` at once.

use std::path::PathBuf;

use trusty_sld_lint::gap::run_gap_report;

/// The workspace root, relative to this crate's manifest dir.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root resolves")
}

#[test]
fn gap_report_runs_on_real_tree() {
    let report = run_gap_report(&workspace_root());

    // The real tree has thousands of public Rust items and dozens of anchored
    // spec sections; a report that found none would mean detection silently
    // broke, not that the tree is somehow perfectly linked both ways.
    assert!(
        report.units_scanned > 100,
        "expected many public code units scanned, got {}",
        report.units_scanned
    );
    assert!(
        report.spec_sections_scanned > 10,
        "expected many anchored spec sections scanned, got {}",
        report.spec_sections_scanned
    );

    // Non-strict is report-and-succeed regardless of gap counts: this is a
    // read-only report, not a gate (issue #595). Rendering must not panic.
    let _ = report.summary();
    let _ = report.to_json();
}
