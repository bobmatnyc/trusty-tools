//! Unit tests for the bare-`tm`-outside-a-git-work-tree menu (#6666).
//!
//! Why: the whole point of `guided_outside_git` is that the routing decision,
//! the input parse, the advertised commands, and the orchestrator are all
//! reachable without a daemon, a terminal, or a spawned process. These tests
//! exercise each of those seams directly.
//! What: routing table, choice parsing, argv construction, the options block's
//! agreement with that argv, and the orchestrator's static / interactive /
//! quit / listing-failure paths.
//! Test: this file.

use std::cell::RefCell;
use std::path::Path;

use super::*;

// ── route_bare_tm ────────────────────────────────────────────────────────────

/// Inside a work tree the pre-#6666 guided default still owns the invocation.
#[test]
fn route_bare_tm_git_tree_is_guided() {
    assert_eq!(
        route_bare_tm(true, false, true, true),
        BareTmRoute::GuidedProject
    );
}

/// A managed pane keeps the guided default even with no work tree, so
/// `fallback_protected`'s #4061 settle retry is still reachable.
#[test]
fn route_bare_tm_managed_pane_is_guided() {
    assert_eq!(
        route_bare_tm(false, true, true, true),
        BareTmRoute::GuidedProject
    );
}

/// Outside a work tree on a real terminal: the menu, and it may block on input.
#[test]
fn route_bare_tm_outside_git_tty_is_interactive() {
    assert_eq!(
        route_bare_tm(false, false, true, true),
        BareTmRoute::OutsideGit { interactive: true }
    );
}

/// Piped stdin would EOF instantly, so the menu prints and exits.
#[test]
fn route_bare_tm_outside_git_piped_stdin_is_static() {
    assert_eq!(
        route_bare_tm(false, false, false, true),
        BareTmRoute::OutsideGit { interactive: false }
    );
}

/// Piped stdout must stay a clean pipeable listing, so no prompt is written.
#[test]
fn route_bare_tm_outside_git_piped_stdout_is_static() {
    assert_eq!(
        route_bare_tm(false, false, true, false),
        BareTmRoute::OutsideGit { interactive: false }
    );
}

/// The dispatch path bare `tm` actually takes, fed by the real classifier
/// rather than a hand-set boolean.
///
/// Why: #6666's contract has two halves — a plain directory reaches the menu,
/// and a git work tree reaches the unchanged guided default. A boolean-only
/// table proves neither half is wired to the predicate `run_guided_default`
/// evaluates, which is `classify_cwd_project(&cwd) != NotGit`.
fn route_for_dir(dir: &Path) -> BareTmRoute {
    use crate::commands::guided::{CwdProject, classify_cwd_project};
    route_bare_tm(
        !matches!(classify_cwd_project(dir), CwdProject::NotGit),
        false,
        true,
        true,
    )
}

/// Create an empty temp directory, replacing any leftover from a prior run.
fn tempdir_with_name(name: &str) -> std::path::PathBuf {
    let tmp = std::env::temp_dir().join(name);
    if tmp.exists() {
        std::fs::remove_dir_all(&tmp).ok();
    }
    std::fs::create_dir_all(&tmp).expect("create temp dir");
    tmp
}

/// A real git work tree takes the pre-#6666 guided default, unchanged.
#[test]
fn route_bare_tm_real_git_work_tree_takes_the_guided_dispatch() {
    let tmp = tempdir_with_name("trusty_test_outside_git_repo_6666");
    let ok = std::process::Command::new("git")
        .args(["init", "-q"])
        .current_dir(&tmp)
        .status()
        .map(|s| s.success())
        .unwrap_or(false);
    if !ok {
        // No git on this machine — the claim is untestable, not false.
        return;
    }
    assert_eq!(route_for_dir(&tmp), BareTmRoute::GuidedProject);
}

/// A real plain directory takes the #6666 menu.
#[test]
fn route_bare_tm_real_plain_dir_takes_the_outside_git_dispatch() {
    let tmp = tempdir_with_name("trusty_test_outside_git_plain_6666");
    // A temp dir that happens to sit inside someone's repo is not this case.
    use crate::commands::guided::{CwdProject, classify_cwd_project};
    if !matches!(classify_cwd_project(&tmp), CwdProject::NotGit) {
        return;
    }
    assert_eq!(
        route_for_dir(&tmp),
        BareTmRoute::OutsideGit { interactive: true }
    );
}

// ── parse_outside_git_choice ─────────────────────────────────────────────────

