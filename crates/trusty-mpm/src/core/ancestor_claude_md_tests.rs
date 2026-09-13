//! Tests for the ancestor memory-file scan and the launch WARN (#7673).
//!
//! Split out with `#[path]` so `ancestor_claude_md.rs` stays under the 500-SLOC
//! production cap.

use super::*;
use crate::core::instruction_pipeline::CLAUDE_MD_STUB;
use tempfile::TempDir;

/// `<tmp>/ancestor/project`, canonicalised so assertions compare the same
/// spelling the scan reports.
fn fixture() -> (TempDir, PathBuf, PathBuf) {
    let tmp = TempDir::new().unwrap();
    let root = std::fs::canonicalize(tmp.path()).unwrap();
    let ancestor = root.join("ancestor");
    let project = ancestor.join("project");
    std::fs::create_dir_all(&project).unwrap();
    (tmp, ancestor, project)
}

fn none() -> BTreeSet<String> {
    BTreeSet::new()
}

#[test]
fn the_token_estimate_divides_bytes_by_four() {
    let file = AncestorMemoryFile {
        path: PathBuf::from("/x/CLAUDE.md"),
        bytes: 4001,
        seed_template: false,
        excluded: false,
    };
    assert_eq!(BYTES_PER_TOKEN, 4);
    assert_eq!(file.token_estimate(), 1000);
}

#[test]
fn the_summary_names_the_divisor() {
    let file = AncestorMemoryFile {
        path: PathBuf::from("/x/CLAUDE.md"),
        bytes: 400,
        seed_template: true,
        excluded: false,
    };
    let line = file.summary();
    assert!(line.contains("/x/CLAUDE.md"), "{line}");
    assert!(line.contains("400 B"), "{line}");
    assert!(line.contains("~100 tokens at bytes/4"), "{line}");
    assert!(line.contains("tm seed template"), "{line}");
}

#[test]
fn no_ancestors_yields_nothing() {
    let (_tmp, _ancestor, project) = fixture();
    assert!(scan_with_excludes(&project, &none()).is_empty());
}

/// FAILS BEFORE THIS CHANGE: nothing walked above the project root, so the
/// `$HOME` seed template was invisible to every surface in the harness.
#[test]
fn a_seed_ancestor_is_reported_as_a_seed() {
    let (_tmp, ancestor, project) = fixture();
    let file = ancestor.join("CLAUDE.md");
    std::fs::write(&file, CLAUDE_MD_STUB).unwrap();

    let found = scan_with_excludes(&project, &none());

    assert_eq!(found.len(), 1, "{found:?}");
    assert_eq!(found[0].path, file);
    assert!(found[0].seed_template);
    assert!(!found[0].excluded);
    assert_eq!(found[0].bytes, CLAUDE_MD_STUB.len() as u64);
}

#[test]
fn a_content_ancestor_is_not_a_seed() {
    let (_tmp, ancestor, project) = fixture();
    std::fs::write(ancestor.join("CLAUDE.md"), "# Monorepo\n\nUse pnpm.\n").unwrap();

    let found = scan_with_excludes(&project, &none());

    assert_eq!(found.len(), 1);
    assert!(!found[0].seed_template);
}

/// The project's OWN instructions are not an ancestor and must never appear —
/// the repair would otherwise rename or exclude the file the session needs.
#[test]
fn a_project_root_file_is_never_reported() {
    let (_tmp, _ancestor, project) = fixture();
    std::fs::write(project.join("CLAUDE.md"), CLAUDE_MD_STUB).unwrap();

    assert!(scan_with_excludes(&project, &none()).is_empty());
}

#[test]
fn every_memory_file_shape_is_reported() {
    let (_tmp, ancestor, project) = fixture();
    std::fs::create_dir_all(ancestor.join(".claude")).unwrap();
    for relative in ["CLAUDE.md", "CLAUDE.local.md", ".claude/CLAUDE.md"] {
        std::fs::write(ancestor.join(relative), "# notes\n").unwrap();
    }

    let found = scan_with_excludes(&project, &none());

    assert_eq!(found.len(), 3, "{found:?}");
}

#[test]
fn an_excluded_ancestor_is_flagged_excluded() {
    let (_tmp, ancestor, project) = fixture();
    let file = ancestor.join("CLAUDE.md");
    std::fs::write(&file, "# notes\n").unwrap();
    let excludes: BTreeSet<String> = [file.display().to_string()].into_iter().collect();

    let found = scan_with_excludes(&project, &excludes);

    assert_eq!(found.len(), 1);
    assert!(found[0].excluded);
}

#[test]
fn a_large_ancestor_is_not_opened_or_called_a_seed() {
    let (_tmp, ancestor, project) = fixture();
    let big = "x".repeat((MAX_SEED_BYTES + 1) as usize);
    std::fs::write(ancestor.join("CLAUDE.md"), &big).unwrap();

    let found = scan_with_excludes(&project, &none());

    assert_eq!(found.len(), 1);
    assert!(!found[0].seed_template);
}

