//! Tests for `tm issue next`: the ranking rule, the label mapping, the goal
//! lookup, the cache and the fail-closed rate limit.

use std::cell::{Cell, RefCell};
use std::collections::{BTreeMap, BTreeSet};

use super::fetch::{CACHE_TTL_SECS, GhSource, IntakeSource, cached_snapshot, is_rate_limit};
use super::goals::{Goal, parse_goal};
use super::model::{BoardStatus, IssueRow, LabelMap, LabelOverride, RepoSignal, Snapshot, rank};
use super::{NextError, load_override, render};
use crate::commands::issue::board::client::BoardClient;
use crate::commands::ticket::runner::{CommandOutput, CommandRunner};

const AC: &str = "Symptom.\n\n## Acceptance criteria\n- the command prints the list\n";

fn issue(number: u64, created: &str, labels: &[&str]) -> IssueRow {
    IssueRow {
        number,
        title: format!("issue {number}"),
        created_at: created.to_string(),
        body: AC.to_string(),
        labels: labels.iter().map(|l| l.to_string()).collect(),
        assignees: Vec::new(),
    }
}

fn board(statuses: &[(u64, &str)]) -> BoardStatus {
    BoardStatus {
        number: 45,
        statuses: statuses.iter().map(|(n, s)| (*n, s.to_string())).collect(),
    }
}

fn snap(repo_labels: &[&str], boards: Vec<BoardStatus>, issues: Vec<IssueRow>) -> Snapshot {
    Snapshot {
        repo: "o/r".into(),
        viewer: "me".into(),
        repo_labels: repo_labels.iter().map(|l| l.to_string()).collect(),
        boards,
        issues,
    }
}

fn numbers(s: &Snapshot, goal: Option<&Goal>) -> Vec<u64> {
    let map = LabelMap::detect(&s.repo_labels, None);
    rank(s, &map, goal, 10)
        .picks
        .iter()
        .map(|p| p.number)
        .collect()
}

const READY: &str = "Ready for Development";

#[test]
fn ranking_orders_by_goal_then_priority_then_age() {
    let s = snap(
        &["P0", "P1", "P2"],
        vec![board(&[
            (1, READY),
            (2, READY),
            (3, READY),
            (4, READY),
            (5, READY),
        ])],
        vec![
            issue(1, "2026-01-01T00:00:00Z", &[]),
            issue(2, "2026-02-01T00:00:00Z", &["P1"]),
            issue(3, "2026-01-15T00:00:00Z", &["P1"]),
            issue(4, "2026-03-01T00:00:00Z", &["P2"]),
            issue(5, "2026-04-01T00:00:00Z", &["P0"]),
        ],
    );
    assert_eq!(numbers(&s, None), vec![5, 3, 2, 4, 1]);
    let goal = Goal {
        id: "G01".into(),
        issues: BTreeSet::from([4]),
    };
    assert_eq!(numbers(&s, Some(&goal)), vec![4, 5, 3, 2, 1]);
    let map = LabelMap::detect(&s.repo_labels, None);
    let out = rank(&s, &map, Some(&goal), 2);
    assert_eq!(out.picks.len(), 2);
    assert!(
        out.picks[0].reason.contains("serves goal G01"),
        "{:?}",
        out.picks[0]
    );
    assert!(
        out.picks[1].reason.contains("priority P0"),
        "{:?}",
        out.picks[1]
    );
}

#[test]
fn an_issue_without_acceptance_criteria_is_never_taken() {
    let mut bare = issue(7, "2026-01-01T00:00:00Z", &["P0"]);
    bare.body = "Symptom only.\n\nAcceptance criteria: TBD\n".into();
    let s = snap(
        &["P0"],
        vec![board(&[(7, READY), (8, READY)])],
        vec![bare, issue(8, "2026-02-01T00:00:00Z", &[])],
    );
    let map = LabelMap::detect(&s.repo_labels, None);
    let out = rank(&s, &map, None, 10);
    assert_eq!(
        out.picks.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![8]
    );
    assert_eq!(out.skipped.no_acceptance_criteria, vec![7]);
}

