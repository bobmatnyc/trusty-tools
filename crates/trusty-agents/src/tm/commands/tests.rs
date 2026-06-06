//! Unit tests for the `/tm` dispatcher + arg parsing.
//!
//! Why: Help/unknown-command rendering and the parse paths that don't need a
//! live tmux server are pure and worth pinning.
//! What: `dispatch_*`, `parse_new_args_*`, `require_name_*`, `truncate_*` tests.
//! Test: This module is itself the test coverage.

use std::path::PathBuf;

use super::handlers::parse_new_args;
use super::{require_name, truncate, write_tm_help};
use crate::tm::project::AdapterType;

#[test]
fn dispatch_help_writes_subcommand_list() {
    let mut out = String::new();
    write_tm_help(&mut out);
    assert!(out.contains("/tm list"));
    assert!(out.contains("/tm new"));
    assert!(out.contains("/tm reconcile"));
}

#[test]
fn parse_new_positional_only() {
    let (n, p, a) = parse_new_args("api-work").unwrap();
    assert_eq!(n.as_deref(), Some("api-work"));
    assert!(p.is_none());
    assert!(a.is_none());
}

#[test]
fn parse_new_with_flags() {
    let (n, p, a) = parse_new_args("ui-dev -p /tmp/foo -a claude-code").unwrap();
    assert_eq!(n.as_deref(), Some("ui-dev"));
    assert_eq!(p, Some(PathBuf::from("/tmp/foo")));
    assert_eq!(a, Some(AdapterType::ClaudeCode));
}

#[test]
fn parse_new_flag_without_value_errors() {
    assert!(parse_new_args("name -p").is_err());
    assert!(parse_new_args("name -a").is_err());
}

#[test]
fn parse_new_unknown_flag_errors() {
    assert!(parse_new_args("name --bogus value").is_err());
}

#[test]
fn truncate_short_unchanged() {
    assert_eq!(truncate("abc", 10), "abc");
}

#[test]
fn truncate_long_appends_ellipsis() {
    let t = truncate("abcdefghij", 5);
    assert!(t.ends_with('…'));
    assert_eq!(t.chars().count(), 5);
}

#[test]
fn require_name_empty_writes_usage() {
    let mut out = String::new();
    let r = require_name("", "pause", &mut out).unwrap();
    assert!(r.is_none());
    assert!(out.contains("usage: /tm pause <name>"));
}

#[test]
fn require_name_returns_first_word() {
    let mut out = String::new();
    let r = require_name("alpha extra", "kill", &mut out).unwrap();
    assert_eq!(r.as_deref(), Some("alpha"));
    assert!(out.is_empty());
}
