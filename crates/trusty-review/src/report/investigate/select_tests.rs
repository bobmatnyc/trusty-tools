//! Tests for deterministic file selection + coverage (#2357).
//!
//! Why: selection is the deterministic gate on what the LLM sees; ranking,
//! budget caps, truncation, and coverage must be exact and reproducible.
//! What: builds a temp fixture repo with planted auth/store/package/error/test
//! files and asserts ranking, the file/byte caps, per-file truncation, dimension
//! classification, keyword extraction, and coverage.  No LLM, no network.
//! Test: included as `#[cfg(test)] mod tests` from `select.rs`.

use std::fs;
use std::path::{Path, PathBuf};

use super::*;
use crate::report::metrics::{MetricFinding, Severity};
use crate::report::scan::list_tracked_files;

/// The exact selection order the fixture produced BEFORE #6078, captured by
/// running `select_files` on `origin/main` (bbc039b46).
///
/// Why: the ticket's hard requirement is that absent signals leave selection
/// byte-identical. A recorded literal is the only assertion that can fail if a
/// future scoring tweak silently changes the no-signal path.
const BASELINE_ORDER: &[&str] = &[
    "src/auth/login.rs",
    "src/errors.rs",
    "src/store/user_store.ts",
    "package.json",
    "tests/login_test.rs",
    "README.md",
];

/// Build a priority list the way `load_manifest` would, from bare paths.
fn priorities(paths: &[&str]) -> Vec<InspectionPriority> {
    paths
        .iter()
        .enumerate()
        .map(|(i, p)| InspectionPriority {
            path: (*p).to_string(),
            weight: 1000 - i as u32,
        })
        .collect()
}

/// An `AnalyzeMetrics` whose findings name `components`.
fn metrics_flagging(components: &[&str]) -> AnalyzeMetrics {
    AnalyzeMetrics {
        findings: components
            .iter()
            .map(|c| MetricFinding {
                title: "clippy diagnostic".to_string(),
                severity: Severity::Amber,
                category: "clippy".to_string(),
                component: (*c).to_string(),
                description: "d".to_string(),
                remediation: String::new(),
            })
            .collect(),
        ..Default::default()
    }
}

/// The selected paths, in ranked order.
fn order(sel: &Selection) -> Vec<String> {
    sel.files.iter().map(|f| f.path.clone()).collect()
}

/// Create `root/rel` with `content`, making parent dirs.
fn write(root: &Path, rel: &str, content: &str) {
    let path = root.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, content).unwrap();
}

/// Build a fixture repo covering several DD dimensions; returns (tmp, root).
fn fixture() -> tempfile::TempDir {
    let tmp = tempfile::TempDir::new().unwrap();
    let r = tmp.path();
    write(
        r,
        "src/auth/login.rs",
        "fn login() { let token = read_secret(); }\n",
    );
    write(
        r,
        "src/store/user_store.ts",
        "export const store = createStore();\n",
    );
    write(r, "src/errors.rs", "pub enum Error { NotFound }\n");
    write(
        r,
        "package.json",
        "{\"dependencies\": {\"react\": \"^18\"}}\n",
    );
    write(
        r,
        "README.md",
        "# Project\n\nSome prose that is not code.\n",
    );
    write(r, "tests/login_test.rs", "#[test] fn t() {}\n");
    tmp
}

/// Why: an instruction-matched, dimension-matched code file must rank first.
/// What: with a brief mentioning "login", the auth file outranks the rest.
/// Test: this test itself.
#[test]
fn ranks_relevant_first() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let sel = select_files(
        tmp.path(),
        &files,
        Some("Focus on the login and authentication flow."),
        Budget::default(),
        RiskSignals::default(),
    );
    assert!(!sel.is_empty());
    assert!(
        sel.files[0].path.contains("auth/login"),
        "auth/login should rank first, got {}",
        sel.files[0].path
    );
}

/// Why: the file cap is a hard budget.
/// What: max_files = 2 selects exactly 2 and reports the rest skipped.
/// Test: this test itself.
#[test]
fn budget_caps_file_count() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let total = files.len();
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget {
            max_files: 2,
            max_bytes: DEFAULT_MAX_BYTES,
        },
        RiskSignals::default(),
    );
    assert_eq!(sel.files.len(), 2);
    assert_eq!(sel.total_files, total);
    assert_eq!(sel.skipped, total - 2);
}

