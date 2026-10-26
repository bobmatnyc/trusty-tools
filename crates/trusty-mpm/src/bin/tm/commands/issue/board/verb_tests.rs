//! Tests for `tm issue board init | apply | check | show` (#10276, AC1–AC5,
//! AC9, AC10's CLI surface).
//!
//! Why: each acceptance criterion is a claim about what reaches GitHub; these
//! tests run the verbs against the simulating fake `gh` and assert the
//! recorded calls and the board state they leave.
//! What: one test per criterion arm, named for it.
//! Test: this IS the test module (classified test file: 3000-SLOC cap).

use std::cell::Cell;

use clap::Parser as _;

use super::backfill::{CursorFile, Pacer};
use super::check::CheckReport;
use super::client::{BoardClient, Gql, parse_probe};
use super::error::BoardError;
use super::fake_gh_tests::{FakeGh, Sim, SimField};
use super::init::{InitOptions, init};
use super::project::{ProjectBlock, validate_project};
use super::readme::manual_steps;
use super::spec::{BoardSpec, embedded_spec};
use super::verbs::{ApplyFlags, apply_verb, check_verb, require_block, show_text};
use crate::commands::issue::config::{STATUS_LIFECYCLE_YAML, StateModel};
use crate::commands::ticket::runner::{CommandOutput, CommandRunner};

const PROBE_OK: &str = "HTTP/2.0 200 OK\r\nX-Oauth-Scopes: project, repo\r\n\r\n{}";

fn spec() -> BoardSpec {
    embedded_spec().expect("embedded spec")
}

fn lifecycle() -> StateModel {
    serde_yaml::from_str(STATUS_LIFECYCLE_YAML).expect("lifecycle")
}

fn sim() -> Sim {
    Sim {
        probe: (PROBE_OK.to_string(), true),
        ..Sim::default()
    }
}

fn block(number: u64, drops: &[&str], extras: &[&str]) -> ProjectBlock {
    ProjectBlock {
        owner: "o".into(),
        number,
        drop_options: drops.iter().map(|s| s.to_string()).collect(),
        extra_fields: extras.iter().map(|s| s.to_string()).collect(),
    }
}

fn init_opts(apply: bool, drops: &[&str]) -> InitOptions {
    InitOptions {
        owner: "o".into(),
        repo: "o/r".into(),
        title: "r".into(),
        drop_options: drops.iter().map(|s| s.to_string()).collect(),
        apply,
    }
}

/// A pacer that counts its pauses.
#[derive(Default)]
struct CountingPacer(Cell<usize>);

impl Pacer for CountingPacer {
    fn pause(&self) {
        self.0.set(self.0.get() + 1);
    }
}

/// A conformant board created by `init --apply`, and the fake holding it.
fn conformant(drops: &[&str]) -> FakeGh {
    let gh = FakeGh::new(sim());
    init(&BoardClient::new(&gh), &spec(), &init_opts(true, drops)).expect("init creates the board");
    gh
}

fn apply(
    gh: &FakeGh,
    b: &ProjectBlock,
    flags: ApplyFlags,
    cursor: &CursorFile,
) -> Result<String, BoardError> {
    let pacer = CountingPacer::default();
    apply_verb(
        &BoardClient::new(gh),
        &spec(),
        &lifecycle(),
        b,
        "o/r",
        flags,
        (&pacer, cursor),
    )
}

fn flags(apply: bool, backfill: bool, status: bool) -> ApplyFlags {
    ApplyFlags {
        apply,
        backfill,
        status_from_labels: status,
    }
}

fn cursor_in(dir: &tempfile::TempDir) -> CursorFile {
    CursorFile(dir.path().join("cursor.json"))
}

fn check(gh: &FakeGh, b: &ProjectBlock, default_project: Option<&str>) -> CheckReport {
    check_verb(&BoardClient::new(gh), &spec(), b, default_project, "o/r").expect("check reads")
}

/// AC3: a dry run reads (refusal checks included) and writes nothing.
#[test]
fn init_dry_run_makes_only_reads_10276() {
    let gh = FakeGh::new(sim());
    let report = init(&BoardClient::new(&gh), &spec(), &init_opts(false, &[])).expect("dry run");
    assert_eq!(report.number, None);
    assert!(
        report.text.contains("[dry-run] would create project \"r\""),
        "{}",
        report.text
    );
    assert!(gh.mutations().is_empty(), "{:?}", gh.mutations());
    assert!(!gh.calls().is_empty(), "the refusal checks still read");
}

