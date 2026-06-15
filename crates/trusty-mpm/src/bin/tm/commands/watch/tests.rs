//! Unit tests for the `tm watch` module (label filtering, project/owner-repo
//! parsing, dry-run no-op, listen dedup, config-default + flag-override
//! precedence, and the gh discovery seam).
//!
//! Why: `tm watch` drives real execution against real repos, so its decision
//! logic — which issues match, how settings resolve, whether dry-run spawns
//! anything, and how listen de-duplicates — must be pinned by fast, network-free
//! tests using the `CommandRunner`/`IssueLister` seams.
//! What: groups tests by concern: github parsing/filtering + the gh-backed
//! lister, args precedence + project resolution, the dispatch task/branch/dry-run
//! invariants, the safety-gate mapping, the board-repo resolver, and the listen
//! dedup core.
//! Test: this file IS the test surface; run via `cargo test -p trusty-mpm`.

use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::Rc;

use super::args::{DEFAULT_INTERVAL_SECS, DEFAULT_LABEL, RawWatchArgs, resolve};
use super::dispatch::{DispatchMode, build_watch_task, dispatch_issue, watch_branch_name};
use super::github::{
    GhIssueLister, IssueLister, IssueState, MatchedIssue, filter_by_label, parse_issues,
};
use super::listen::{select_new_issues, should_warn};
use super::{BoardRepo, dispatch_mode, resolve_board_repo};

use crate::commands::ticket::runner::{CommandOutput, CommandRunner};
use trusty_mpm::core::trusty_tools_config::WatchConfig;

use trusty_mpm::runtime::RuntimeKind;

// --- shared fakes ----------------------------------------------------------

/// Recorded `(program, args)` invocations, shared across a move into a lister.
///
/// Why: a named alias keeps the `Rc<RefCell<…>>` handle off the `type_complexity`
/// lint while documenting that it is a shared, mutable call log.
/// What: a reference-counted, interior-mutable vector of `(program, args)` pairs.
/// Test: used by `FakeRunner` and `gh_list_passes_label_and_state_flags`.
type CallLog = Rc<RefCell<Vec<(String, Vec<String>)>>>;

/// A scripted [`CommandRunner`] returning queued outputs and recording calls.
///
/// Why: drives the gh-backed lister + board resolver without spawning processes.
/// What: pops queued [`CommandOutput`]s FIFO; records `(program, args)` per call.
/// Test: used by `gh_list_*` and `resolve_board_repo_*`.
struct FakeRunner {
    outputs: RefCell<Vec<CommandOutput>>,
    /// Shared so a test can keep a handle after the runner is moved into a lister.
    calls: CallLog,
}

impl FakeRunner {
    fn new(outputs: Vec<CommandOutput>) -> Self {
        Self {
            outputs: RefCell::new(outputs),
            calls: Rc::new(RefCell::new(Vec::new())),
        }
    }

    /// A cloneable handle to the recorded calls (read after a move).
    fn calls_handle(&self) -> CallLog {
        Rc::clone(&self.calls)
    }
}

impl CommandRunner for FakeRunner {
    fn run(&self, program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        self.calls.borrow_mut().push((
            program.to_string(),
            args.iter().map(|a| a.to_string()).collect(),
        ));
        let mut outs = self.outputs.borrow_mut();
        if outs.is_empty() {
            anyhow::bail!("FakeRunner exhausted")
        }
        Ok(outs.remove(0))
    }
}

fn ok_out(stdout: &str) -> CommandOutput {
    CommandOutput {
        success: true,
        stdout: stdout.to_string(),
        stderr: String::new(),
    }
}

fn fail_out(stderr: &str) -> CommandOutput {
    CommandOutput {
        success: false,
        stdout: String::new(),
        stderr: stderr.to_string(),
    }
}

fn issue(number: u64, labels: &[&str]) -> MatchedIssue {
    MatchedIssue {
        number,
        title: format!("Issue {number}"),
        body: "body".to_string(),
        labels: labels.iter().map(|s| s.to_string()).collect(),
        url: format!("https://github.com/o/r/issues/{number}"),
    }
}

// --- github: parsing + filtering ------------------------------------------

#[test]
fn issue_state_default_is_open() {
    assert_eq!(IssueState::default(), IssueState::Open);
}

#[test]
fn issue_state_as_gh() {
    assert_eq!(IssueState::Open.as_gh(), "open");
    assert_eq!(IssueState::All.as_gh(), "all");
}