/// Why: the byte cap is a hard budget.
/// What: a tiny max_bytes keeps bytes_sent within the cap and drops files.
/// Test: this test itself.
#[test]
fn budget_caps_total_bytes() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget {
            max_files: 40,
            max_bytes: 30,
        },
        RiskSignals::default(),
    );
    assert!(sel.bytes_sent <= 30, "bytes_sent {} > cap", sel.bytes_sent);
    assert!(sel.files.len() < files.len());
}

/// Why: an oversize file must be truncated with the visible marker so the byte
/// budget is respected and the reader knows content was cut.
/// What: plants a >24KB file and asserts it is truncated.
/// Test: this test itself.
#[test]
fn truncates_oversize_file() {
    let tmp = tempfile::TempDir::new().unwrap();
    let big = "x".repeat(30 * 1024);
    write(tmp.path(), "src/big.rs", &big);
    let files = list_tracked_files(tmp.path());
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget::default(),
        RiskSignals::default(),
    );
    let f = sel
        .files
        .iter()
        .find(|f| f.path.contains("big.rs"))
        .unwrap();
    assert!(f.truncated);
    assert!(f.content.contains(TRUNCATION_MARKER.trim()));
    assert!(f.content.len() < big.len());
}

/// Why: coverage must report which DD dimensions were reached and which were not.
/// What: the fixture covers auth/deps/state/error/tests but not scalability.
/// Test: this test itself.
#[test]
fn coverage_reports_dimensions() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget::default(),
        RiskSignals::default(),
    );
    let covered = &sel.dimensions_covered;
    assert!(covered.iter().any(|d| d == "authentication & secrets"));
    assert!(covered.iter().any(|d| d == "dependencies"));
    assert!(covered.iter().any(|d| d == "state management"));
    assert!(covered.iter().any(|d| d == "error handling"));
    assert!(covered.iter().any(|d| d == "test coverage"));
    assert!(
        sel.dimensions_absent.iter().any(|d| d == "scalability"),
        "scalability has no fixture file and must be absent"
    );
}

/// Why: the path heuristics are the deterministic classifier steering selection.
/// What: spot-checks each dimension's path pattern.
/// Test: this test itself.
#[test]
fn dimension_heuristics_classify() {
    assert!(
        dimensions_for("src/auth/mod.rs")
            .iter()
            .any(|d| d == "authentication & secrets")
    );
    assert!(
        dimensions_for("config/settings.rs")
            .iter()
            .any(|d| d == "authentication & secrets")
    );
    assert!(
        dimensions_for("Cargo.toml")
            .iter()
            .any(|d| d == "dependencies")
    );
    assert!(
        dimensions_for("src/state/reducer.ts")
            .iter()
            .any(|d| d == "state management")
    );
    assert!(
        dimensions_for("src/errors.rs")
            .iter()
            .any(|d| d == "error handling")
    );
    assert!(
        dimensions_for("src/queue/worker.rs")
            .iter()
            .any(|d| d == "scalability")
    );
    assert!(dimensions_for("src/main.rs").is_empty());
}

/// Why: instruction keywords steer ranking; short tokens are noise.
/// What: keeps ≥4-char tokens, lower-cases, dedupes, drops short words.
/// Test: this test itself.
#[test]
fn instruction_keywords_extracted() {
    let kw = instruction_keywords(Some("Check the AUTH and db pooling; auth auth."));
    assert!(kw.contains(&"auth".to_string()));
    assert!(kw.contains(&"pooling".to_string()));
    assert!(!kw.iter().any(|k| k == "db"), "short tokens dropped");
    assert_eq!(kw.iter().filter(|k| *k == "auth").count(), 1, "deduped");
    assert!(instruction_keywords(None).is_empty());
}

/// Why: a non-existent path yields no files, and selection is empty (not a panic).
/// What: selects over an empty file list.
/// Test: this test itself.
#[test]
fn empty_repo_selects_nothing() {
    let sel = select_files(
        Path::new("/nonexistent-xyz"),
        &[] as &[PathBuf],
        None,
        Budget::default(),
        RiskSignals::default(),
    );
    assert!(sel.is_empty());
    assert_eq!(sel.total_files, 0);
}

// ─── #6078: risk-ranked selection ────────────────────────────────────────────

/// Why: #6078 adds two ranking inputs, and absent both the selection must be
/// what it was before — anything else silently rewrites every existing report.
/// What: asserts the fixture's ranked order equals [`BASELINE_ORDER`], the
/// literal captured from `origin/main`.
/// Test: this test itself.
#[test]
fn absent_signals_reproduce_baseline_order() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget::default(),
        RiskSignals::default(),
    );
    assert_eq!(order(&sel), BASELINE_ORDER);
}

