//! `run --issue-docs-file` (#9197).
//!
//! Why: the flag reads third-party JSON from disk, so it must be bounded,
//! parsed by the same strict rules as the MCP parameter, fail the run before
//! any network call when bad, and work on a local diff.
//! What: parses `RunArgs` through clap, reads the request the way `cmd_run`
//! does, and drives one local-diff review under a capturing LLM. No network:
//! `TRUSTY_SEARCH_SOCKET` is pinned to a missing path for the review.
//! Test: this module IS the tests.

use std::sync::{Arc, Mutex};

use clap::Parser as _;
use trusty_review::{
    config::{ReviewConfig, constants::MAX_ISSUE_DOCS_FILE_BYTES},
    integrations::NullSearchClient,
    llm::{LlmError, LlmProvider, LlmRequest, LlmResponse},
    pipeline::{DiffSource, ReviewDeps, ReviewOptions, run_review_with},
};

use super::{RunArgs, caller_context_from_args, run_input, run_request_with_issue_docs};

fn args_with_file(path: &std::path::Path) -> RunArgs {
    let path = path.display().to_string();
    RunArgs::try_parse_from(["run", "--issue-docs-file", path.as_str()]).expect("parse")
}

/// #9197: a valid file becomes the request's docs, ids normalised, and turns
/// the context-source report on; no flag leaves the request without docs.
#[test]
fn issue_docs_file_parses_and_reports_context() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("docs.json");
    std::fs::write(&file, r##"[{"id": "42", "title": "T", "body": "b"}]"##).expect("write");
    let request = run_request_with_issue_docs(&args_with_file(&file)).expect("reads");
    let docs = request.issue_docs.as_deref().expect("docs");
    assert_eq!(docs[0].id, "#42");
    assert!(request.ledger_enabled());

    let plain = RunArgs::try_parse_from(["run"]).expect("parse");
    let request = run_request_with_issue_docs(&plain).expect("no file");
    assert!(request.issue_docs.is_none() && !request.ledger_enabled());
}