/// AC3: a same-title project refuses, naming its number, with no mutation.
#[test]
fn init_apply_refuses_a_same_title_project_10276() {
    let mut s = sim();
    s.new_project("o", 7, "r");
    let gh = FakeGh::new(s);
    let err = init(&BoardClient::new(&gh), &spec(), &init_opts(true, &[])).unwrap_err();
    assert!(
        matches!(err, BoardError::SameTitle { number: 7, .. }),
        "{err}"
    );
    assert!(err.to_string().contains("project #7 titled `r`"), "{err}");
    assert!(gh.mutations().is_empty());
}

/// AC3: a repo already linked to a project refuses, naming it.
#[test]
fn init_apply_refuses_a_repo_linked_project_10276() {
    let mut s = sim();
    let p = s.new_project("o", 3, "other");
    s.projects[p].repos.push("o/r".into());
    let gh = FakeGh::new(s);
    let err = init(&BoardClient::new(&gh), &spec(), &init_opts(true, &[])).unwrap_err();
    assert!(
        matches!(&err, BoardError::RepoLinked { numbers, .. } if numbers == &[3]),
        "{err}"
    );
    assert!(gh.mutations().is_empty());
}

/// AC3: uncommitted changes to issue-state.yaml refuse, naming the file.
#[test]
fn init_apply_refuses_a_dirty_issue_state_yaml_10276() {
    let gh = FakeGh::new(Sim {
        dirty: true,
        ..sim()
    });
    let err = init(&BoardClient::new(&gh), &spec(), &init_opts(true, &[])).unwrap_err();
    assert!(
        matches!(&err, BoardError::DirtyIssueState { path } if path == "/repo/issue-state.yaml"),
        "{err}"
    );
    assert!(gh.mutations().is_empty());
}

/// AC3 success path (and the AC11 shape: no Published, no On Preview): the
/// project is created, the defaults replaced, Hold and the dates and views
/// created, the repo linked, and the printed block parses under Phase 1.
#[test]
fn init_apply_creates_the_board_and_prints_a_parseable_block_10276() {
    let gh = FakeGh::new(sim());
    let drops = ["On Preview", "Published"];
    let report = init(&BoardClient::new(&gh), &spec(), &init_opts(true, &drops)).expect("created");
    assert_eq!(report.number, Some(1));

    let fields: Vec<String> = gh
        .mutations()
        .iter()
        .filter_map(|c| {
            c.iter()
                .find_map(|a| a.strip_prefix("query=mutation { ").map(str::to_string))
        })
        .map(|q| q.split('(').next().unwrap_or_default().to_string())
        .collect();
    assert_eq!(fields[0], "createProjectV2");
    for want in [
        "updateProjectV2Field",
        "createProjectV2Field",
        "createProjectV2View",
        "linkProjectV2ToRepository",
        "updateProjectV2",
        "deleteProjectV2View",
    ] {
        assert!(
            fields.iter().any(|f| f == want),
            "{want} missing from {fields:?}"
        );
    }
    let status = &gh.inputs("updateProjectV2Field")[0]["singleSelectOptions"];
    let names: Vec<&str> = status
        .as_array()
        .expect("list")
        .iter()
        .filter_map(|o| o["name"].as_str())
        .collect();
    assert_eq!(
        names,
        [
            "Backlog",
            "Ready for Development",
            "In Development",
            "In PR",
            "Merged",
            "On Prod",
            "Won't Do"
        ]
    );
    let created: Vec<String> = gh
        .inputs("createProjectV2Field")
        .iter()
        .filter_map(|i| i["name"].as_str().map(str::to_string))
        .collect();
    assert_eq!(
        created,
        [
            "Start date",
            "Target date",
            "Deployed on",
            "Published on",
            "Hold"
        ]
    );
    let roadmap = gh
        .inputs("createProjectV2View")
        .into_iter()
        .find(|i| i["name"] == "Roadmap")
        .expect("Roadmap created");
    assert!(
        roadmap["configuration"].is_null(),
        "a Roadmap takes no visible fields: {roadmap}"
    );

    let yaml = &report.text[report.text.find("project:\n").expect("block printed")..];
    #[derive(serde::Deserialize)]
    struct Wrapper {
        project: ProjectBlock,
    }
    let parsed: Wrapper = serde_yaml::from_str(yaml).expect("the block parses");
    assert_eq!(parsed.project, block(1, &drops, &[]));
    validate_project(&parsed.project, &spec()).expect("the block validates against the spec");

    let sim = gh.sim.borrow();
    let views: Vec<&str> = sim.projects[0]
        .views
        .iter()
        .map(|v| v.name.as_str())
        .collect();
    assert_eq!(
        views,
        ["List", "Swimlanes", "Roadmap"],
        "GitHub's default view is gone"
    );
    drop(sim);
    let report = check(&gh, &parsed.project, None);
    assert!(report.clean(), "{}", report.render());
}