/// Why: a file trusty-analyze already flagged carries direct evidence of risk,
/// so it must outrank an otherwise-identical unflagged file.
/// What: two files that score identically on every path heuristic; flagging one
/// moves it to the front, and the same call with `None` metrics does not.
/// Test: this test itself.
#[test]
fn analyze_flagged_file_outranks_peer() {
    let tmp = tempfile::TempDir::new().unwrap();
    // Same extension, same directory, no dimension or keyword hits: identical
    // heuristic score, so `path asc` alone decides — `a.rs` before `b.rs`.
    write(tmp.path(), "src/a.rs", "fn a() {}\n");
    write(tmp.path(), "src/b.rs", "fn b() {}\n");
    let files = list_tracked_files(tmp.path());

    let plain = select_files(
        tmp.path(),
        &files,
        None,
        Budget::default(),
        RiskSignals::default(),
    );
    assert_eq!(order(&plain), ["src/a.rs", "src/b.rs"]);

    let m = metrics_flagging(&["src/b.rs"]);
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget::default(),
        RiskSignals {
            metrics: Some(&m),
            ..Default::default()
        },
    );
    assert_eq!(order(&sel), ["src/b.rs", "src/a.rs"]);
}

/// Why: the analyze daemon may name a file absolutely and with a line suffix;
/// both must still resolve to the tracked repo-relative path.
/// What: an absolute `component` carrying `:41` flags the same file.
/// Test: this test itself.
#[test]
fn analyze_component_with_line_suffix_matches() {
    let tmp = tempfile::TempDir::new().unwrap();
    write(tmp.path(), "src/a.rs", "fn a() {}\n");
    write(tmp.path(), "src/b.rs", "fn b() {}\n");
    let files = list_tracked_files(tmp.path());
    let m = metrics_flagging(&["/abs/checkout/src/b.rs:41"]);
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget::default(),
        RiskSignals {
            metrics: Some(&m),
            ..Default::default()
        },
    );
    assert_eq!(order(&sel), ["src/b.rs", "src/a.rs"]);
}

/// Why: `inspect_priority` is the interface external selection intelligence
/// writes to, so a declared path must be inspected ahead of every file the
/// heuristics rank highly — including one the analyst brief names.
/// What: the fixture's README, which scores lowest of all, is declared first and
/// leads the ranking despite a brief that steers toward auth.
/// Test: this test itself.
#[test]
fn manifest_priority_outranks_heuristics() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let prio = priorities(&["README.md"]);
    let sel = select_files(
        tmp.path(),
        &files,
        Some("Focus on the login and authentication flow."),
        Budget::default(),
        RiskSignals {
            priorities: &prio,
            ..Default::default()
        },
    );
    assert_eq!(sel.files[0].path, "README.md");
    assert_eq!(sel.files[1].path, "src/auth/login.rs");
}

/// Why: the declared list is RANKED, and the budget must truncate it from the
/// bottom — never scramble it — or an external ranker cannot predict what gets
/// read.
/// What: three priorities under a two-file budget select the first two in
/// declared order, even though their paths sort the other way.
/// Test: this test itself.
#[test]
fn priorities_beyond_the_budget_truncate_in_rank_order() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let prio = priorities(&["src/store/user_store.ts", "package.json", "README.md"]);
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget {
            max_files: 2,
            max_bytes: DEFAULT_MAX_BYTES,
        },
        RiskSignals {
            priorities: &prio,
            ..Default::default()
        },
    );
    assert_eq!(order(&sel), ["src/store/user_store.ts", "package.json"]);
}

/// Why: a priority naming nothing in the repo must be inert, not a failure and
/// not a displacement — an external ranker works from a stale index sometimes.
/// What: an unmatched path leaves the baseline order untouched.
/// Test: this test itself.
#[test]
fn unmatched_priority_is_inert() {
    let tmp = fixture();
    let files = list_tracked_files(tmp.path());
    let prio = priorities(&["src/does/not/exist.rs"]);
    let sel = select_files(
        tmp.path(),
        &files,
        None,
        Budget::default(),
        RiskSignals {
            priorities: &prio,
            ..Default::default()
        },
    );
    assert_eq!(order(&sel), BASELINE_ORDER);
}