/// #9197: a file over 256 KiB is refused, naming the flag and the path.
#[test]
fn issue_docs_file_over_256_kib_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    let file = dir.path().join("big.json");
    let cap = usize::try_from(MAX_ISSUE_DOCS_FILE_BYTES).expect("cap fits usize");
    let body = "a".repeat(cap);
    std::fs::write(&file, format!(r#"[{{"id": "1", "body": "{body}"}}]"#)).expect("write");
    let err = run_request_with_issue_docs(&args_with_file(&file)).expect_err("over the cap");
    let msg = format!("{err:#}");
    assert!(
        msg.contains("--issue-docs-file") && msg.contains("big.json"),
        "{msg}"
    );
    assert!(msg.contains("cap"), "{msg}");
}

/// #9197: the file is parsed as strictly as the MCP parameter; a JIRA-shaped
/// id or a non-JSON file fails the run.
#[test]
fn issue_docs_file_with_a_jira_id_is_refused() {
    let dir = tempfile::tempdir().expect("tempdir");
    for (name, text, needle) in [
        ("jira.json", r#"[{"id": "PROJ-12", "body": "b"}]"#, "'id'"),
        ("prose.json", "not json", "--issue-docs-file"),
    ] {
        let file = dir.path().join(name);
        std::fs::write(&file, text).expect("write");
        let err = run_request_with_issue_docs(&args_with_file(&file)).expect_err(name);
        let msg = format!("{err:#}");
        assert!(msg.contains(needle) && msg.contains(name), "{msg}");
    }
}

/// APPROVE stub that records each reviewer prompt's user text.
struct Capture(Mutex<Vec<String>>);

#[async_trait::async_trait]
impl LlmProvider for Capture {
    fn name(&self) -> &str {
        "capture-9197"
    }

    async fn complete(&self, req: LlmRequest) -> Result<LlmResponse, LlmError> {
        let user: String = req.messages.iter().map(|m| m.content.as_str()).collect();
        if let Ok(mut seen) = self.0.lock() {
            seen.push(user);
        }
        Ok(LlmResponse {
            text: "```json\n{\"verdict\":\"APPROVE\",\"summary\":\"ok\",\"findings\":[]}\n```"
                .into(),
            model: req.model.clone(),
            input_tokens: 1,
            output_tokens: 1,
            latency_ms: 0,
            cost_usd: 0.0,
            finish_reason: None,
        })
    }
}

/// Restores `TRUSTY_SEARCH_SOCKET` on drop; only for serial tests.
struct SocketPin(Option<std::ffi::OsString>);

impl Drop for SocketPin {
    fn drop(&mut self) {
        let key = trusty_common::search_rpc::TRUSTY_SEARCH_SOCKET_ENV;
        // SAFETY: the test holding this pin is `#[serial_test::serial]`.
        unsafe {
            match self.0.take() {
                Some(v) => std::env::set_var(key, v),
                None => std::env::remove_var(key),
            }
        }
    }
}

/// #9197: `run --local-diff` with `--issue-docs-file` carries the doc to the
/// reviewer prompt; issue docs need no GitHub PR.
#[serial_test::serial]
#[tokio::test]
async fn local_diff_run_carries_issue_docs_to_the_reviewer() {
    let dir = tempfile::tempdir().expect("tempdir");
    let diff = dir.path().join("change.diff");
    std::fs::write(
        &diff,
        "diff --git a/src/a.rs b/src/a.rs\n--- a/src/a.rs\n+++ b/src/a.rs\n\
         @@ -1 +1 @@\n-fn a() {}\n+fn a() { println!(\"a\"); }\n",
    )
    .expect("write diff");
    let docs = dir.path().join("docs.json");
    std::fs::write(&docs, r##"[{"id": "#5", "body": "CANARY_9197_RUN"}]"##).expect("write");
    let (diff_arg, docs_arg) = (diff.display().to_string(), docs.display().to_string());
    let argv = [
        "run",
        "--local-diff",
        diff_arg.as_str(),
        "--issue-docs-file",
        docs_arg.as_str(),
        "--no-log",
        "--json",
    ];
    let args = RunArgs::try_parse_from(argv).expect("parse");
    let request = run_request_with_issue_docs(&args).expect("readable docs");
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
    let llm = Arc::new(Capture(Mutex::new(Vec::new())));
    let deps = ReviewDeps {
        llm: llm.clone(),
        verifier: None,
        search: Arc::new(NullSearchClient::new("no search in #9197 tests")),
        analyze: None,
        dedup: None,
    };
    let mut config = ReviewConfig::load(None);
    config.search_url = "http://127.0.0.1:9".into();
    config.context.require_search = Some(false);
    config.context.require_analyze = false;
    config.log_dir = dir.path().to_path_buf();
    let outcome = run_review_with(&config, input, deps, ReviewOptions::new(request)).await;

    let prompts = llm.0.lock().map(|p| p.clone()).unwrap_or_default();
    let reviewer = prompts.first().expect("the reviewer must be called");
    assert!(reviewer.contains("### Issue #5 — (no title)"), "{reviewer}");
    assert!(reviewer.contains("CANARY_9197_RUN"), "{reviewer}");
    // The ledger is on: the caller row (no text flags), then the issues row;
    // #9194: then every other row, the inputs not asked for `not_requested`.
    let names: Vec<&str> = outcome
        .context_sources
        .iter()
        .map(|r| r.source.as_str())
        .collect();
    assert_eq!(&names[1..3], ["caller_context", "issues"], "{names:?}");
    assert_eq!(names.len(), 9, "{names:?}"); // #9195: `changed_files` is the 9th
}

// #9194: `--report-context`, reusing this module's LLM capture and socket pin.
#[path = "run_report_context_tests.rs"]
mod report_context;