/// The same board as `conformant`, partly undone by hand.
fn drifted() -> FakeGh {
    let gh = conformant(&[]);
    {
        let mut s = gh.sim.borrow_mut();
        let p = &mut s.projects[0];
        p.fields.retain(|f| f.name != "Hold");
        p.views.retain(|v| v.name != "Roadmap");
        p.repos.clear();
        p.readme.clear();
    }
    gh
}

/// AC4: a dry-run apply (with backfill) plans and writes nothing.
#[test]
fn apply_dry_run_makes_no_mutation_10276() {
    let gh = drifted();
    gh.sim.borrow_mut().with_issues("o/r", 3);
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mark = gh.calls().len();
    let text = apply(
        &gh,
        &block(1, &[], &[]),
        flags(false, true, true),
        &cursor_in(&dir),
    )
    .expect("plan");
    assert!(
        text.contains("[dry-run] would create field `Hold`"),
        "{text}"
    );
    assert!(text.contains("would add 3 issue(s)"), "{text}");
    let after: Vec<_> = gh.calls()[mark..]
        .iter()
        .filter(|c| super::fake_gh_tests::is_mutation(c))
        .cloned()
        .collect();
    assert!(after.is_empty(), "{after:?}");
}

/// AC4: `apply` restores the drift, and a second run records no mutation.
#[test]
fn apply_twice_records_no_mutation_on_the_second_run_10276() {
    let gh = drifted();
    gh.sim.borrow_mut().with_issues("o/r", 3);
    let dir = tempfile::TempDir::new().expect("tempdir");
    let b = block(1, &[], &[]);
    let before = gh.mutations().len();
    apply(&gh, &b, flags(true, true, true), &cursor_in(&dir)).expect("first apply");
    assert!(
        gh.mutations().len() > before,
        "the first run repairs the drift"
    );
    let roadmap = gh
        .inputs("createProjectV2View")
        .into_iter()
        .filter(|i| i["name"] == "Roadmap")
        .count();
    assert_eq!(roadmap, 2, "init's and apply's");

    let mark = gh.mutations().len();
    let text = apply(&gh, &b, flags(true, true, true), &cursor_in(&dir)).expect("second apply");
    assert_eq!(gh.mutations().len(), mark, "second run wrote: {text}");
    assert!(check(&gh, &b, None).clean());
}

/// Owner decision 2: removing an option items still use is refused, naming
/// the option and its item count, before any write.
#[test]
fn apply_refuses_to_remove_an_option_items_use_naming_it_and_the_count_10276() {
    let gh = conformant(&[]);
    {
        let mut s = gh.sim.borrow_mut();
        let status = s.projects[0]
            .fields
            .iter_mut()
            .find(|f| f.name == "Status")
            .expect("Status");
        status.options.push(super::fake_gh_tests::SimOption {
            id: "O_todo".into(),
            name: "Todo".into(),
            color: "GREEN".into(),
            description: String::new(),
        });
        for n in 1..=3 {
            s.put_item(0, "o/r", n, Some("Todo"));
        }
        s.put_item(0, "o/r", 4, Some("Backlog"));
    }
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mark = gh.mutations().len();
    let err = apply(
        &gh,
        &block(1, &[], &[]),
        flags(true, false, false),
        &cursor_in(&dir),
    )
    .unwrap_err();
    assert!(
        matches!(&err, BoardError::OptionInUse { field, in_use } if field == "Status" && in_use == &[("Todo".to_string(), 3)]),
        "{err}"
    );
    assert!(err.to_string().contains("`Todo` (3 items)"), "{err}");
    assert_eq!(gh.mutations().len(), mark, "refused before any write");
}

