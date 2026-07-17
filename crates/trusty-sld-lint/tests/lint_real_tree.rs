//! Integration test: the linter must pass on the real trusty-tools tree.
//!
//! DOC-38 is the acid test — its own body quotes many fenced `# Spec References`
//! and `{#SPEC-…}` examples that a conforming scanner must NOT extract. A clean
//! run over the live `docs/specs/` + `crates/` tree proves the fenced-code
//! exclusion, the test-file exclusion, and the grandfathering model all hold.

use std::path::PathBuf;

use trusty_sld_lint::{run, LintOptions};

/// The workspace root, relative to this crate's manifest dir.
fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("workspace root resolves")
}

#[test]
fn lints_real_tree_clean_default_mode() {
    let root = workspace_root();
    let report = run(&LintOptions::new(&root)).expect("linter runs on the real tree");
    assert!(
        report.is_clean(),
        "the real tree has {} SLD error(s) in default mode:\n{}",
        report.error_count(),
        report
            .diagnostics
            .iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert!(report.spec_docs > 10, "should scan the specs catalog");
    assert!(report.code_files > 10, "should scan the crates tree");
}