#[test]
fn warn_is_silent_when_there_are_no_ancestors() {
    assert_eq!(warning_text(&[]), None);
}

#[test]
fn an_excluded_ancestor_produces_no_warning() {
    let found = vec![AncestorMemoryFile {
        path: PathBuf::from("/x/CLAUDE.md"),
        bytes: 400,
        seed_template: false,
        excluded: true,
    }];
    assert_eq!(warning_text(&found), None);
}

#[test]
fn the_warning_names_both_remedies() {
    let found = vec![AncestorMemoryFile {
        path: PathBuf::from("/Users/ada/CLAUDE.md"),
        bytes: 1200,
        seed_template: true,
        excluded: false,
    }];

    let text = warning_text(&found).expect("a loading ancestor warns");

    assert!(text.contains("/Users/ada/CLAUDE.md"), "{text}");
    assert!(text.contains("~300 tokens"), "{text}");
    assert!(text.contains("delete the file"), "{text}");
    assert!(text.contains("claudeMdExcludes"), "{text}");
}

#[test]
fn the_rename_target_carries_the_date() {
    assert_eq!(
        stale_seed_name(Path::new("/Users/ada/CLAUDE.md"), "20260912"),
        PathBuf::from("/Users/ada/CLAUDE.md.stale-seed-20260912")
    );
}

/// `git -C <dir> init -q`, reporting whether git was available at all.
fn git_init(dir: &Path) -> bool {
    std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["init", "-q"])
        .output()
        .is_ok_and(|out| out.status.success())
}

/// FAILS BEFORE THIS ROUND (#7673 review, CRITICAL): `scan_with_excludes`
/// walked literal ancestors of whatever directory it was handed, with no
/// git-boundary awareness, so a caller that passed a SUBDIRECTORY of a
/// project — exactly what `tm doctor` does when run from `crates/trusty-mpm`
/// in this repo — reported the project's own root `CLAUDE.md` as a stray
/// ancestor. `scan` must resolve to the git toplevel first and never report a
/// file at or below it.
#[test]
fn scan_does_not_report_the_git_roots_own_claude_md_for_a_nested_project_root() {
    let tmp = TempDir::new().unwrap();
    let repo = std::fs::canonicalize(tmp.path()).unwrap().join("repo");
    let nested = repo.join("crates").join("thing");
    std::fs::create_dir_all(&nested).unwrap();
    if !git_init(&repo) {
        eprintln!("#7673 tests: git unavailable, skipping");
        return;
    }
    std::fs::write(repo.join("CLAUDE.md"), "# Root project\n\nUse cargo.\n").unwrap();

    let found = scan(&nested, None, None);

    assert!(found.is_empty(), "{found:?}");
}

/// The registered-project half of [`resolve_project_root`]: a non-git project
/// marked by `.trusty-mpm` is found from a nested subdirectory too.
#[test]
fn resolve_project_root_finds_the_registered_marker_above_a_nested_dir() {
    let tmp = TempDir::new().unwrap();
    let project = std::fs::canonicalize(tmp.path())
        .unwrap()
        .join("registered");
    std::fs::create_dir_all(project.join(".trusty-mpm")).unwrap();
    let nested = project.join("sub").join("dir");
    std::fs::create_dir_all(&nested).unwrap();

    assert_eq!(resolve_project_root(&nested), project);
}

/// With neither a git ancestor nor a `.trusty-mpm` marker, `resolve_project_root`
/// falls back to the given directory itself, canonicalized.
#[test]
fn resolve_project_root_falls_back_to_the_given_dir_with_no_git_or_marker() {
    let tmp = TempDir::new().unwrap();
    let dir = std::fs::canonicalize(tmp.path()).unwrap().join("scratch");
    std::fs::create_dir_all(&dir).unwrap();

    assert_eq!(resolve_project_root(&dir), dir);
}

/// `scan` (as opposed to `scan_with_excludes`) resolves the exclude set from the
/// project's own settings layers.
#[test]
fn scan_reads_the_excludes_from_the_project_layer() {
    let (_tmp, ancestor, project) = fixture();
    let file = ancestor.join("CLAUDE.md");
    std::fs::write(&file, "# notes\n").unwrap();
    let settings = project.join(".claude").join("settings.local.json");
    std::fs::create_dir_all(settings.parent().unwrap()).unwrap();
    std::fs::write(
        &settings,
        serde_json::json!({ "claudeMdExcludes": [file.display().to_string()] }).to_string(),
    )
    .unwrap();

    let found = scan(&project, None, None);

    assert_eq!(found.len(), 1);
    assert!(found[0].excluded, "{found:?}");
}