/// Owner decision 2: an option edit on a board with items sends every
/// existing option with its id, and the items keep their values.
#[test]
fn apply_sends_every_existing_option_id_on_an_option_edit_10276() {
    let gh = conformant(&[]);
    let ids: Vec<(String, String)> = {
        let mut s = gh.sim.borrow_mut();
        s.put_item(0, "o/r", 1, Some("In PR"));
        s.put_item(0, "o/r", 2, Some("Merged"));
        let status = s.projects[0]
            .fields
            .iter_mut()
            .find(|f| f.name == "Status")
            .expect("Status");
        status.options.swap(0, 1);
        status.options[2].color = "RED".into();
        status.options[3].description = "edited by hand".into();
        status
            .options
            .iter()
            .map(|o| (o.name.clone(), o.id.clone()))
            .collect()
    };
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mark = gh.inputs("updateProjectV2Field").len();
    apply(
        &gh,
        &block(1, &[], &[]),
        flags(true, false, false),
        &cursor_in(&dir),
    )
    .expect("apply");
    let edits = gh.inputs("updateProjectV2Field");
    assert_eq!(edits.len(), mark + 1, "one option edit");
    let sent = edits[mark]["singleSelectOptions"]
        .as_array()
        .expect("list")
        .clone();
    assert_eq!(sent.len(), ids.len());
    for (name, id) in &ids {
        let option = sent
            .iter()
            .find(|o| o["name"] == name.as_str())
            .expect("option sent");
        assert_eq!(option["id"], id.as_str(), "{name} lost its id");
    }
    let s = gh.sim.borrow();
    let in_pr = s.projects[0]
        .field("Status")
        .and_then(|f| f.options.iter().find(|o| o.name == "In PR"))
        .map(|o| o.id.clone());
    assert_eq!(
        s.projects[0].items[0].values.get("F_Status").cloned(),
        in_pr,
        "the item kept its value"
    );
}

/// AC4: 250 open issues are paged, paced and resumed from the recorded cursor
/// after a failure; no issue is added twice.
#[test]
fn backfill_pages_paces_and_resumes_without_adding_twice_10276() {
    let gh = conformant(&[]);
    gh.sim.borrow_mut().with_issues("o/r", 250);
    let dir = tempfile::TempDir::new().expect("tempdir");
    let cursor = cursor_in(&dir);
    let b = block(1, &[], &[]);
    gh.fail_once("contentId: \"ISSUE_150\"", "HTTP 502: Bad gateway");
    let pacer = CountingPacer::default();
    let run = |p: &CountingPacer| {
        apply_verb(
            &BoardClient::new(&gh),
            &spec(),
            &lifecycle(),
            &b,
            "o/r",
            flags(true, true, false),
            (p, &cursor),
        )
    };
    let err = run(&pacer).unwrap_err();
    assert!(err.to_string().contains("502"), "{err}");
    let recorded = std::fs::read_to_string(&cursor.0).expect("cursor recorded");
    assert!(recorded.contains("\"after\":\"c100\""), "{recorded}");

    let mark = gh.calls().len();
    let text = run(&pacer).expect("resumed run");
    assert!(text.contains("resumed from the recorded cursor"), "{text}");
    let first_read = gh.calls()[mark..]
        .iter()
        .find(|c| c.iter().any(|a| a.contains("query OpenIssues")))
        .cloned()
        .expect("an OpenIssues read");
    assert!(
        first_read.contains(&"cursor=c100".to_string()),
        "{first_read:?}"
    );

    let adds: Vec<String> = gh
        .inputs("addProjectV2ItemById")
        .iter()
        .filter_map(|i| i["contentId"].as_str().map(str::to_string))
        .collect();
    for n in 1..=250 {
        let want = if n == 150 { 2 } else { 1 };
        let got = adds.iter().filter(|a| **a == format!("ISSUE_{n}")).count();
        assert_eq!(got, want, "#{n} add attempts");
    }
    assert_eq!(gh.sim.borrow().projects[0].items.len(), 250);
    assert_eq!(pacer.0.get(), 251, "one pause per add attempt");
    assert!(!cursor.0.exists(), "a finished backfill clears its cursor");
}