#[test]
fn an_issue_assigned_to_someone_else_is_skipped() {
    let mut theirs = issue(1, "2026-01-01T00:00:00Z", &[]);
    theirs.assignees = vec!["other".into()];
    let mut mine = issue(2, "2026-01-02T00:00:00Z", &[]);
    mine.assignees = vec!["Me".into()];
    let s = snap(
        &[],
        vec![board(&[(1, READY), (2, READY)])],
        vec![theirs, mine],
    );
    let map = LabelMap::detect(&s.repo_labels, None);
    let out = rank(&s, &map, None, 10);
    assert_eq!(
        out.picks.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![2]
    );
    assert_eq!(out.skipped.assigned_to_others, 1);
}

#[test]
fn a_label_only_repo_takes_by_label() {
    let s = snap(
        &["status:ready", "status:in-progress", "bug"],
        vec![],
        vec![
            issue(1, "2026-01-01T00:00:00Z", &["status:ready", "bug"]),
            issue(2, "2026-01-02T00:00:00Z", &["status:in-progress"]),
            issue(3, "2026-01-03T00:00:00Z", &[]),
        ],
    );
    let map = LabelMap::detect(&s.repo_labels, None);
    let out = rank(&s, &map, None, 10);
    assert_eq!(out.signal, RepoSignal::Labels);
    assert_eq!(out.picks.len(), 1);
    assert_eq!(out.picks[0].signal, "label status:ready");
    assert_eq!(out.picks[0].types, vec!["bug".to_string()]);
    assert_eq!(out.skipped.not_ready, 2);
}

#[test]
fn a_board_only_repo_takes_by_status() {
    let s = snap(
        &["status:in-progress"],
        vec![board(&[(1, READY), (2, "Backlog")])],
        vec![
            issue(1, "2026-01-01T00:00:00Z", &[]),
            issue(2, "2026-01-02T00:00:00Z", &[]),
            issue(3, "2026-01-03T00:00:00Z", &[]),
        ],
    );
    let map = LabelMap::detect(&s.repo_labels, None);
    let out = rank(&s, &map, None, 10);
    assert_eq!(out.signal, RepoSignal::Board);
    assert_eq!(out.picks.len(), 1);
    assert_eq!(out.picks[0].signal, "board Status: Ready for Development");
    assert!(out.repair_candidates.is_empty());
}

#[test]
fn a_repo_with_no_signal_takes_nothing() {
    let s = snap(
        &["bug", "P1"],
        vec![],
        vec![issue(1, "2026-01-01T00:00:00Z", &["P1"])],
    );
    let map = LabelMap::detect(&s.repo_labels, None);
    let out = rank(&s, &map, None, 10);
    assert_eq!(out.signal, RepoSignal::None);
    assert!(out.picks.is_empty());
    assert!(
        render(&out, false).contains("no intake signal"),
        "{}",
        render(&out, false)
    );
}

#[test]
fn conflicting_signals_are_taken_and_listed_for_repair() {
    let s = snap(
        &["status:ready", "status:in-progress"],
        vec![board(&[(1, READY), (2, "In Development")])],
        vec![
            // Board ready, label says in progress.
            issue(1, "2026-01-01T00:00:00Z", &["status:in-progress"]),
            // Label ready, board says In Development.
            issue(2, "2026-01-02T00:00:00Z", &["status:ready"]),
            // Both ready: no conflict.
            issue(3, "2026-01-03T00:00:00Z", &["status:ready"]),
        ],
    );
    let map = LabelMap::detect(&s.repo_labels, None);
    let out = rank(&s, &map, None, 10);
    assert_eq!(
        out.picks.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![1, 2, 3]
    );
    assert_eq!(out.repair_candidates, vec![1, 2]);
    assert!(out.picks[0].repair_candidate && !out.picks[2].repair_candidate);
    let text = render(&out, false);
    assert!(text.contains("tm issue repair`): #1 #2"), "{text}");
}

#[test]
fn labels_are_detected_from_repo_conventions() {
    let labels: Vec<String> = [
        "status:ready",
        "Status:In-Progress",
        "P0",
        "priority:low",
        "bug",
    ]
    .iter()
    .map(|s| s.to_string())
    .collect();
    let map = LabelMap::detect(&labels, None);
    assert_eq!(map.ready, vec!["status:ready".to_string()]);
    assert_eq!(map.not_ready, vec!["Status:In-Progress".to_string()]);
    assert_eq!(map.priority.get("p0"), Some(&0));
    assert_eq!(map.priority.get("priority:low"), Some(&3));
}

