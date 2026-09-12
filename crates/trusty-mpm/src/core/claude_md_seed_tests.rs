//! Tests for the seed-shape detector and the seed-site guard (#7673).
//!
//! Split out with `#[path]` so `claude_md_seed.rs` stays under the 500-SLOC
//! production cap.

use super::*;
use tempfile::TempDir;

#[test]
fn the_stub_is_a_seed_template() {
    assert!(is_seed_template(CLAUDE_MD_STUB));
}

/// Claude Code strips HTML comments when it loads a memory file, so the copy an
/// operator SEES is the stub minus its comments. The detector has to agree with
/// that view or the 🔴 verdict never fires on the file that motivated #7673.
#[test]
fn a_stub_with_the_comments_stripped_is_still_a_seed_template() {
    let mut stripped = String::new();
    let mut rest = CLAUDE_MD_STUB;
    while let Some(start) = rest.find("<!--") {
        stripped.push_str(&rest[..start]);
        let end = rest[start..].find("-->").expect("closed comment");
        rest = &rest[start + end + 3..];
    }
    stripped.push_str(rest);

    assert_ne!(stripped, CLAUDE_MD_STUB, "the fixture must differ verbatim");
    assert!(is_seed_template(&stripped));
}

#[test]
fn a_stub_with_one_added_line_is_not_a_seed_template() {
    let edited = format!("{CLAUDE_MD_STUB}\nWe deploy with `make ship`.\n");
    assert!(
        !is_seed_template(&edited),
        "one line of real content must demote the file to ⚠️, never 🔴"
    );
}

#[test]
fn an_unrelated_file_is_not_a_seed_template() {
    assert!(!is_seed_template("# My Project\n\nRun `cargo test`.\n"));
}

#[test]
fn the_home_refusal_names_the_path() {
    let msg = SeedRefusal::Home.message(Path::new("/Users/ada"));
    assert!(msg.contains("/Users/ada"), "{msg}");
    assert!(msg.contains("home directory"), "{msg}");
}

/// FAILS BEFORE THIS CHANGE: nothing consulted the home directory at all, so
/// the seeder wrote `$HOME/CLAUDE.md` — the 2026-09-12 finding.
#[test]
fn seeding_into_home_is_refused() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    // A dotfiles repo makes `$HOME` look exactly like a project root; the home
    // refusal must outrank the marker test, not fall through it.
    std::fs::create_dir_all(home.join(".git")).unwrap();

    assert_eq!(
        refuse_seed_at(&home, Some(&home)),
        Some(SeedRefusal::Home),
        "a git checkout at $HOME is still $HOME"
    );
}

#[test]
fn seeding_into_a_bare_directory_is_refused() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("scratch");
    std::fs::create_dir_all(&dir).unwrap();
    let home = tmp.path().join("home");

    assert_eq!(
        refuse_seed_at(&dir, Some(&home)),
        Some(SeedRefusal::NotAProjectRoot)
    );
}

#[test]
fn a_git_checkout_is_a_project_root() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("repo");
    std::fs::create_dir_all(dir.join(".git")).unwrap();

    assert_eq!(refuse_seed_at(&dir, Some(tmp.path())), None);
}

/// A linked worktree carries `.git` as a FILE, not a directory.
#[test]
fn a_git_worktree_pointer_file_is_a_project_root() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("wt");
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join(".git"), "gitdir: /elsewhere/.git/worktrees/wt\n").unwrap();

    assert_eq!(refuse_seed_at(&dir, Some(tmp.path())), None);
}

#[test]
fn a_harness_root_is_a_project_root() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("registered");
    std::fs::create_dir_all(dir.join(".trusty-mpm")).unwrap();

    assert_eq!(refuse_seed_at(&dir, Some(tmp.path())), None);
}

#[test]
fn a_bare_directory_is_not_a_project_root() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(
        refuse_seed_at(tmp.path(), None),
        Some(SeedRefusal::NotAProjectRoot),
        "with no home to compare against, the marker test still decides"
    );
}