/// D3: Status is written only under the explicit flag, from the labels.
#[test]
fn backfill_writes_status_only_under_the_flag_10276() {
    let gh = conformant(&[]);
    {
        let mut s = gh.sim.borrow_mut();
        s.with_issues("o/r", 3);
        let issues = s.issues.get_mut("o/r").expect("issues");
        issues[0].labels = vec!["status:coded".into()];
        issues[2].labels = vec!["status:coded".into(), "status:merged".into()];
    }
    let dir = tempfile::TempDir::new().expect("tempdir");
    let b = block(1, &[], &[]);
    apply(&gh, &b, flags(true, true, false), &cursor_in(&dir)).expect("backfill without the flag");
    assert!(
        gh.inputs("updateProjectV2ItemFieldValue").is_empty(),
        "no Status write without the flag"
    );

    let text = apply(&gh, &b, flags(true, true, true), &cursor_in(&dir)).expect("with the flag");
    let writes = gh.inputs("updateProjectV2ItemFieldValue");
    assert_eq!(writes.len(), 2, "{text}");
    assert!(text.contains("#1 → In PR"), "{text}");
    assert!(text.contains("#2 → Backlog"), "{text}");
    assert!(
        text.contains("skipped #3: Status not set — carries labels for In PR and Merged"),
        "{text}"
    );
}

/// AC5: drift, repo extras, information and missing issues — reads only.
#[test]
fn check_reports_drift_extras_info_and_missing_issues_without_mutating_10276() {
    let gh = conformant(&[]);
    {
        let mut s = gh.sim.borrow_mut();
        s.with_issues("o/r", 2);
        s.put_item(0, "o/r", 2, None);
        let p = &mut s.projects[0];
        p.fields.retain(|f| f.name != "Target date");
        for (name, t) in [
            ("Crate", "SINGLE_SELECT"),
            ("Agents", "TEXT"),
            ("Sprint", "TEXT"),
        ] {
            p.fields.push(SimField {
                id: format!("F_{name}"),
                name: name.into(),
                data_type: t.into(),
                options: Vec::new(),
            });
        }
        p.readme = "hand-written".into();
    }
    let mark = gh.mutations().len();
    let report = check(&gh, &block(1, &[], &["Crate", "Agents"]), None);
    assert!(
        report
            .drift
            .iter()
            .any(|d| d == "standard field `Target date` is missing"),
        "{:?}",
        report.drift
    );
    assert!(
        report
            .drift
            .iter()
            .any(|d| d.contains("`Sprint` is not standard")),
        "{:?}",
        report.drift
    );
    assert_eq!(report.extras, ["Crate", "Agents"]);
    assert!(
        report.info.iter().any(|i| i.starts_with("README differs")),
        "{:?}",
        report.info
    );
    assert_eq!(report.missing_issues, [1]);
    assert!(
        report
            .token
            .iter()
            .any(|t| t.contains("scopes: project, repo")),
        "{:?}",
        report.token
    );
    assert!(!report.clean());
    assert_eq!(gh.mutations().len(), mark, "check never writes");
}

/// AC5: scopes come from `X-OAuth-Scopes`; a 403 with `X-GitHub-SSO` is SSO.
#[test]
fn check_reports_scopes_and_sso_from_headers_10276() {
    let probe = parse_probe("HTTP/2.0 200 OK\r\nX-Oauth-Scopes: read:project, repo\r\n\r\n{}");
    assert_eq!(
        probe.scopes,
        Some(vec!["read:project".to_string(), "repo".to_string()])
    );
    let (lines, sso) = super::check::token_lines(&probe);
    assert!(
        lines.iter().any(|l| l.starts_with("no `project` scope")),
        "{lines:?}"
    );
    assert!(sso.is_none());
    let (lines, _) = super::check::token_lines(&parse_probe("HTTP/2.0 200 OK\r\n\r\n{}"));
    assert!(
        lines[0].contains("fine-grained or GitHub App token"),
        "{lines:?}"
    );

    let gh = conformant(&[]);
    gh.sim.borrow_mut().probe = (
        "HTTP/2.0 403 Forbidden\r\nX-Github-Sso: required; url=https://github.com/orgs/o/sso?x=1\r\n\r\n{}".into(),
        false,
    );
    let mark = gh.calls().len();
    let report = check(&gh, &block(1, &[], &[]), None);
    assert!(
        report
            .sso_required
            .as_deref()
            .is_some_and(|s| s.starts_with("required;")),
        "{report:?}"
    );
    assert!(!report.clean());
    assert!(
        report.render().contains("SSO authorisation required"),
        "{}",
        report.render()
    );
    assert_eq!(gh.calls().len(), mark + 1, "only the probe ran");
}