#[test]
fn a_repo_override_replaces_the_detected_mapping() {
    let ovr = LabelOverride {
        ready_labels: Some(vec!["triaged".into()]),
        ready_status: Some(vec!["Todo".into()]),
        priority_labels: Some(BTreeMap::from([("urgent!".to_string(), 0)])),
        not_ready_labels: None,
    };
    let s = snap(
        &["triaged", "urgent!"],
        vec![board(&[(2, "Todo")])],
        vec![
            issue(1, "2026-01-01T00:00:00Z", &["triaged"]),
            issue(2, "2026-01-02T00:00:00Z", &["urgent!"]),
        ],
    );
    let map = LabelMap::detect(&s.repo_labels, Some(&ovr));
    let out = rank(&s, &map, None, 10);
    assert_eq!(
        out.picks.iter().map(|p| p.number).collect::<Vec<_>>(),
        vec![2, 1]
    );
}

#[test]
fn a_malformed_override_file_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("issue-next.yaml");
    std::fs::write(&path, "repos:\n  o/r:\n    ready_lables: [x]\n").expect("write");
    assert!(matches!(
        load_override(&path, "o/r"),
        Err(NextError::Override { .. })
    ));
    std::fs::write(&path, "repos:\n  O/R:\n    ready_labels: [x]\n").expect("write");
    let ovr = load_override(&path, "o/r").expect("parses").expect("entry");
    assert_eq!(ovr.ready_labels, Some(vec!["x".to_string()]));
    assert!(
        load_override(&dir.path().join("absent.yaml"), "o/r")
            .expect("ok")
            .is_none()
    );
}

#[test]
fn a_goal_section_collects_its_issue_references() {
    let text = "# Goals\n## 1. Build (G01)\n- do #8436 then #8536.\n## 2. Other (G02)\n- #9\n";
    let goal = parse_goal(text, "ar-G01").expect("found");
    assert_eq!(goal.issues, BTreeSet::from([8436, 8536]));
    assert!(goal.serves(9, &["goal:g01".to_string()]));
    assert!(!goal.serves(9, &[]));
}

#[test]
fn an_unknown_goal_is_an_error() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("goals.md");
    std::fs::write(&path, "## 1. Build (G01)\n").expect("write");
    let err = super::goals::load_goal(&path, "G99").expect_err("unknown goal");
    assert!(matches!(err, NextError::Goals { .. }), "{err}");
    let err = super::goals::load_goal(&dir.path().join("none.md"), "G01").expect_err("no file");
    assert!(matches!(err, NextError::Goals { .. }), "{err}");
}

#[test]
fn the_text_report_names_signal_reason_and_repair() {
    let s = snap(
        &["P1"],
        vec![board(&[(1, READY)])],
        vec![issue(1, "2026-01-01T00:00:00Z", &["P1"])],
    );
    let out = rank(&s, &LabelMap::detect(&s.repo_labels, None), None, 10);
    let text = render(&out, true);
    assert!(text.contains("#1 issue 1"), "{text}");
    assert!(
        text.contains("signal: board Status: Ready for Development"),
        "{text}"
    );
    assert!(text.contains("reason: priority P1"), "{text}");
    assert!(text.contains("(cached)"), "{text}");
}

/// A source that counts its calls and answers a fixed result.
struct CountingSource {
    calls: Cell<usize>,
    answer: Result<Snapshot, String>,
}

impl IntakeSource for CountingSource {
    fn fetch(&self, _repo: &str) -> Result<Snapshot, NextError> {
        self.calls.set(self.calls.get() + 1);
        self.answer
            .clone()
            .map_err(|detail| super::fetch::classify(BoardErrorShim::err(detail)))
    }
}

/// Builds the board transport's error from a `gh` stderr text.
struct BoardErrorShim;

impl BoardErrorShim {
    fn err(detail: String) -> crate::commands::issue::board::error::BoardError {
        crate::commands::issue::board::error::BoardError::Command {
            what: "gh api graphql (IntakeIssues)".into(),
            detail,
        }
    }
}

fn one_issue_snap() -> Snapshot {
    snap(
        &[],
        vec![board(&[(1, READY)])],
        vec![issue(1, "2026-01-01T00:00:00Z", &[])],
    )
}

