//! Tests for the git-init-offer prompt wiring at the `tm sessions
//! instructions` entry point (#7673 round 3 follow-up).
//!
//! Split out with `#[path]` so `instructions.rs` stays a focused CLI file.

use super::*;
use tempfile::TempDir;

/// FAILS BEFORE THIS FOLLOW-UP: `compose_session_instructions_with_roster`
/// had no seam at all, so the non-interactive default could not be proven
/// without mutating the test process's own stdin. A marker-less, non-git
/// directory still seeds its `CLAUDE.md` with no `.git` created when
/// `should_init` is `None` — the one entry point this prompt reaches with no
/// git-project guard already in front of it.
#[test]
fn compose_session_instructions_declines_git_init_without_a_prompt() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("project");
    std::fs::create_dir_all(&dir).unwrap();

    let (_, output, _) = compose_session_instructions_with_roster_and_init(&dir, None, None)
        .expect("a plain directory still composes instructions");

    assert!(output.claude_md_created);
    assert!(dir.join("CLAUDE.md").is_file());
    assert!(
        !dir.join(".git").exists(),
        "a None should_init must never create a repository"
    );
}

/// FAILS BEFORE THIS FOLLOW-UP: the same directory, but with an accepting
/// closure injected directly — never real stdin — proving the TTY-capable
/// path actually runs `git init` through the shared
/// `trusty_common::git::command_in` helper, rather than a second
/// `Command::new("git")`.
#[test]
fn compose_session_instructions_runs_git_init_when_the_prompt_accepts() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("project");
    std::fs::create_dir_all(&dir).unwrap();
    let mut accept = || true;

    let (_, output, _) = compose_session_instructions_with_roster_and_init(
        &dir,
        None,
        Some(&mut accept as &mut dyn FnMut() -> bool),
    )
    .expect("a plain directory still composes instructions");

    assert!(output.claude_md_created);
    assert!(dir.join("CLAUDE.md").is_file());
    assert!(
        dir.join(".git").is_dir(),
        "an accepted offer must have actually run git init"
    );
}

/// FAILS BEFORE THE #7774 REVIEW FIX: an accepted offer whose `git init` exited
/// non-zero was read as a decline, so `CLAUDE.md` was still seeded into the
/// non-git directory. A `.git` FILE with no `gitdir:` line is a directory no
/// repository check detects and `git init` cannot initialise.
#[test]
fn compose_session_instructions_refuses_to_seed_when_git_init_fails() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("project");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".git"), "not a gitfile\n").unwrap();
    let mut accept = || true;

    let err = compose_session_instructions_with_roster_and_init(
        &dir,
        None,
        Some(&mut accept as &mut dyn FnMut() -> bool),
    )
    .expect_err("a failed git init must refuse the seed");

    assert!(format!("{err:#}").contains("`git init` failed"), "{err:#}");
    assert!(
        !dir.join("CLAUDE.md").exists(),
        "no CLAUDE.md may be seeded after git init fails"
    );
}

/// The decision half of [`stdin_git_init_prompt`] — never a real TTY check in
/// a test, but the function itself degrades correctly when one is absent. This
/// asserts the shape compiles and returns `None` is exercised implicitly by
/// the `cargo test` harness itself always running with stdin redirected
/// (never a TTY), which is the same non-interactive case every CI run hits.
#[test]
fn stdin_git_init_prompt_is_none_under_the_test_harness() {
    assert!(
        stdin_git_init_prompt(std::path::Path::new("/tmp/7673-not-a-tty-probe")).is_none(),
        "cargo test's stdin is never a TTY"
    );
}

/// FAILS BEFORE THIS ROUND (#7673 round 3 review, LOW): the prompt text never
/// named the directory that would be initialised.
#[test]
fn git_init_prompt_text_names_the_directory() {
    let text = git_init_prompt_text(std::path::Path::new("/Users/ada/projects"));
    assert!(text.contains("/Users/ada/projects"), "{text}");
    assert!(text.contains("initialise one? [y/N]"), "{text}");
}