#[test]
fn parse_issues_basic() {
    let json = r#"[
        {"number":1,"title":"A","body":"do a","labels":[{"name":"tm-agent"}],"url":"https://x/1"},
        {"number":2,"title":"B","body":"","labels":[{"name":"bug"},{"name":"tm-agent"}],"url":"https://x/2"}
    ]"#;
    let issues = parse_issues(json).expect("parse");
    assert_eq!(issues.len(), 2);
    assert_eq!(issues[0].number, 1);
    assert_eq!(issues[0].title, "A");
    assert_eq!(issues[0].body, "do a");
    assert_eq!(issues[0].labels, vec!["tm-agent".to_string()]);
    assert_eq!(issues[0].url, "https://x/1");
    assert_eq!(
        issues[1].labels,
        vec!["bug".to_string(), "tm-agent".to_string()]
    );
}

#[test]
fn parse_issues_empty() {
    assert!(parse_issues("[]").unwrap().is_empty());
    assert!(parse_issues("   ").unwrap().is_empty());
    assert!(parse_issues("").unwrap().is_empty());
}

#[test]
fn parse_issues_rejects_garbage() {
    assert!(parse_issues("not json").is_err());
}

#[test]
fn filter_by_label_keeps_only_matching() {
    let issues = vec![
        issue(1, &["tm-agent"]),
        issue(2, &["bug"]),
        issue(3, &["enhancement", "tm-agent"]),
    ];
    let kept = filter_by_label(issues, "tm-agent");
    let nums: Vec<u64> = kept.iter().map(|i| i.number).collect();
    assert_eq!(nums, vec![1, 3]);
}

#[test]
fn filter_by_label_case_insensitive() {
    let issues = vec![issue(1, &["TM-Agent"]), issue(2, &["bug"])];
    let kept = filter_by_label(issues, "tm-agent");
    assert_eq!(kept.len(), 1);
    assert_eq!(kept[0].number, 1);
}

#[test]
fn filter_by_label_empty_when_none_match() {
    let issues = vec![issue(1, &["bug"]), issue(2, &["chore"])];
    assert!(filter_by_label(issues, "tm-agent").is_empty());
}

// --- github: gh-backed lister ---------------------------------------------

#[test]
fn gh_list_parses_and_filters() {
    // gh returns two issues; both carry the label server-side, so both survive
    // the defensive re-filter.
    let json = r#"[
        {"number":10,"title":"X","body":"b","labels":[{"name":"tm-agent"}],"url":"https://x/10"},
        {"number":11,"title":"Y","body":"b","labels":[{"name":"tm-agent"}],"url":"https://x/11"}
    ]"#;
    let lister = GhIssueLister::new(FakeRunner::new(vec![ok_out(json)]));
    let got = lister
        .list("o/r", "tm-agent", IssueState::Open)
        .expect("list");
    assert_eq!(got.len(), 2);
    assert_eq!(got[0].number, 10);
}

#[test]
fn gh_list_defensive_filter_drops_unmatched() {
    // A loose backend returns an issue WITHOUT the label; the defensive filter
    // must drop it so only labelled issues ever reach dispatch.
    let json = r#"[
        {"number":10,"title":"X","body":"b","labels":[{"name":"tm-agent"}],"url":"https://x/10"},
        {"number":12,"title":"Z","body":"b","labels":[{"name":"unrelated"}],"url":"https://x/12"}
    ]"#;
    let lister = GhIssueLister::new(FakeRunner::new(vec![ok_out(json)]));
    let got = lister
        .list("o/r", "tm-agent", IssueState::Open)
        .expect("list");
    assert_eq!(got.len(), 1);
    assert_eq!(got[0].number, 10);
}

#[test]
fn gh_list_surfaces_gh_failure() {
    let lister = GhIssueLister::new(FakeRunner::new(vec![fail_out("not logged in")]));
    let err = lister
        .list("o/r", "tm-agent", IssueState::Open)
        .unwrap_err()
        .to_string();
    assert!(err.contains("gh issue list"), "got: {err}");
    assert!(err.contains("not logged in"), "got: {err}");
}

#[test]
fn gh_list_passes_label_and_state_flags() {
    let runner = FakeRunner::new(vec![ok_out("[]")]);
    let calls = runner.calls_handle();
    let lister = GhIssueLister::new(runner);
    let _ = lister.list("acme/widget", "ship-it", IssueState::All);
    let recorded = calls.borrow();
    let args = &recorded[0].1;
    assert!(args.contains(&"-R".to_string()));
    assert!(args.contains(&"acme/widget".to_string()));
    assert!(args.contains(&"--label".to_string()));
    assert!(args.contains(&"ship-it".to_string()));
    assert!(args.contains(&"--state".to_string()));
    assert!(args.contains(&"all".to_string()));
}

// --- args: project resolution + precedence --------------------------------

#[test]
fn resolve_project_direct_owner_repo() {
    let raw = RawWatchArgs {
        project: "bobmatnyc/trusty-tools".into(),
        ..Default::default()
    };
    let cfg = WatchConfig::default();
    let r = resolve(&raw, &cfg).expect("resolve");
    assert_eq!(r.repo, "bobmatnyc/trusty-tools");
}

