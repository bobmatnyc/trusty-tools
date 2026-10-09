//! Tests for the call-chain report parser (#9196).
//!
//! Why: the report is an unversioned text contract (ruling Q10); a format
//! change must read as unparseable, never as a symbol with no edges.
//! What: the contract fixture, renamed headers, the ambiguity header, the
//! byte bound, and the test-caller rule.
//! Test: included as `#[cfg(test)] mod tests` from `callgraph.rs`.

use super::*;

/// The report shape trusty-search renders for `direction=both`,
/// `max_depth=1`, `include_source=false`, written from its renderer
/// (`trusty-search/src/service/call_chain/mod.rs` at cb47439155) with the
/// `# Generated:` timestamp line removed. The live check re-captures it.
fn fixture() -> String {
    let path = concat!(
        env!("CARGO_MANIFEST_DIR"),
        "/tests/fixtures/call_chain_report.txt"
    );
    std::fs::read_to_string(path).expect("call_chain_report.txt is readable")
}

#[test]
fn parses_a_real_report() {
    let edges = parse_report(&fixture()).expect("parses");
    assert_eq!(
        edges.entry_file,
        "crates/trusty-review/src/pipeline/optional_context/files.rs"
    );
    assert_eq!(edges.entry_line, 203);
    assert_eq!(edges.signature, "pub(crate) async fn apply_files(");
    let names = |list: &[Edge]| list.iter().map(|e| e.symbol.clone()).collect::<Vec<_>>();
    assert_eq!(
        names(&edges.callees),
        ["empty_row", "fetcher_for", "screen", "read_all"]
    );
    assert_eq!(names(&edges.callers), ["run_review_with", "run_with"]);
    assert_eq!(
        edges.callers[0].location,
        "crates/trusty-review/src/pipeline/runner.rs:240"
    );
    assert_eq!(
        edges.callers[0].file(),
        "crates/trusty-review/src/pipeline/runner.rs"
    );
    assert!(!edges.cut);
}

/// Plan arm 12: a report whose list headers were renamed is an error, not
/// an empty edge set that would render "none found".
#[test]
fn renamed_headers_are_unparseable_not_empty() {
    let report = fixture()
        .replace("Calls →", "Callees:")
        .replace("Called by ←", "Callers:");
    assert_eq!(parse_report(&report), Err(ParseError::Unparseable));
    assert_eq!(
        parse_report("{\"error\":\"entry point not found: x\"}"),
        Err(ParseError::Unparseable)
    );
    assert_eq!(parse_report(""), Err(ParseError::Unparseable));
    let orphan = fixture().replace("Calls →\n", "");
    assert_eq!(parse_report(&orphan), Err(ParseError::Unparseable));
}

/// Ruling Q9: the daemon's pick among several definitions is never read.
#[test]
fn an_ambiguous_report_is_refused() {
    let report = fixture().replacen(
        "═══",
        "# AMBIGUOUS: `apply_files` matches 2 definitions; anchored to the most \
         connected one. Re-run with one of these to pick another:\n#   b.rs::apply_files\n\n═══",
        1,
    );
    assert_eq!(parse_report(&report), Err(ParseError::Ambiguous));
}

/// Plan arm 13: past the byte bound the report is cut, the edges before the
/// cut are kept, and `cut` says so.
#[test]
fn an_oversize_report_is_cut_and_marked() {
    let mut report = fixture().replace("Called by ←\n", "");
    let at = report.find("\n\n───").expect("separator") + 1;
    let many: String = (0..20_000)
        .map(|i| format!("  · callee_{i}  src/c.rs:{i}\n"))
        .collect();
    report.insert_str(at, &many);
    assert!(report.len() > MAX_SYMBOL_REPORT_BYTES);
    let edges = parse_report(&report).expect("parses up to the bound");
    assert!(edges.cut);
    assert!(edges.callees.len() > 1_000, "{}", edges.callees.len());
    assert!(edges.callers.is_empty());
}

/// Ruling Q11: a caller is a test by its path or a `test_` name.
#[test]
fn test_callers_are_told_apart_by_path_or_name() {
    let edge = |symbol: &str, location: &str| Edge {
        symbol: symbol.to_string(),
        location: location.to_string(),
    };
    assert!(is_test_edge(&edge("run_with", "src/a/files_tests.rs:224")));
    assert!(is_test_edge(&edge("check", "crates/x/tests/it.rs:3")));
    assert!(is_test_edge(&edge("Suite::test_total", "src/lib.rs:9")));
    assert!(!is_test_edge(&edge("run_review_with", "src/runner.rs:240")));
    assert!(!is_test_edge(&edge("contest", "src/contest.rs:1")));
}

/// #9196: a Rust test file is told by its path even when the caller's name has
/// no `test_` prefix.
#[test]
fn rust_test_file_paths_classify_a_caller_as_a_test() {
    let edge = |symbol: &str, location: &str| Edge {
        symbol: symbol.to_string(),
        location: location.to_string(),
    };
    for path in [
        "crates/x/src/service/server/tests_search.rs",
        "crates/x/src/tests.rs",
        "crates/x/src/a/b_tests.rs",
        "crates/x/src/a/b_test.rs",
        "crates/x/tests/it.rs",
    ] {
        let caller = edge("file_is_within_root_symlinked_root", &format!("{path}:42"));
        assert!(is_test_edge(&caller), "{path} should be a test location");
    }
    for path in [
        "crates/x/src/contests.rs",
        "crates/x/src/latest_results.rs",
        "crates/x/src/lib.rs",
    ] {
        let caller = edge("file_is_within_root_symlinked_root", &format!("{path}:42"));
        assert!(!is_test_edge(&caller), "{path} is not a test location");
    }
}
