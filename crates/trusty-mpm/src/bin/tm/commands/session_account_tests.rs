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

/// 🔴 #8914 MEDIUM: the project-wide re-pin is printed with the account it
/// replaces.
#[test]
fn pin_notice_names_the_account_it_replaces() {
    assert_eq!(
        pin_notice("itinerary", Some("bob-duetto"), "bobmatnyc"),
        "tm: every session of project 'itinerary' now runs as gh account 'bobmatnyc' \
         (was 'bob-duetto')"
    );
    assert!(pin_notice("itinerary", None, "bobmatnyc").ends_with("(was unpinned)"));
    assert!(pin_notice("itinerary", Some(" "), "bobmatnyc").ends_with("(was unpinned)"));
}

/// 🔴 #8914 MEDIUM: an SSH origin is named as outside the pin; HTTPS is not.
#[test]
fn transport_notice_says_when_git_uses_ssh() {
    for origin in [
        "git@github.com:acme/itinerary.git",
        "ssh://git@github.com/acme/itinerary.git",
    ] {
        let notice = transport_notice(origin, "bobmatnyc");
        assert!(
            notice.contains("uses SSH") && notice.contains("does not pin"),
            "{notice}"
        );
    }
    let https = transport_notice("https://github.com/acme/itinerary", "bobmatnyc");
    assert!(https.contains("HTTPS git as 'bobmatnyc'"), "{https}");
}

/// The stdin token is one trimmed token.
#[test]
fn read_stdin_token_trims_one_token() {
    let token = read_stdin_token("  ghp_abc123\n".as_bytes()).expect("one token");
    assert_eq!(token, "ghp_abc123");
}

/// FAIL-OPEN CHECK: empty stdin, or two words, is refused rather than stored.
#[test]
fn read_stdin_token_refuses_empty_or_spaced_input() {
    for input in ["", " \n", "ghp_a ghp_b\n"] {
        let err = read_stdin_token(input.as_bytes()).expect_err("must be refused");
        assert!(err.to_string().contains("exactly one token"), "{err}");
    }
}

/// `--account-token-stdin` needs `--account`: a token for no account is
/// refused at parse time.
#[test]
fn cli_account_token_stdin_requires_an_account() {
    let bare = Cli::try_parse_from([
        "tm",
        "sessions",
        "new",
        "/w",
        "--task",
        "t",
        "--account-token-stdin",
    ]);
    assert!(bare.is_err(), "a token for no account must not parse");
    let cli = Cli::try_parse_from([
        "tm",
        "--account",
        "bobmatnyc",
        "--account-token-stdin",
        "sessions",
        "new",
        "/w",
        "--task",
        "t",
    ])
    .expect("parses");
    assert!(cli.account_token_stdin);
}
