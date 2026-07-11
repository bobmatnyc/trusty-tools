//! CLI parse tests for the `tm projects` verb tree (DOC-35 §3.1/§10.8).
//!
//! Why: `tm projects` (#2115) and its `deliverables`/`milestones` subtrees
//! (#2381) build a large clap namespace; a parse test per verb pins the exact
//! flag/positional shape so a rename or a moved flag fails loudly here rather
//! than silently at runtime. Split into its own file to keep `tests.rs` under the
//! test-file line cap.
//! What: `Cli::try_parse_from` round-trips for every projects verb, asserting the
//! parsed `Command`/action variant and its salient fields.
//! Test: this file is the test.

use clap::Parser;

use crate::cli::{
    Cli, Command, DeliverableKindArg, DeliverableStatusArg, DeliverablesAction, EstimationTierArg,
    MilestonesAction, ProjectsAction,
};

/// Assert `argv` parses to a `Command::Projects` and hand its action to `check`.
fn projects_action(argv: &[&str]) -> ProjectsAction {
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command.expect("subcommand") {
        Command::Projects { action } => action,
        other => panic!("expected Command::Projects, got {other:?}"),
    }
}

/// #2118: `ProjectsAction` stays a MANDATORY clap subcommand — a bare
/// `tm projects` (no verb) must still fail to parse with a clap usage error,
/// unchanged from before #2118. The interactive-TTY TUI launch is intercepted
/// in `main.rs` BEFORE `Cli::try_parse()` even runs (see
/// `commands::projects::is_bare_projects_argv`), so it never reaches this
/// parse path at all.
///
/// Why this asserts `exit_code()` rather than a specific `ErrorKind`: clap's
/// internal validator (`clap_builder`'s `parser::validator::Validator::validate`)
/// checks `is_arg_required_else_help_set()` BEFORE `is_subcommand_required_set()`
/// — whichever is true first decides between
/// `ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand` and
/// `ErrorKind::MissingSubcommand`. That internal, non-`Option`-derived
/// resolution was observed to differ between this project's local dev
/// environment (always `MissingSubcommand`, confirmed via
/// `cargo test --workspace` too, not just `-p trusty-mpm`) and CI's Linux
/// runner (`DisplayHelpOnMissingArgumentOrSubcommand`) despite an identical
/// locked `clap_builder` version (4.6.0, verified via `Cargo.lock`) — i.e. it
/// is not this project's dependency resolution that differs. Both kinds are
/// `Stream::Stderr` in `clap_builder`'s `error::Error::stream()` (only
/// `DisplayHelp`/`DisplayVersion` map to `Stream::Stdout`), so both share
/// `exit_code() == 2` regardless of which one fires — that shared, portable
/// contract (a non-zero exit, not clap's internal error-kind taxonomy) is
/// what `main.rs`'s bare-invocation interception actually depends on, so it
/// is what this test pins.
/// Test: this test.
#[test]
fn cli_rejects_bare_projects_with_a_usage_error() {
    let err =
        Cli::try_parse_from(["tm", "projects"]).expect_err("bare `projects` must fail to parse");
    assert_eq!(err.exit_code(), 2);
    assert!(
        matches!(
            err.kind(),
            clap::error::ErrorKind::MissingSubcommand
                | clap::error::ErrorKind::DisplayHelpOnMissingArgumentOrSubcommand
        ),
        "expected a 'needs a subcommand' usage error, got {:?}",
        err.kind()
    );
}

// ───────────────────────── registry verbs (#2115) ─────────────────────────

