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
    // refusal must be first and unconditional, not something a marker can
    // out-argue.
    std::fs::create_dir_all(home.join(".git")).unwrap();

    assert_eq!(
        refuse_seed_at(&home, Some(&home)),
        Some(SeedRefusal::Home),
        "a git checkout at $HOME is still $HOME"
    );
}

/// A directory ABOVE `$HOME` — `/`, `/Users`, `/home` — loads into every
/// session `$HOME` does and more, so it is refused on the same reasoning.
#[test]
fn seeding_above_the_home_directory_is_refused() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    assert_eq!(
        refuse_seed_at(tmp.path(), Some(&home)),
        Some(SeedRefusal::AboveHome)
    );
}

/// The CRITICAL regression this round fixes: the first round's marker test
/// refused a bare temp directory, which is exactly the shape
/// `tm sessions instructions --dir <dir>` is documented to accept on a
/// directory tm has never touched.
#[test]
fn a_bare_first_touch_directory_is_seeded() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("scratch");
    std::fs::create_dir_all(&dir).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    assert_eq!(refuse_seed_at(&dir, Some(&home)), None);
}

/// The other half of the same regression: `tm session start` already certifies
/// any directory inside a git work tree, at any depth, through
/// `harness_root_for`. The seed guard must not disagree with it.
#[test]
fn a_subdirectory_of_a_git_project_is_seeded() {
    let tmp = TempDir::new().unwrap();
    let repo = tmp.path().join("repo");
    let nested = repo.join("crates").join("thing");
    std::fs::create_dir_all(&nested).unwrap();
    if !git_init(&repo) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    assert_eq!(refuse_seed_at(&nested, Some(&home)), None);
}

/// A registered project that is not a git checkout keeps working too.
#[test]
fn a_harness_root_is_seeded() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("registered");
    std::fs::create_dir_all(dir.join(".trusty-mpm")).unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    assert_eq!(refuse_seed_at(&dir, Some(&home)), None);
}

/// With no home injected there is no home to seed above, so nothing is refused.
#[test]
fn no_injected_home_refuses_nothing() {
    let tmp = TempDir::new().unwrap();
    assert_eq!(refuse_seed_at(tmp.path(), None), None);
}

/// FAILS BEFORE THIS ROUND: `--dir ""` produced a bare `CLAUDE.md` whose parent
/// is the empty path, and the call site's `if let` then skipped the guard
/// entirely. The decision must fail CLOSED instead.
#[test]
fn a_path_with_no_directory_component_is_refused() {
    assert_eq!(
        refuse_seed_for(Path::new("CLAUDE.md"), Some(Path::new("/Users/ada"))),
        Some(SeedRefusal::NoDirectory)
    );
    assert_eq!(
        refuse_seed_for(Path::new("CLAUDE.md"), None),
        Some(SeedRefusal::NoDirectory),
        "an absent home must not turn the degenerate path back into a pass"
    );
}

/// `refuse_seed_for` must still be the ordinary guard for a real file path.
#[test]
fn a_file_path_inside_home_is_refused_through_refuse_seed_for() {
    let tmp = TempDir::new().unwrap();
    let home = tmp.path().join("home");
    std::fs::create_dir_all(&home).unwrap();

    assert_eq!(
        refuse_seed_for(&home.join("CLAUDE.md"), Some(&home)),
        Some(SeedRefusal::Home)
    );
}

/// `git init` in `dir`, reporting whether git was available at all.
fn git_init(dir: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init", "-q"])
        .output()
        .is_ok_and(|out| out.status.success())
}