#[test]
fn parse_outside_git_choice_n_is_untracked() {
    for input in ["n", "N", " n \n", "n\r\n"] {
        assert_eq!(
            parse_outside_git_choice(input),
            OutsideGitChoice::UntrackedSession,
            "input {input:?}"
        );
    }
}

#[test]
fn parse_outside_git_choice_m_is_managed() {
    for input in ["m", "M", " m\n"] {
        assert_eq!(
            parse_outside_git_choice(input),
            OutsideGitChoice::ManagedSession,
            "input {input:?}"
        );
    }
}

/// A bare Enter and an EOF-empty read both quit — the documented default.
#[test]
fn parse_outside_git_choice_empty_is_quit() {
    for input in ["", "\n", "   "] {
        assert_eq!(
            parse_outside_git_choice(input),
            OutsideGitChoice::Quit,
            "input {input:?}"
        );
    }
}

#[test]
fn parse_outside_git_choice_unknown_is_quit() {
    for input in ["q", "yes", "1"] {
        assert_eq!(
            parse_outside_git_choice(input),
            OutsideGitChoice::Quit,
            "input {input:?}"
        );
    }
}

// ── choice_argv ──────────────────────────────────────────────────────────────

#[test]
fn outside_git_untracked_choice_maps_to_sessions_start_argv() {
    let argv = choice_argv(&OutsideGitChoice::UntrackedSession, Path::new("/tmp/plain"));
    assert_eq!(
        argv,
        Some(vec![
            "sessions".to_string(),
            "start".to_string(),
            "--dir".to_string(),
            "/tmp/plain".to_string(),
        ])
    );
}

#[test]
fn outside_git_managed_choice_maps_to_sessions_new_argv() {
    let argv = choice_argv(&OutsideGitChoice::ManagedSession, Path::new("/tmp/plain"));
    assert_eq!(
        argv,
        Some(vec![
            "sessions".to_string(),
            "new".to_string(),
            "/tmp/plain".to_string(),
            "--task".to_string(),
            String::new(),
        ])
    );
}

#[test]
fn outside_git_quit_choice_maps_to_no_argv() {
    assert_eq!(
        choice_argv(&OutsideGitChoice::Quit, Path::new("/tmp")),
        None
    );
}

/// Both argv forms must parse back through the real CLI into the `sessions`
/// group — the guarantee `dispatch_session_argv` relies on.
#[test]
fn outside_git_argv_parses_back_into_the_sessions_group() {
    use clap::Parser as _;

    for choice in [
        OutsideGitChoice::UntrackedSession,
        OutsideGitChoice::ManagedSession,
    ] {
        let argv = choice_argv(&choice, Path::new("/tmp/plain")).expect("action choice has argv");
        let full = std::iter::once("tm".to_string()).chain(argv.clone());
        let cli = crate::cli::Cli::try_parse_from(full)
            .unwrap_or_else(|e| panic!("argv {argv:?} must parse: {e}"));
        assert!(
            matches!(cli.command, Some(crate::cli::Command::Sessions { .. })),
            "argv {argv:?} must land in the sessions group"
        );
    }
}

// ── options_block ────────────────────────────────────────────────────────────

#[test]
fn outside_git_options_block_names_both_real_commands() {
    let block = options_block(Path::new("/tmp/plain"));
    assert!(
        block.contains("tm sessions start --dir /tmp/plain"),
        "block: {block}"
    );
    assert!(
        block.contains("tm sessions new /tmp/plain --task ''"),
        "block: {block}"
    );
    assert!(block.contains("[q] quit"), "block: {block}");
    assert!(
        block.contains("not inside a git work tree"),
        "block: {block}"
    );
}

/// The advertised text and the executed argv share one source, so every argv
/// token must appear in the block.
#[test]
fn outside_git_options_block_matches_choice_argv() {
    let cwd = Path::new("/tmp/plain");
    let block = options_block(cwd);
    for choice in [
        OutsideGitChoice::UntrackedSession,
        OutsideGitChoice::ManagedSession,
    ] {
        for token in choice_argv(&choice, cwd).expect("action choice has argv") {
            if token.is_empty() {
                continue;
            }
            assert!(
                block.contains(&token),
                "token {token:?} missing from block: {block}"
            );
        }
    }
}

// ── run_outside_git_menu ─────────────────────────────────────────────────────

/// Records what the orchestrator's injected edges were asked to do.
#[derive(Default)]
struct MenuSpy {
    listed: RefCell<u32>,
    read: RefCell<u32>,
    ran: RefCell<Vec<Vec<String>>>,
}