#[test]
fn resolve_project_name_uses_config() {
    // A bare name (not owner/repo) falls back to the configured repo.
    let raw = RawWatchArgs {
        project: "myboard".into(),
        ..Default::default()
    };
    let cfg = WatchConfig {
        repo: Some("acme/widget".into()),
        ..Default::default()
    };
    let r = resolve(&raw, &cfg).expect("resolve");
    assert_eq!(r.repo, "acme/widget");
}

#[test]
fn resolve_project_unresolvable_errors() {
    let raw = RawWatchArgs {
        project: "justaname".into(),
        ..Default::default()
    };
    let cfg = WatchConfig::default();
    let err = resolve(&raw, &cfg).unwrap_err().to_string();
    assert!(err.contains("could not resolve project"), "got: {err}");
    assert!(err.contains("owner/repo"), "got: {err}");
}

#[test]
fn resolve_cli_overrides_config() {
    let raw = RawWatchArgs {
        project: "o/r".into(),
        label: Some("cli-label".into()),
        interval_secs: Some(5),
        state: Some(IssueState::All),
    };
    let cfg = WatchConfig {
        repo: None,
        label: Some("config-label".into()),
        interval_secs: Some(999),
    };
    let r = resolve(&raw, &cfg).expect("resolve");
    assert_eq!(r.label, "cli-label");
    assert_eq!(r.interval_secs, 5);
    assert_eq!(r.state, IssueState::All);
}

#[test]
fn resolve_config_used_when_no_cli() {
    let raw = RawWatchArgs {
        project: "o/r".into(),
        ..Default::default()
    };
    let cfg = WatchConfig {
        repo: None,
        label: Some("config-label".into()),
        interval_secs: Some(120),
    };
    let r = resolve(&raw, &cfg).expect("resolve");
    assert_eq!(r.label, "config-label");
    assert_eq!(r.interval_secs, 120);
}

#[test]
fn resolve_uses_default_label_when_unset() {
    let raw = RawWatchArgs {
        project: "o/r".into(),
        ..Default::default()
    };
    let r = resolve(&raw, &WatchConfig::default()).expect("resolve");
    assert_eq!(r.label, DEFAULT_LABEL);
}

#[test]
fn resolve_uses_default_interval_when_unset() {
    let raw = RawWatchArgs {
        project: "o/r".into(),
        ..Default::default()
    };
    let r = resolve(&raw, &WatchConfig::default()).expect("resolve");
    assert_eq!(r.interval_secs, DEFAULT_INTERVAL_SECS);
}

#[test]
fn resolve_zero_interval_falls_back_to_default() {
    // A zero interval (CLI or config) is nonsense; it must fall back to default.
    let raw = RawWatchArgs {
        project: "o/r".into(),
        interval_secs: Some(0),
        ..Default::default()
    };
    let r = resolve(&raw, &WatchConfig::default()).expect("resolve");
    assert_eq!(r.interval_secs, DEFAULT_INTERVAL_SECS);
}

#[test]
fn resolve_state_defaults_to_open() {
    let raw = RawWatchArgs {
        project: "o/r".into(),
        ..Default::default()
    };
    let r = resolve(&raw, &WatchConfig::default()).expect("resolve");
    assert_eq!(r.state, IssueState::Open);
}

// --- dispatch: task / branch / dry-run ------------------------------------

#[test]
fn watch_branch_name_is_namespaced() {
    assert_eq!(watch_branch_name(&issue(42, &["tm-agent"])), "tm-watch/42");
}

#[test]
fn build_watch_task_includes_branch_and_close() {
    let task = build_watch_task(&issue(7, &["tm-agent"]), "tm-watch/7", "main");
    assert!(task.contains("issue #7"));
    assert!(task.contains("tm-watch/7"));
    assert!(task.contains("default branch `main`"));
    assert!(task.contains("Closes #7"));
    assert!(task.contains("pull request"));
}

#[test]
fn build_watch_task_handles_empty_body() {
    let mut i = issue(8, &["tm-agent"]);
    i.body = "   ".into();
    let task = build_watch_task(&i, "tm-watch/8", "master");
    assert!(task.contains("(no body provided)"));
    assert!(task.contains("default branch `master`"));
}

#[tokio::test]
async fn dispatch_dry_run_spawns_nothing() {
    // The daemon URL is deliberately unroutable; a dry-run must NOT make any HTTP
    // call, so this returns Ok(false) without erroring on the bad URL.
    let client = reqwest::Client::new();
    let i = issue(99, &["tm-agent"]);
    let spawned = dispatch_issue(
        &client,
        "http://127.0.0.1:1", // would fail if contacted
        "https://github.com/o/r",
        "main",
        &i,
        DispatchMode::DryRun,
        RuntimeKind::ClaudeCode,
    )
    .await
    .expect("dry-run must not error");
    assert!(!spawned, "dry-run must report nothing spawned");
}