#[test]
fn a_fresh_cache_is_read_without_fetching() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cache/o-r.json");
    let source = CountingSource {
        calls: Cell::new(0),
        answer: Ok(one_issue_snap()),
    };
    let (_, cached) = cached_snapshot(&source, "o/r", Some(&path), 1_000, false).expect("first");
    assert!(!cached);
    let (hit, cached) = cached_snapshot(
        &source,
        "o/r",
        Some(&path),
        1_000 + CACHE_TTL_SECS - 1,
        false,
    )
    .expect("hit");
    assert!(cached);
    assert_eq!(hit, one_issue_snap());
    assert_eq!(source.calls.get(), 1);
}

#[test]
fn a_stale_cache_is_refetched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("o-r.json");
    let source = CountingSource {
        calls: Cell::new(0),
        answer: Ok(one_issue_snap()),
    };
    cached_snapshot(&source, "o/r", Some(&path), 1_000, false).expect("first");
    let (_, cached) =
        cached_snapshot(&source, "o/r", Some(&path), 1_000 + CACHE_TTL_SECS, false).expect("stale");
    assert!(!cached);
    assert_eq!(source.calls.get(), 2);
}

#[test]
fn a_rate_limit_fails_closed() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("o-r.json");
    let good = CountingSource {
        calls: Cell::new(0),
        answer: Ok(one_issue_snap()),
    };
    cached_snapshot(&good, "o/r", Some(&path), 1_000, false).expect("seed the cache");
    let limited = CountingSource {
        calls: Cell::new(0),
        answer: Err("API rate limit exceeded for user ID 1".into()),
    };
    // The stale cache must not stand in for the refused read.
    let err = cached_snapshot(&limited, "o/r", Some(&path), 1_000 + CACHE_TTL_SECS, false)
        .expect_err("rate limit is an error");
    assert!(matches!(err, NextError::RateLimited { .. }), "{err}");
    assert!(err.to_string().contains("no issues returned"), "{err}");
}

#[test]
fn rate_limit_texts_are_recognised() {
    assert!(is_rate_limit("API rate limit exceeded"));
    assert!(is_rate_limit("type: RATE_LIMITED"));
    assert!(!is_rate_limit("Could not resolve to a Repository"));
    let err = super::fetch::classify(BoardErrorShim::err("not found".into()));
    assert!(matches!(err, NextError::Fetch { .. }), "{err}");
}

/// A `gh` fake that answers each GraphQL operation by name and records them.
struct FakeGh {
    seen: RefCell<Vec<String>>,
    /// How the fake misbehaves; `Fault::None` answers every page well.
    fault: Fault,
}

/// One way a GraphQL reply can go wrong.
#[derive(Clone, Copy, PartialEq)]
enum Fault {
    None,
    /// Every reply is `{"data":{}}`.
    EmptyData,
    /// `repository` is null, with no `errors`.
    NullRepository,
    /// The issues reply has no `issues` connection.
    NoIssuesConnection,
    /// Page 2 of the board query fails.
    BoardPage2,
    /// Page 2 of the issues query fails.
    IssuesPage2,
}

impl FakeGh {
    fn new(fault: Fault) -> Self {
        Self {
            seen: RefCell::new(Vec::new()),
            fault,
        }
    }
}