/// The non-TTY path lists sessions through the injected lister, prints the
/// options, and exits 0 without reading input or running an action.
#[tokio::test]
async fn outside_git_menu_static_lists_then_prints_options() {
    let spy = MenuSpy::default();
    let out = run_outside_git_menu(
        Path::new("/tmp/plain"),
        false,
        || async {
            *spy.listed.borrow_mut() += 1;
            Ok(())
        },
        || {
            *spy.read.borrow_mut() += 1;
            Ok(String::new())
        },
        |argv| async {
            spy.ran.borrow_mut().push(argv);
            Ok(())
        },
    )
    .await;

    assert!(out.is_ok(), "static path must exit 0: {out:?}");
    assert_eq!(*spy.listed.borrow(), 1, "the listing runs exactly once");
}

/// The same run proves the static path never blocks on stdin and never acts.
#[tokio::test]
async fn outside_git_menu_static_never_reads_or_runs() {
    let spy = MenuSpy::default();
    run_outside_git_menu(
        Path::new("/tmp/plain"),
        false,
        || async { Ok(()) },
        || {
            *spy.read.borrow_mut() += 1;
            Ok("n".to_string())
        },
        |argv| async {
            spy.ran.borrow_mut().push(argv);
            Ok(())
        },
    )
    .await
    .expect("static path must exit 0");

    assert_eq!(*spy.read.borrow(), 0, "no stdin read on a non-TTY");
    assert!(spy.ran.borrow().is_empty(), "no action on a non-TTY");
}

/// On a TTY the chosen key runs the exact argv the options block advertised.
#[tokio::test]
async fn outside_git_menu_tty_choice_runs_the_advertised_argv() {
    for (key, expected) in [
        (
            "n\n",
            vec!["sessions", "start", "--dir", "/tmp/plain"]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        ),
        (
            "m\n",
            vec!["sessions", "new", "/tmp/plain", "--task", ""]
                .into_iter()
                .map(str::to_string)
                .collect::<Vec<_>>(),
        ),
    ] {
        let spy = MenuSpy::default();
        run_outside_git_menu(
            Path::new("/tmp/plain"),
            true,
            || async { Ok(()) },
            || Ok(key.to_string()),
            |argv| async {
                spy.ran.borrow_mut().push(argv);
                Ok(())
            },
        )
        .await
        .unwrap_or_else(|e| panic!("key {key:?} must succeed: {e}"));

        assert_eq!(*spy.ran.borrow(), vec![expected], "key {key:?}");
    }
}

/// Enter (and `q`) quit without running anything, and still exit 0.
#[tokio::test]
async fn outside_git_menu_quit_runs_nothing() {
    for key in ["\n", "q\n", ""] {
        let spy = MenuSpy::default();
        run_outside_git_menu(
            Path::new("/tmp/plain"),
            true,
            || async { Ok(()) },
            || Ok(key.to_string()),
            |argv| async {
                spy.ran.borrow_mut().push(argv);
                Ok(())
            },
        )
        .await
        .unwrap_or_else(|e| panic!("key {key:?} must exit 0: {e}"));

        assert!(spy.ran.borrow().is_empty(), "key {key:?} must run nothing");
    }
}

/// An unreachable daemon must not hide the two commands — the listing failure
/// is reported and the options are still offered.
#[tokio::test]
async fn outside_git_menu_reports_a_listing_failure_and_still_offers_the_options() {
    let spy = MenuSpy::default();
    let out = run_outside_git_menu(
        Path::new("/tmp/plain"),
        true,
        || async { Err(anyhow::anyhow!("daemon unreachable")) },
        || {
            *spy.read.borrow_mut() += 1;
            Ok("q\n".to_string())
        },
        |argv| async {
            spy.ran.borrow_mut().push(argv);
            Ok(())
        },
    )
    .await;

    assert!(out.is_ok(), "a dead daemon must not fail the menu: {out:?}");
    assert_eq!(*spy.read.borrow(), 1, "the prompt is still offered");
}

// ── dispatch_session_argv ────────────────────────────────────────────────────

/// A non-`sessions` argv is an internal bug, not a silently different command.
#[tokio::test]
async fn outside_git_dispatch_rejects_a_non_session_argv() {
    let client = reqwest::Client::new();
    let out =
        dispatch_session_argv(&client, "http://127.0.0.1:1", vec!["status".to_string()]).await;
    assert!(out.is_err(), "a non-sessions argv must be refused");
}