// --- safety gate -----------------------------------------------------------

#[test]
fn dispatch_mode_defaults_to_dry_run() {
    // Neither flag set → dry-run (the safe default).
    assert_eq!(dispatch_mode(false, false), DispatchMode::DryRun);
}

#[test]
fn dispatch_mode_execute_opts_in() {
    assert_eq!(dispatch_mode(true, false), DispatchMode::Execute);
}

#[test]
fn dispatch_mode_dry_run_wins_over_execute() {
    // If both are passed, safety wins.
    assert_eq!(dispatch_mode(true, true), DispatchMode::DryRun);
}

#[test]
fn dispatch_mode_explicit_dry_run() {
    assert_eq!(dispatch_mode(false, true), DispatchMode::DryRun);
}

// --- board-repo resolver ---------------------------------------------------

#[test]
fn resolve_board_repo_threads_default_branch() {
    let runner = FakeRunner::new(vec![ok_out("trunk")]);
    // No `github:` config → default host `github.com`.
    let board = resolve_board_repo(&runner, "acme/widget", None).expect("resolve");
    assert_eq!(
        board,
        BoardRepo {
            clone_url: "https://github.com/acme/widget".into(),
            default_branch: "trunk".into(),
        }
    );
}

#[test]
fn resolve_board_repo_falls_back_to_main() {
    let runner = FakeRunner::new(vec![ok_out("")]);
    let board = resolve_board_repo(&runner, "acme/widget", None).expect("resolve");
    assert_eq!(board.default_branch, "main");
}

#[test]
fn resolve_board_repo_surfaces_failure() {
    let runner = FakeRunner::new(vec![fail_out("no such repo")]);
    let err = resolve_board_repo(&runner, "acme/widget", None)
        .unwrap_err()
        .to_string();
    assert!(err.contains("no such repo"), "got: {err}");
}

#[test]
fn resolve_board_repo_uses_configured_host() {
    use trusty_mpm::core::trusty_tools_config::GithubConfig;
    let runner = FakeRunner::new(vec![ok_out("main")]);
    let github = GithubConfig {
        host: Some("github.example.com".into()),
        ..Default::default()
    };
    let board = resolve_board_repo(&runner, "acme/widget", Some(&github)).expect("resolve");
    assert_eq!(
        board.clone_url, "https://github.example.com/acme/widget",
        "clone URL must honour the configured GHE host"
    );
}

// --- listen: dedup core ----------------------------------------------------

#[test]
fn select_new_issues_records_new() {
    let mut seen = HashSet::new();
    let batch = vec![issue(1, &["tm-agent"]), issue(2, &["tm-agent"])];
    let fresh = select_new_issues(&batch, &mut seen);
    assert_eq!(fresh.len(), 2);
    assert!(seen.contains(&1));
    assert!(seen.contains(&2));
}

#[test]
fn select_new_issues_filters_seen() {
    let mut seen = HashSet::new();
    seen.insert(1);
    let batch = vec![issue(1, &["tm-agent"]), issue(2, &["tm-agent"])];
    let fresh = select_new_issues(&batch, &mut seen);
    // Only #2 is new; #1 was already seen.
    let nums: Vec<u64> = fresh.iter().map(|i| i.number).collect();
    assert_eq!(nums, vec![2]);
}

#[test]
fn select_new_issues_dedups_within_batch() {
    // A duplicate number within a single batch must only be returned once.
    let mut seen = HashSet::new();
    let batch = vec![issue(3, &["tm-agent"]), issue(3, &["tm-agent"])];
    let fresh = select_new_issues(&batch, &mut seen);
    assert_eq!(fresh.len(), 1);
}

#[test]
fn select_new_issues_second_poll_is_empty() {
    // Simulate two polls returning the same issues: the second yields nothing.
    let mut seen = HashSet::new();
    let batch = vec![issue(1, &["tm-agent"]), issue(2, &["tm-agent"])];
    let first = select_new_issues(&batch, &mut seen);
    assert_eq!(first.len(), 2);
    let second = select_new_issues(&batch, &mut seen);
    assert!(second.is_empty(), "re-poll of same issues must be empty");
}

#[test]
fn should_warn_only_for_execute() {
    // The in-memory-dedup restart warning must fire only when we actually spawn
    // work (Execute); dry-run re-listing is harmless and must stay quiet.
    assert!(should_warn(DispatchMode::Execute));
    assert!(!should_warn(DispatchMode::DryRun));
}