impl CommandRunner for FakeGh {
    fn run(&self, _program: &str, args: &[&str]) -> anyhow::Result<CommandOutput> {
        let query = args
            .iter()
            .find(|a| a.starts_with("query="))
            .copied()
            .unwrap_or("");
        let op = query
            .trim_start_matches("query=query ")
            .split(['(', ' '])
            .next()
            .unwrap_or("")
            .to_string();
        let second_page = args.contains(&"cursor=c1");
        self.seen.borrow_mut().push(op.clone());
        let fault = self.fault;
        if fault == Fault::EmptyData {
            return Ok(CommandOutput {
                success: true,
                stdout: r#"{"data":{}}"#.into(),
                stderr: String::new(),
            });
        }
        if second_page
            && ((fault == Fault::BoardPage2 && op == "IntakeBoard")
                || (fault == Fault::IssuesPage2 && op == "IntakeIssues"))
        {
            anyhow::bail!("gh: HTTP 502 on page 2 of {op}");
        }
        let data = match op.as_str() {
            "IntakeMeta" if fault == Fault::NullRepository => {
                serde_json::json!({"viewer": {"login": "me"}, "repository": null})
            }
            "IntakeIssues" if fault == Fault::NoIssuesConnection => {
                serde_json::json!({"repository": {}})
            }
            "IntakeIssues" if fault == Fault::IssuesPage2 => {
                serde_json::json!({"repository": {"issues": {"pageInfo": {"hasNextPage": true, "endCursor": "c1"},
                "nodes": []}}})
            }
            "IntakeMeta" => {
                serde_json::json!({"viewer": {"login": "me"}, "repository": {"projectsV2": {"nodes": [
                {"id": "P1", "number": 45, "closed": false}, {"id": "P2", "number": 9, "closed": true}]}}})
            }
            "IntakeLabels" => {
                serde_json::json!({"repository": {"labels": {"pageInfo": {"hasNextPage": false},
                "nodes": [{"name": "P1"}]}}})
            }
            "IntakeBoard" => {
                let (num, next) = if second_page { (2, false) } else { (1, true) };
                serde_json::json!({"node": {"items": {"pageInfo": {"hasNextPage": next, "endCursor": "c1"},
                    "nodes": [{"content": {"__typename": "Issue", "number": num, "state": "OPEN",
                    "repository": {"nameWithOwner": "o/r"}}, "fieldValueByName": {"name": READY}},
                    {"content": {"__typename": "Issue", "number": 99, "state": "OPEN",
                    "repository": {"nameWithOwner": "o/other"}}, "fieldValueByName": {"name": READY}}]}}})
            }
            "IntakeIssues" => {
                serde_json::json!({"repository": {"issues": {"pageInfo": {"hasNextPage": false},
                "nodes": [{"number": 1, "title": "t", "createdAt": "2026-01-01T00:00:00Z", "body": AC,
                "labels": {"nodes": [{"name": "P1"}]}, "assignees": {"nodes": []}}]}}})
            }
            other => anyhow::bail!("unexpected operation {other}"),
        };
        Ok(CommandOutput {
            success: true,
            stdout: serde_json::json!({ "data": data }).to_string(),
            stderr: String::new(),
        })
    }
}

#[test]
fn status_is_read_in_one_batched_query_per_open_board() {
    let fake = FakeGh::new(Fault::None);
    let source = GhSource {
        client: BoardClient::new(&fake),
    };
    let snap = source.fetch("o/r").expect("fetch");
    // Two pages of the one open board; the closed board is not read, and no
    // call names a single issue.
    assert_eq!(
        *fake.seen.borrow(),
        vec![
            "IntakeMeta",
            "IntakeLabels",
            "IntakeBoard",
            "IntakeBoard",
            "IntakeIssues"
        ]
    );
    assert_eq!(snap.boards.len(), 1);
    assert_eq!(
        snap.boards[0].statuses.keys().copied().collect::<Vec<_>>(),
        vec![1, 2]
    );
    assert_eq!(snap.viewer, "me");
}

#[test]
fn a_rate_limited_graphql_answer_is_a_rate_limit_error() {
    struct Limited;
    impl CommandRunner for Limited {
        fn run(&self, _p: &str, _a: &[&str]) -> anyhow::Result<CommandOutput> {
            Ok(CommandOutput {
                success: true,
                stdout:
                    r#"{"errors":[{"type":"RATE_LIMITED","message":"API rate limit exceeded"}]}"#
                        .into(),
                stderr: String::new(),
            })
        }
    }
    let source = GhSource {
        client: BoardClient::new(&Limited),
    };
    let err = source.fetch("o/r").expect_err("limited");
    assert!(matches!(err, NextError::RateLimited { .. }), "{err}");
}

#[test]
fn cli_parses_issue_next_flags() {
    use crate::cli::{Cli, Command, IssueCmd};
    use clap::Parser;
    let cli = Cli::try_parse_from([
        "trusty-mpm",
        "issue",
        "next",
        "--repo",
        "o/r",
        "--goal",
        "G01",
        "--limit",
        "5",
        "--json",
        "--refresh",
    ])
    .unwrap();
    let Command::Issue {
        cmd:
            IssueCmd::Next {
                repo,
                goal,
                limit,
                json,
                refresh,
            },
        ..
    } = cli.command.unwrap()
    else {
        panic!("not `issue next`");
    };
    assert_eq!(
        (repo.as_deref(), goal.as_deref(), limit, json, refresh),
        (Some("o/r"), Some("G01"), 5, true, true)
    );
    let bare = Cli::try_parse_from(["trusty-mpm", "issue", "next"]).unwrap();
    assert!(matches!(
        bare.command.unwrap(),
        Command::Issue {
            cmd: IssueCmd::Next {
                limit: 10,
                json: false,
                refresh: false,
                ..
            },
            ..
        }
    ));
}

