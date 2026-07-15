//! CLI parse tests for the `tm manager` verb tree (DOC-36 §3.2/§6 phase 1,
//! epic #2109, WI-6 #2583).
//!
//! Why: pins the exact flag/positional shape of `tm manager status|digest|chat`
//! so a rename or a moved flag fails loudly here rather than silently at
//! runtime, matching `tests_projects.rs`'s convention for `tm projects`.
//! What: `Cli::try_parse_from` round-trips for every manager verb, asserting
//! the parsed `Command::Manager`/`ManagerAction` variant and its fields.
//! Test: this file is the test.

use clap::Parser;

use crate::cli::{Cli, Command, ManagerAction};

/// Assert `argv` parses to a `Command::Manager` and hand its action to the caller.
fn manager_action(argv: &[&str]) -> ManagerAction {
    let cli = Cli::try_parse_from(argv).expect("parse");
    match cli.command.expect("subcommand") {
        Command::Manager { action } => action,
        other => panic!("expected Command::Manager, got {other:?}"),
    }
}

#[test]
fn cli_parses_manager_status() {
    match manager_action(&["tm", "manager", "status"]) {
        ManagerAction::Status { json } => assert!(!json),
        other => panic!("expected Status, got {other:?}"),
    }
}

#[test]
fn cli_parses_manager_status_json() {
    match manager_action(&["tm", "manager", "status", "--json"]) {
        ManagerAction::Status { json } => assert!(json),
        other => panic!("expected Status, got {other:?}"),
    }
}

#[test]
fn cli_parses_manager_digest_default_scope() {
    match manager_action(&["tm", "manager", "digest"]) {
        ManagerAction::Digest { scope, json } => {
            assert_eq!(scope, "portfolio");
            assert!(!json);
        }
        other => panic!("expected Digest, got {other:?}"),
    }
}

#[test]
fn cli_parses_manager_digest_project_scope() {
    match manager_action(&[
        "tm",
        "manager",
        "digest",
        "--scope",
        "project:widget",
        "--json",
    ]) {
        ManagerAction::Digest { scope, json } => {
            assert_eq!(scope, "project:widget");
            assert!(json);
        }
        other => panic!("expected Digest, got {other:?}"),
    }
}

#[test]
fn cli_parses_manager_chat_with_message() {
    match manager_action(&["tm", "manager", "chat", "what needs my attention?"]) {
        ManagerAction::Chat {
            message,
            conversation,
            json,
        } => {
            assert_eq!(message.as_deref(), Some("what needs my attention?"));
            assert_eq!(conversation, None);
            assert!(!json);
        }
        other => panic!("expected Chat, got {other:?}"),
    }
}

#[test]
fn cli_parses_manager_chat_without_message() {
    match manager_action(&["tm", "manager", "chat"]) {
        ManagerAction::Chat { message, .. } => assert_eq!(message, None),
        other => panic!("expected Chat, got {other:?}"),
    }
}

#[test]
fn cli_parses_manager_chat_conversation_override() {
    match manager_action(&[
        "tm",
        "manager",
        "chat",
        "hello",
        "--conversation",
        "test-key-1",
        "--json",
    ]) {
        ManagerAction::Chat {
            message,
            conversation,
            json,
        } => {
            assert_eq!(message.as_deref(), Some("hello"));
            assert_eq!(conversation.as_deref(), Some("test-key-1"));
            assert!(json);
        }
        other => panic!("expected Chat, got {other:?}"),
    }
}

#[test]
fn cli_parses_manager_route() {
    match manager_action(&["tm", "manager", "route", "fix the flaky auth test"]) {
        ManagerAction::Route { text, json } => {
            assert_eq!(text, "fix the flaky auth test");
            assert!(!json);
        }
        other => panic!("expected Route, got {other:?}"),
    }
}

#[test]
fn cli_parses_manager_route_json() {
    match manager_action(&["tm", "manager", "route", "some task", "--json"]) {
        ManagerAction::Route { text, json } => {
            assert_eq!(text, "some task");
            assert!(json);
        }
        other => panic!("expected Route, got {other:?}"),
    }
}

/// A bare `tm manager` (no verb) must fail with a clap usage error — the
/// action subcommand is mandatory, matching `ProjectsAction`'s convention.
#[test]
fn cli_rejects_bare_manager_with_a_usage_error() {
    let err = Cli::try_parse_from(["tm", "manager"]).unwrap_err();
    assert_eq!(err.exit_code(), 2);
}
