//! Tests for `--account` on the session-spawning verbs (#8914).
//!
//! Why: the global `--account` flag was parsed and dropped on `tm sessions
//! new`; these pin which verbs apply it and which records the pin updates.
//! What: clap parses and pure record selection — no daemon, `gh` or network.
//! Test: this file IS the test module.

use clap::Parser;

use super::*;
use crate::cli::{Cli, Command};

/// Parse `args` and return the `tm sessions` action and the global account.
fn parse(args: &[&str]) -> (SessionAction, Option<String>) {
    let cli = Cli::try_parse_from(args).expect("parses");
    match cli.command {
        Some(Command::Sessions { action }) => (action, cli.account),
        other => panic!("expected `tm sessions`, got {other:?}"),
    }
}

/// 🔴 #8914: `tm sessions new --account X <path>` binds X and names the path
/// the pin is made for.
#[test]
fn spawn_dir_names_the_new_and_start_checkouts() {
    let (action, account) = parse(&[
        "tm",
        "sessions",
        "new",
        "--account",
        "bobmatnyc",
        "/work/itinerary",
        "--task",
        "t",
    ]);
    assert_eq!(account.as_deref(), Some("bobmatnyc"));
    assert_eq!(
        spawn_dir(&action).expect("resolves"),
        Some(PathBuf::from("/work/itinerary"))
    );
    let (action, account) = parse(&[
        "tm",
        "--account",
        "bobmatnyc",
        "sessions",
        "start",
        "--dir",
        "/w",
    ]);
    assert_eq!(account.as_deref(), Some("bobmatnyc"));
    assert_eq!(
        spawn_dir(&action).expect("resolves"),
        Some(PathBuf::from("/w"))
    );
}

/// A verb that spawns nothing has no dir, so `--account` on it is refused
/// rather than dropped.
#[test]
fn spawn_dir_is_none_for_a_verb_that_spawns_nothing() {
    let (action, _) = parse(&[
        "tm",
        "sessions",
        "resume",
        "tm-x-01",
        "--account",
        "bobmatnyc",
    ]);
    assert_eq!(spawn_dir(&action).expect("resolves"), None);
}

fn project(name: &str, repo_url: &str) -> Project {
    Project {
        name: name.to_string(),
        repo_url: repo_url.to_string(),
        default_branch: "main".to_string(),
        stack_hint: None,
        tags: vec![],
        description: None,
        gh_user: None,
        gh_account: Some("bob-duetto".to_string()),
        github: None,
        commit_name: None,
        commit_email: None,
        worktree: None,
    }
}

/// Every record for the origin is re-pinned, in its own `repo_url` spelling,
/// so no record is left pinning another account.
#[test]
fn pin_targets_updates_every_matching_record() {
    let projects = vec![
        project("itinerary", "git@github.com:acme/itinerary.git"),
        project("itin-alias", "https://github.com/acme/itinerary"),
        project("other", "https://github.com/acme/other"),
    ];
    let targets = pin_targets(&projects, "https://github.com/acme/itinerary.git").expect("targets");
    assert_eq!(
        targets,
        vec![
            (
                "itinerary".to_string(),
                "git@github.com:acme/itinerary.git".to_string()
            ),
            (
                "itin-alias".to_string(),
                "https://github.com/acme/itinerary".to_string()
            ),
        ]
    );
}

/// An origin no record matches gets one new record, named the way the
/// daemon's own auto-registration names it.
#[test]
fn pin_targets_names_a_new_record_from_the_origin() {
    let origin = "https://github.com/acme/itinerary.git";
    let targets = pin_targets(&[], origin).expect("targets");
    let expected = trusty_mpm::project::derive_name_from_url(origin).expect("a name");
    assert_eq!(targets, vec![(expected, origin.to_string())]);
}