/// Run `cached_snapshot` over a `FakeGh` with `fault`; return the result and
/// whether a cache file exists afterwards.
fn fetch_through(fault: Fault) -> (Result<Snapshot, NextError>, bool) {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cache/o-r.json");
    let fake = FakeGh::new(fault);
    let source = GhSource {
        client: BoardClient::new(&fake),
    };
    let got = cached_snapshot(&source, "o/r", Some(&path), 1_000, false).map(|(s, _)| s);
    (got, path.exists())
}

#[test]
fn a_malformed_graphql_reply_fails_closed_and_is_not_cached() {
    for fault in [
        Fault::EmptyData,
        Fault::NullRepository,
        Fault::NoIssuesConnection,
    ] {
        let (got, cached) = fetch_through(fault);
        assert!(
            matches!(got, Err(NextError::Fetch { .. })),
            "a malformed reply must be an error, got {got:?}"
        );
        assert!(!cached, "a malformed reply was cached");
    }
}

#[test]
fn a_page_two_failure_fails_closed_and_is_not_cached() {
    for fault in [Fault::BoardPage2, Fault::IssuesPage2] {
        let (got, cached) = fetch_through(fault);
        assert!(
            got.is_err(),
            "a page-2 failure must be an error, got {got:?}"
        );
        assert!(!cached, "a partial read was cached");
    }
}

#[test]
fn a_garbage_cache_file_is_refetched() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("o-r.json");
    std::fs::write(&path, b"{not json").expect("write garbage");
    let source = CountingSource {
        calls: Cell::new(0),
        answer: Ok(one_issue_snap()),
    };
    let (snap, cached) =
        cached_snapshot(&source, "o/r", Some(&path), 1_000, false).expect("refetch");
    assert!(!cached);
    assert_eq!(snap, one_issue_snap());
    assert_eq!(source.calls.get(), 1);
    let (_, cached) = cached_snapshot(&source, "o/r", Some(&path), 1_001, false).expect("hit");
    assert!(cached, "the refetch did not rewrite the garbage file");
}

#[cfg(unix)]
#[test]
fn the_cache_is_private_to_the_user() {
    use std::os::unix::fs::PermissionsExt;
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("cache/o-r.json");
    let source = CountingSource {
        calls: Cell::new(0),
        answer: Ok(one_issue_snap()),
    };
    cached_snapshot(&source, "o/r", Some(&path), 1_000, false).expect("first");
    let mode =
        |p: &std::path::Path| std::fs::metadata(p).expect("metadata").permissions().mode() & 0o777;
    assert_eq!(mode(path.parent().expect("dir")), 0o700, "cache dir mode");
    assert_eq!(mode(&path), 0o600, "cache file mode");
    let leftovers: Vec<_> = std::fs::read_dir(path.parent().expect("dir"))
        .expect("read_dir")
        .map(|e| e.expect("entry").file_name())
        .collect();
    assert_eq!(
        leftovers,
        vec![std::ffi::OsString::from("o-r.json")],
        "temp file left behind"
    );
}

#[test]
fn refresh_bypasses_and_rewrites_the_cache() {
    let dir = tempfile::tempdir().expect("tempdir");
    let path = dir.path().join("o-r.json");
    let old = CountingSource {
        calls: Cell::new(0),
        answer: Ok(one_issue_snap()),
    };
    cached_snapshot(&old, "o/r", Some(&path), 1_000, false).expect("seed");
    let mut newer = one_issue_snap();
    newer.issues[0].title = "retitled".into();
    let fresh = CountingSource {
        calls: Cell::new(0),
        answer: Ok(newer.clone()),
    };
    let (got, cached) = cached_snapshot(&fresh, "o/r", Some(&path), 1_001, true).expect("refresh");
    assert!(!cached, "--refresh read the cache");
    assert_eq!(fresh.calls.get(), 1);
    assert_eq!(got, newer);
    let (hit, cached) = cached_snapshot(&old, "o/r", Some(&path), 1_002, false).expect("hit");
    assert!(cached);
    assert_eq!(hit, newer, "--refresh did not rewrite the cache");
}