/// AC5: no production file of the module names a token variable or reads one.
#[test]
fn board_module_never_reads_the_token_value_10276() {
    let sources = [
        include_str!("attach.rs"),
        include_str!("backfill.rs"),
        include_str!("check.rs"),
        include_str!("client.rs"),
        include_str!("error.rs"),
        include_str!("init.rs"),
        include_str!("mod.rs"),
        include_str!("project.rs"),
        include_str!("readme.rs"),
        include_str!("reconcile.rs"),
        include_str!("snapshot.rs"),
        include_str!("spec.rs"),
        include_str!("validate.rs"),
        include_str!("verbs.rs"),
    ];
    let banned = [
        concat!("GH_", "TOKEN"),
        concat!("GITHUB_", "TOKEN"),
        concat!("auth", " token"),
        concat!("\"tok", "en\""),
    ];
    for source in sources {
        for word in banned {
            assert!(!source.contains(word), "a board source names `{word}`");
        }
    }
}

/// `show` prints the embedded spec and its version.
#[test]
fn show_prints_the_embedded_spec_and_its_version_10276() {
    let text = show_text(&spec());
    assert!(
        text.starts_with("board spec version 1 (embedded in this tm)"),
        "{text}"
    );
    assert!(
        text.contains("version: 1\n") && text.contains("- name: Status"),
        "{text}"
    );
}

/// AC9: a non-GitHub tracker is refused by name, before anything runs.
#[test]
fn board_refuses_a_non_github_tracker_naming_it_10276() {
    use crate::commands::ticket::system::TicketSystemKind;
    let err = super::run_cli(crate::cli::BoardCmd::Show, TicketSystemKind::Jira).unwrap_err();
    assert!(
        err.to_string().contains("this repo's tracker is `jira`"),
        "{err}"
    );
    let err = super::require_github(TicketSystemKind::Linear).unwrap_err();
    assert!(matches!(&err, BoardError::NotGitHub { tracker } if tracker == "linear"));
    assert!(super::require_github(TicketSystemKind::Gh).is_ok());
}

/// The CLI parses each verb (AC10's surface).
#[test]
fn cli_parses_issue_board_verbs_10276() {
    use crate::cli::{BoardCmd, Cli, Command, IssueCmd};
    let parse = |args: &[&str]| -> BoardCmd {
        let mut argv = vec!["tm", "issue", "board"];
        argv.extend_from_slice(args);
        match Cli::try_parse_from(argv).expect("parses").command {
            Some(Command::Issue {
                cmd: IssueCmd::Board(b),
                ..
            }) => b,
            other => panic!("not a board verb: {other:?}"),
        }
    };
    let init = parse(&[
        "init",
        "--owner",
        "o",
        "--repo",
        "o/r",
        "--drop-option",
        "Published",
        "--drop-option",
        "On Preview",
        "--apply",
    ]);
    assert!(
        matches!(init, BoardCmd::Init { ref drop_option, apply: true, .. } if drop_option.len() == 2),
        "{init:?}"
    );
    let apply = parse(&["apply", "--apply", "--backfill", "--status-from-labels"]);
    assert!(
        matches!(
            apply,
            BoardCmd::Apply {
                apply: true,
                backfill: true,
                status_from_labels: true,
                pace_ms: 1000,
                ..
            }
        ),
        "{apply:?}"
    );
    assert!(matches!(parse(&["check"]), BoardCmd::Check { .. }));
    assert!(matches!(parse(&["show"]), BoardCmd::Show));
    assert!(matches!(
        parse(&["add", "12"]),
        BoardCmd::Add { issue: 12, .. }
    ));
    let refused = Cli::try_parse_from(["tm", "issue", "board", "apply", "--status-from-labels"]);
    assert!(refused.is_err(), "the Status flag requires --backfill");
}

