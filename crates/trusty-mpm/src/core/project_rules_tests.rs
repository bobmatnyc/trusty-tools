//! Tests for the dispatch-time project rules block (#7684).

use super::project_rules::*;

#[test]
fn marked_regions_are_extracted_in_order() {
    let body = format!(
        "# T\nprivate\n{RULES_OPEN}\nrule one\n{RULES_CLOSE}\nskip\n{RULES_OPEN}rule two{RULES_CLOSE}\ntail"
    );
    assert_eq!(extract(&body).as_deref(), Some("rule one\n\nrule two"));
}

#[test]
fn an_unmarked_file_is_carried_whole() {
    assert_eq!(
        extract("\n# Rules\nall of it\n").as_deref(),
        Some("# Rules\nall of it")
    );
}

#[test]
fn a_blank_file_yields_nothing() {
    assert_eq!(extract("  \n"), None);
    assert_eq!(extract(&format!("x\n{RULES_OPEN}\n{RULES_CLOSE}")), None);
}

#[test]
fn rules_are_found_from_a_subdirectory() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::write(dir.path().join(CLAUDE_MD), "use --no-fail-fast").expect("write");
    let sub = dir.path().join("a/b");
    std::fs::create_dir_all(&sub).expect("mkdir");
    assert_eq!(rules_for(&sub).as_deref(), Some("use --no-fail-fast"));
}

#[test]
fn block_carries_the_heading_and_rules() {
    let block = rules_block("rule");
    assert!(block.starts_with("\n\n---\n"), "{block}");
    assert!(block.contains(PROJECT_RULES_HEADING), "{block}");
    assert!(block.ends_with("rule"), "{block}");
}

#[test]
fn an_over_cap_block_is_cut_with_a_notice() {
    let rules = format!("{}\n", "x".repeat(100)).repeat(PROJECT_RULES_CAP_BYTES / 50);
    let block = rules_block(&rules);
    assert!(
        block.len() < PROJECT_RULES_CAP_BYTES + 200,
        "{}",
        block.len()
    );
    assert!(block.ends_with("read CLAUDE.md for the rest]"));
}

/// The rules this repository's own CLAUDE.md must hand every subagent once
/// `omitClaudeMd` stops it loading the file (#7684). Removing a marker region
/// that carries one of them fails this.
#[test]
fn this_repos_claude_md_carries_the_build_and_test_rules() {
    let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../CLAUDE.md");
    let body = std::fs::read_to_string(&path).expect("repo CLAUDE.md");
    assert!(
        body.contains(RULES_OPEN),
        "CLAUDE.md marks no agent-rules region"
    );
    let rules = extract(&body).expect("rules");
    for needle in [
        "cargo test -p <crate> --no-fail-fast",
        "cargo clippy -p <crate> --all-targets -- -D warnings",
        "CARGO_TARGET_DIR",
        "Scope down, never scope away",
        "Rust Test Ladder",
        "changelog.d",
        "SLOC hard cap",
        "No `unwrap()` in library code",
        "Stage by name, never `-A`",
    ] {
        assert!(rules.contains(needle), "agent rules lack {needle:?}");
    }
    assert!(rules.len() <= PROJECT_RULES_CAP_BYTES, "{}", rules.len());
}

/// #7684: an unreadable `CLAUDE.md` puts a notice in the block, never nothing.
#[test]
fn an_unreadable_claude_md_yields_a_notice() {
    let dir = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(dir.path().join(CLAUDE_MD)).expect("mkdir CLAUDE.md");
    let notice = rules_for(dir.path()).expect("a notice, not None");
    assert!(notice.starts_with(RULES_UNAVAILABLE), "{notice}");
    assert!(notice.contains(CLAUDE_MD), "{notice}");
    assert!(!notice.contains('\n'), "{notice}");

    let bad = tempfile::tempdir().expect("tempdir");
    std::fs::write(bad.path().join(CLAUDE_MD), [0xff, 0xfe, 0x00]).expect("write");
    let notice = rules_for(bad.path()).expect("a notice for invalid UTF-8");
    assert!(notice.starts_with(RULES_UNAVAILABLE), "{notice}");
    assert!(rules_block(&notice).contains(PROJECT_RULES_HEADING));
}
