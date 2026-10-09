//! `run --report-context` (#9194).
//!
//! Why: AC1-AC3 on the CLI: `--json` wraps every ledger row beside the
//! unchanged review only when asked, and the human output names each row a
//! reader must act on, `absent` included.
//! What: drives one offline local-diff review the way `cmd_run` does, with
//! the issue-docs tests' capturing LLM and socket pin, and calls
//! `run_json_value` and `ledger_notes` directly.
//! Test: this module.

use clap::CommandFactory as _;
use trusty_review::models::{ContextSourceRecord, SourceState};
use trusty_review::pipeline::ReviewOutcome;
use trusty_review::run_output::run_json_payload;

use super::super::{ledger_notes, run_json_value, run_request};
use super::*;

/// One offline `run --local-diff <diff> [flags]` review, as `cmd_run` builds it.
async fn local_run(flags: &[&str]) -> (ReviewOutcome, bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let diff = dir.path().join("change.diff");
    std::fs::write(
        &diff,
        "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
         @@ -1 +1 @@\n-fn a() {}\n+fn a() { println!(\"a\"); }\n",
    )
    .expect("write diff");
    let diff_arg = diff.display().to_string();
    let mut argv = vec![
        "run",
        "--local-diff",
        diff_arg.as_str(),
        "--no-log",
        "--json",
    ];
    argv.extend_from_slice(flags);
    let args = RunArgs::try_parse_from(argv).expect("parse");
    let request = run_request_with_issue_docs(&args).expect("no docs file");
    let wants_ledger = request.ledger_enabled();
    let caller = caller_context_from_args(&args).expect("no text flags");
    let input = run_input(
        &args,
        DiffSource::LocalFile { path: diff },
        "m".into(),
        caller,
    );

    let key = trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;
    let _pin = SocketPin(std::env::var_os(key));
    // SAFETY: serial test; `SocketPin` restores the old value.
    unsafe { std::env::set_var(key, dir.path().join("no-search.sock")) };
    let deps = ReviewDeps {
        llm: Arc::new(Capture(Mutex::new(Vec::new()))),
        verifier: None,
        search: Arc::new(NullSearchClient::new("no search in #9194 tests")),
        analyze: None,
        dedup: None,
    };
    let mut config = ReviewConfig::load(None);
    config.search_url = "http://127.0.0.1:9".into();
    config.context.require_search = Some(false);
    config.context.require_analyze = false;
    config.log_dir = dir.path().to_path_buf();
    let outcome = run_review_with(&config, input, deps, ReviewOptions::new(request)).await;
    (outcome, wants_ledger)
}

/// #9194 AC2: `--report-context --json` wraps the unchanged review beside
/// all ten rows (#9195: `changed_files` after `claude_md`; #9196:
/// `symbol_context` after `analyze`).
#[serial_test::serial]
#[tokio::test]
async fn run_report_context_json_wraps_every_row() {
    let (outcome, wants_ledger) = local_run(&["--report-context"]).await;
    assert!(wants_ledger);
    let value = run_json_value(&outcome.result, Some(&outcome.context_sources));
    assert_eq!(value["result"], run_json_payload(&outcome.result));
    let rows = value["context_sources"].as_array().expect("an array");
    let names: Vec<&str> = rows.iter().filter_map(|r| r["source"].as_str()).collect();
    assert_eq!(
        names,
        [
            "pr_body",
            "caller_context",
            "issues",
            "spec_docs",
            "claude_md",
            "changed_files",
            "search",
            "analyze",
            "symbol_context",
            "external_sources"
        ]
    );
    assert_eq!(
        rows[7]["state"], "unavailable",
        "no analyze client: {value}"
    );
}

/// #9194 AC1: without a ledger flag, `--json` is the review object alone.
#[serial_test::serial]
#[tokio::test]
async fn run_without_report_context_json_is_the_plain_result() {
    let (outcome, wants_ledger) = local_run(&[]).await;
    assert!(!wants_ledger);
    assert!(outcome.context_sources.is_empty());
    assert_eq!(
        run_json_value(&outcome.result, None),
        run_json_payload(&outcome.result)
    );
}

fn with_detail(source: &str, state: SourceState, detail: &str) -> ContextSourceRecord {
    let mut row = ContextSourceRecord::new(source, state);
    row.detail = Some(detail.to_string());
    row
}

/// #9194 AC3: the human output names `absent`, `unavailable` and
/// `truncated` rows, in ledger order.
#[test]
fn run_human_output_names_absent_unavailable_and_truncated_rows() {
    let mut cut = ContextSourceRecord::new("pr_body", SourceState::Truncated);
    cut.chars_omitted = 12;
    let rows = [
        cut,
        with_detail("search", SourceState::Absent, "no hits for the query"),
        with_detail("analyze", SourceState::Unavailable, "analyze client absent"),
    ];
    assert_eq!(
        ledger_notes(&rows),
        [
            "context source pr_body: truncated — 12 characters omitted",
            "context source search: absent — no hits for the query",
            "context source analyze: unavailable — analyze client absent",
        ]
    );
}

/// #9194: `used` and `not_requested` rows need no action and print nothing.
#[test]
fn ledger_notes_skip_used_and_not_requested_rows() {
    let rows = [
        ContextSourceRecord::new("caller_context", SourceState::Used),
        with_detail(
            "issues",
            SourceState::NotRequested,
            "no issue_docs were sent",
        ),
    ];
    assert!(ledger_notes(&rows).is_empty());
}

/// #9194: `--report-context --help` names the rows and the states.
#[test]
fn report_context_help_names_the_rows_and_states() {
    let help = RunArgs::command()
        .get_arguments()
        .find(|a| a.get_id() == "report_context")
        .and_then(|a| a.get_help())
        .map(ToString::to_string)
        .unwrap_or_default();
    for word in [
        "changed_files", // #9195
        "search",
        "analyze",
        "symbol_context", // #9196
        "external_sources",
        "absent",
        "not_requested",
    ] {
        assert!(help.contains(word), "{word} missing from: {help}");
    }
}

/// #9195: `--changed-files` sets the flag, a new input; `--changed-files-budget`
/// parses into the request and refuses a negative value.
#[test]
fn run_changed_files_flags_set_the_request() {
    let args = RunArgs::try_parse_from(["run", "--changed-files"]).expect("parse");
    let request = run_request(&args);
    assert!(request.changed_files && request.requested_new());
    assert_eq!(request.changed_files_budget, None);
    let argv = ["run", "--changed-files", "--changed-files-budget", "5000"];
    let args = RunArgs::try_parse_from(argv).expect("parse");
    assert_eq!(run_request(&args).changed_files_budget, Some(5000));
    assert!(RunArgs::try_parse_from(["run", "--changed-files-budget", "-1"]).is_err());
}

/// #9195 amendment 10: `--changed-files-budget` without `--changed-files` is
/// inert: no new input, no ledger.
#[test]
fn changed_files_budget_without_flag_is_inert() {
    let args = RunArgs::try_parse_from(["run", "--changed-files-budget", "5000"]).expect("parse");
    let request = run_request(&args);
    assert!(!request.changed_files);
    assert!(!request.requested_new() && !request.ledger_enabled());
}

/// #9195: a local diff has no head SHA; `--changed-files` reports the row
/// `unavailable` and the review runs.
#[serial_test::serial]
#[tokio::test]
async fn run_changed_files_on_a_local_diff_is_unavailable() {
    let (outcome, wants_ledger) = local_run(&["--changed-files"]).await;
    assert!(wants_ledger);
    let row = outcome
        .context_sources
        .iter()
        .find(|r| r.source == "changed_files")
        .unwrap_or_else(|| panic!("no changed_files row: {:?}", outcome.context_sources));
    assert_eq!(row.state, SourceState::Unavailable);
    assert!(
        row.detail
            .as_deref()
            .unwrap_or("")
            .contains("no PR head SHA")
    );
}

/// #9196: `--symbol-context` sets the flag, a new input that turns the
/// ledger on; off by default.
#[test]
fn run_symbol_context_flag_sets_the_request() {
    let args = RunArgs::try_parse_from(["run", "--symbol-context"]).expect("parse");
    let request = run_request(&args);
    assert!(request.symbol_context && request.requested_new() && request.ledger_enabled());
    let args = RunArgs::try_parse_from(["run"]).expect("parse");
    assert!(!run_request(&args).symbol_context);
    assert!(RunArgs::try_parse_from(["run", "--symbol-context=yes"]).is_err());
}

/// #9197 B2b (AC13): `--fetch-linked-issues` parses, defaults off and turns
/// the ledger on.
#[test]
fn run_fetch_linked_issues_flag_sets_the_request() {
    let args = RunArgs::try_parse_from(["run", "--fetch-linked-issues"]).expect("parse");
    let request = run_request(&args);
    assert!(request.fetch_linked_issues && request.requested_new() && request.ledger_enabled());
    let args = RunArgs::try_parse_from(["run"]).expect("parse");
    assert!(!run_request(&args).fetch_linked_issues);
    assert!(RunArgs::try_parse_from(["run", "--fetch-linked-issues=yes"]).is_err());
}