/// GraphQL literals: strings escaped, enums bare, lists and objects nested.
#[test]
fn gql_literals_escape_strings_and_keep_enums_bare_10276() {
    let lit = Gql::Obj(vec![
        ("name", Gql::s("PR open, `Refs #N` \"quoted\"\nline")),
        ("color", Gql::Enum("GRAY".into())),
        ("ids", Gql::List(vec![Gql::s("a"), Gql::s("b")])),
    ]);
    assert_eq!(
        lit.render(),
        r#"{name: "PR open, `Refs #N` \"quoted\"\nline", color: GRAY, ids: ["a", "b"]}"#
    );
}

/// Owner decision 1: the manual checklist lists Roadmap fields and the
/// workflows to turn on, but not a BOARD view's Status grouping.
#[test]
fn manual_steps_list_roadmap_fields_and_workflows_but_not_board_status_grouping_10276() {
    let steps = manual_steps(&spec());
    assert!(
        steps
            .iter()
            .any(|s| s.starts_with("View `Roadmap`: show the fields")),
        "{steps:?}"
    );
    assert!(
        steps
            .iter()
            .any(|s| s.starts_with("Workflows: turn on `Auto-add to project`")),
        "{steps:?}"
    );
    assert!(
        !steps.iter().any(|s| s.contains("group by `Status`")),
        "{steps:?}"
    );
    let mut grouped = spec();
    if let Some(v) = grouped.views.iter_mut().find(|v| v.name == "List") {
        v.vertical_group_by = Some("Status".into());
    }
    assert!(
        manual_steps(&grouped)
            .iter()
            .any(|s| s.starts_with("View `List`: group by `Status`"))
    );
}

/// A runner that answers every call with one fixed output.
struct Raw(bool, &'static str);

impl CommandRunner for Raw {
    fn run(&self, _program: &str, _args: &[&str]) -> anyhow::Result<CommandOutput> {
        Ok(CommandOutput {
            success: self.0,
            stdout: self.1.to_string(),
            stderr: if self.0 { String::new() } else { "boom".into() },
        })
    }
}

/// Each `BoardError` variant is produced by its failure path, or rendered
/// with its value where the path is a single constructor.
#[test]
fn every_board_error_variant_renders_its_value_10276() {
    let client = BoardClient::new(&Raw(true, "not json"));
    let parse = super::snapshot::owner_node(&client, "o").unwrap_err();
    assert!(matches!(parse, BoardError::Parse { .. }), "{parse}");
    let failing = Raw(false, "");
    let command = super::snapshot::owner_node(&BoardClient::new(&failing), "o").unwrap_err();
    assert!(command.to_string().contains("failed: boom"), "{command}");

    let model = lifecycle();
    let none = require_block(&model, Some("trusty-mpm")).unwrap_err();
    assert!(
        none.to_string()
            .contains("`trusty-mpm` is only an attach fallback"),
        "{none}"
    );

    let gh = conformant(&[]);
    gh.sim.borrow_mut().projects[0]
        .fields
        .iter_mut()
        .filter(|f| f.name == "Start date")
        .for_each(|f| f.data_type = "TEXT".into());
    let dir = tempfile::TempDir::new().expect("tempdir");
    let mismatch = apply(
        &gh,
        &block(1, &[], &[]),
        flags(true, false, false),
        &cursor_in(&dir),
    )
    .unwrap_err();
    assert!(
        matches!(&mismatch, BoardError::FieldTypeMismatch { field, found, .. } if field == "Start date" && found == "TEXT"),
        "{mismatch}"
    );
    let missing = apply(
        &gh,
        &block(99, &[], &[]),
        flags(false, false, false),
        &cursor_in(&dir),
    )
    .unwrap_err();
    assert!(
        matches!(missing, BoardError::ProjectNotFound { number: 99, .. }),
        "{missing}"
    );

    std::fs::write(dir.path().join("cursor.json"), "garbage").expect("write");
    let fresh = conformant(&[]);
    fresh.sim.borrow_mut().with_issues("o/r", 1);
    let cursor = apply(
        &fresh,
        &block(1, &[], &[]),
        flags(true, true, false),
        &cursor_in(&dir),
    )
    .unwrap_err();
    assert!(matches!(cursor, BoardError::Cursor { .. }), "{cursor}");

    let spec_err = BoardError::from(super::validate::BoardSpecError::ZeroProjectNumber);
    assert!(
        spec_err.to_string().contains("project.number"),
        "{spec_err}"
    );
    let no_target = BoardError::NoAttachTarget("x".into());
    assert!(no_target.to_string().contains("no board to attach to: x"));
}