#[test]
fn cli_parses_projects_list_bare() {
    match projects_action(&["trusty-mpm", "projects", "list"]) {
        ProjectsAction::List { json, tag } => {
            assert!(!json);
            assert_eq!(tag, None);
        }
        other => panic!("expected list, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_list_json_and_tag() {
    match projects_action(&[
        "trusty-mpm",
        "projects",
        "list",
        "--json",
        "--tag",
        "backend",
    ]) {
        ProjectsAction::List { json, tag } => {
            assert!(json);
            assert_eq!(tag.as_deref(), Some("backend"));
        }
        other => panic!("expected list, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_register_full() {
    let action = projects_action(&[
        "trusty-mpm",
        "projects",
        "register",
        "widget",
        "--repo-url",
        "https://github.com/acme/widget",
        "--default-branch",
        "develop",
        "--description",
        "the widget",
        "--tags",
        "backend,oss",
        "--stack-hint",
        "rust",
        "--gh-user",
        "acme-bot",
    ]);
    match action {
        ProjectsAction::Register {
            name,
            repo_url,
            default_branch,
            description,
            tags,
            stack_hint,
            gh_user,
        } => {
            assert_eq!(name, "widget");
            assert_eq!(repo_url, "https://github.com/acme/widget");
            assert_eq!(default_branch.as_deref(), Some("develop"));
            assert_eq!(description.as_deref(), Some("the widget"));
            assert_eq!(tags, vec!["backend".to_string(), "oss".to_string()]);
            assert_eq!(stack_hint.as_deref(), Some("rust"));
            assert_eq!(gh_user.as_deref(), Some("acme-bot"));
        }
        other => panic!("expected register, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_register_minimal() {
    match projects_action(&[
        "trusty-mpm",
        "projects",
        "register",
        "minimal",
        "--repo-url",
        "https://github.com/acme/minimal",
    ]) {
        ProjectsAction::Register {
            name,
            repo_url,
            default_branch,
            tags,
            ..
        } => {
            assert_eq!(name, "minimal");
            assert_eq!(repo_url, "https://github.com/acme/minimal");
            assert_eq!(default_branch, None);
            assert!(tags.is_empty());
        }
        other => panic!("expected register, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_show() {
    match projects_action(&["trusty-mpm", "projects", "show", "widget", "--json"]) {
        ProjectsAction::Show { name, json } => {
            assert_eq!(name, "widget");
            assert!(json);
        }
        other => panic!("expected show, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_status() {
    match projects_action(&["trusty-mpm", "projects", "status", "widget"]) {
        ProjectsAction::Status { name, json } => {
            assert_eq!(name, "widget");
            assert!(!json);
        }
        other => panic!("expected status, got {other:?}"),
    }
}

// ─────────────────────── deliverables subtree (#2381) ──────────────────────

fn deliverables_action(argv: &[&str]) -> DeliverablesAction {
    match projects_action(argv) {
        ProjectsAction::Deliverables { action } => action,
        other => panic!("expected deliverables, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_deliverables_list_with_status() {
    match deliverables_action(&[
        "trusty-mpm",
        "projects",
        "deliverables",
        "list",
        "widget",
        "--status",
        "in-progress",
        "--json",
    ]) {
        DeliverablesAction::List {
            project,
            json,
            status,
        } => {
            assert_eq!(project, "widget");
            assert!(json);
            assert_eq!(status, Some(DeliverableStatusArg::InProgress));
        }
        other => panic!("expected list, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_deliverables_add() {
    match deliverables_action(&[
        "trusty-mpm",
        "projects",
        "deliverables",
        "add",
        "widget",
        "--name",
        "OAuth2 flow",
        "--kind",
        "feature",
        "--estimate",
        "L",
        "--spec-ref",
        "docs/specs/tm-project-control-plane.md",
        "--ticket-ref",
        "#2117",
    ]) {
        DeliverablesAction::Add {
            project,
            name,
            kind,
            estimate,
            spec_ref,
            ticket_ref,
            ..
        } => {
            assert_eq!(project, "widget");
            assert_eq!(name, "OAuth2 flow");
            assert_eq!(kind, DeliverableKindArg::Feature);
            assert_eq!(estimate, EstimationTierArg::L);
            assert_eq!(
                spec_ref.as_deref(),
                Some("docs/specs/tm-project-control-plane.md")
            );
            assert_eq!(ticket_ref.as_deref(), Some("#2117"));
        }
        other => panic!("expected add, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_deliverables_create_alias() {
    // `create` is an accepted alias of `add` (the prompt's spelling).
    match deliverables_action(&[
        "trusty-mpm",
        "projects",
        "deliverables",
        "create",
        "widget",
        "--name",
        "x",
        "--kind",
        "bugfix",
        "--estimate",
        "S",
    ]) {
        DeliverablesAction::Add { kind, estimate, .. } => {
            assert_eq!(kind, DeliverableKindArg::Bugfix);
            assert_eq!(estimate, EstimationTierArg::S);
        }
        other => panic!("expected add (via create alias), got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_deliverables_show() {
    match deliverables_action(&[
        "trusty-mpm",
        "projects",
        "deliverables",
        "show",
        "widget",
        "00000000-0000-0000-0000-000000000001",
    ]) {
        DeliverablesAction::Show { project, id, json } => {
            assert_eq!(project, "widget");
            assert_eq!(id, "00000000-0000-0000-0000-000000000001");
            assert!(!json);
        }
        other => panic!("expected show, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_deliverables_set_status() {
    match deliverables_action(&[
        "trusty-mpm",
        "projects",
        "deliverables",
        "set-status",
        "widget",
        "00000000-0000-0000-0000-000000000001",
        "complete",
    ]) {
        DeliverablesAction::SetStatus {
            project,
            id,
            status,
        } => {
            assert_eq!(project, "widget");
            assert_eq!(id, "00000000-0000-0000-0000-000000000001");
            assert_eq!(status, DeliverableStatusArg::Complete);
        }
        other => panic!("expected set-status, got {other:?}"),
    }
}

#[test]
fn cli_parses_estimate_xl_uppercase() {
    // The estimation tier value names are the exact uppercase letters.
    match deliverables_action(&[
        "trusty-mpm",
        "projects",
        "deliverables",
        "add",
        "widget",
        "--name",
        "x",
        "--kind",
        "chore",
        "--estimate",
        "XL",
    ]) {
        DeliverablesAction::Add { estimate, .. } => {
            assert_eq!(estimate, EstimationTierArg::Xl);
        }
        other => panic!("expected add, got {other:?}"),
    }
}

// ──────────────────────── milestones subtree (#2381) ───────────────────────

fn milestones_action(argv: &[&str]) -> MilestonesAction {
    match projects_action(argv) {
        ProjectsAction::Milestones { action } => action,
        other => panic!("expected milestones, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_milestones_list() {
    match milestones_action(&["trusty-mpm", "projects", "milestones", "list", "widget"]) {
        MilestonesAction::List { project, json } => {
            assert_eq!(project, "widget");
            assert!(!json);
        }
        other => panic!("expected list, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_milestones_add() {
    match milestones_action(&[
        "trusty-mpm",
        "projects",
        "milestones",
        "add",
        "widget",
        "--name",
        "v1.0 Alpha",
        "--target-date",
        "2026-09-01T00:00:00Z",
    ]) {
        MilestonesAction::Add {
            project,
            name,
            target_date,
            ..
        } => {
            assert_eq!(project, "widget");
            assert_eq!(name, "v1.0 Alpha");
            assert_eq!(target_date, "2026-09-01T00:00:00Z");
        }
        other => panic!("expected add, got {other:?}"),
    }
}

#[test]
fn cli_parses_projects_milestones_show() {
    match milestones_action(&[
        "trusty-mpm",
        "projects",
        "milestones",
        "show",
        "widget",
        "00000000-0000-0000-0000-000000000002",
        "--json",
    ]) {
        MilestonesAction::Show { project, id, json } => {
            assert_eq!(project, "widget");
            assert_eq!(id, "00000000-0000-0000-0000-000000000002");
            assert!(json);
        }
        other => panic!("expected show, got {other:?}"),
    }
}
