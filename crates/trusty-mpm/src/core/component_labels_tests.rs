//! Tests for [`super`] — the accepted component-label set (#7123).
//!
//! Every test but one builds a throwaway workspace in a `TempDir`, so it does
//! not depend on the checkout it runs in.
//! `resolve_accepts_every_live_workspace_crate` is the exception (#7182): it
//! runs the real derivation against THIS checkout's actual `crates/*`
//! listing, so a drift a synthetic fixture could not see still fails CI.

use std::fs;
use std::path::Path;

use tempfile::TempDir;

use super::{ComponentLabels, CrateOwnership, cargo_workspace_root, expand_member, package_name};
use crate::core::trusty_tools_config::ResolvedTicketing;

/// Write a crate directory with a `[package] name`.
fn crate_at(root: &Path, dir: &str, package: &str) {
    let path = root.join(dir);
    fs::create_dir_all(&path).unwrap();
    fs::write(
        path.join("Cargo.toml"),
        format!("[package]\nname = \"{package}\"\nversion = \"0.1.0\"\n"),
    )
    .unwrap();
}

/// A workspace shaped like this repo's: a `crates/*` glob, one crate whose
/// package name differs from its directory (`tga`), and one nested member
/// listed by full path.
fn workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\", \"crates/trusty-audit/ui/src-tauri\"]\n\
         resolver = \"2\"\n",
    )
    .unwrap();
    crate_at(root, "crates/trusty-mpm", "trusty-mpm");
    crate_at(root, "crates/trusty-audit", "trusty-audit");
    crate_at(root, "crates/trusty-console", "trusty-console");
    crate_at(root, "crates/trusty-git-analytics", "tga");
    crate_at(root, "crates/trusty-review", "trusty_review");
    crate_at(root, "crates/trusty-audit/ui/src-tauri", "trusty-audit-ui");
    tmp
}

#[test]
fn resolve_accepts_every_workspace_crate_label() {
    let tmp = workspace();
    let labels = ComponentLabels::resolve(&ResolvedTicketing::default(), Some(tmp.path()));
    for expected in [
        "trusty-mpm",
        "trusty-audit",
        "trusty-console",
        "tga",
        "trusty-audit-ui",
    ] {
        assert!(
            labels.accepts(expected),
            "expected `{expected}` to be accepted; set was {:?}",
            labels.names()
        );
    }
}

#[test]
fn an_underscored_package_is_accepted_hyphenated() {
    // A GitHub label name cannot contain `_`, so the package `trusty_review` is
    // labelled `trusty-review`. Both spellings satisfy the rule.
    let tmp = workspace();
    let labels = ComponentLabels::resolve(&ResolvedTicketing::default(), Some(tmp.path()));
    assert!(labels.accepts("trusty_review"), "the package name");
    assert!(
        labels.accepts("trusty-review"),
        "the label spelling; set was {:?}",
        labels.names()
    );
}

#[test]
fn a_nested_member_contributes_no_path_segment() {
    // #7123: deriving from the directory rather than the package would accept
    // `src-tauri`, which names no component and nothing labels an issue with.
    let tmp = workspace();
    let labels = ComponentLabels::resolve(&ResolvedTicketing::default(), Some(tmp.path()));
    assert!(!labels.accepts("src-tauri"), "set was {:?}", labels.names());
    assert!(labels.accepts("trusty-audit-ui"), "the package it declares");
}

#[test]
fn resolve_finds_the_workspace_from_a_nested_directory() {
    let tmp = workspace();
    let nested = tmp.path().join("crates/trusty-mpm");
    let labels = ComponentLabels::resolve(&ResolvedTicketing::default(), Some(&nested));
    assert!(labels.accepts("trusty-console"));
}

#[test]
fn resolve_without_a_workspace_is_the_seed_table() {
    let tmp = TempDir::new().unwrap();
    let labels = ComponentLabels::resolve(&ResolvedTicketing::default(), Some(tmp.path()));
    assert_eq!(labels.names(), ["trusty-mpm"]);
}

#[test]
fn resolve_with_no_directory_is_the_seed_table() {
    let labels = ComponentLabels::resolve(&ResolvedTicketing::default(), None);
    assert_eq!(labels.names(), ["trusty-mpm"]);
}

#[test]
fn resolve_ignores_a_malformed_manifest() {
    let tmp = TempDir::new().unwrap();
    fs::write(tmp.path().join("Cargo.toml"), "[workspace\nmembers = [").unwrap();
    let labels = ComponentLabels::resolve(&ResolvedTicketing::default(), Some(tmp.path()));
    assert_eq!(labels.names(), ["trusty-mpm"]);
}

#[test]
fn resolve_never_accepts_a_workstream_label() {
    let tmp = workspace();
    let labels = ComponentLabels::resolve(&ResolvedTicketing::default(), Some(tmp.path()));
    assert!(!labels.accepts("ws/trusty-tools-ec"));
}

#[test]
fn from_names_deduplicates() {
    let labels = ComponentLabels::from_names(
        ["a", "b", "a", ""]
            .into_iter()
            .map(ToString::to_string)
            .collect::<Vec<_>>(),
    );
    assert_eq!(labels.names(), ["a", "b"]);
}

#[test]
fn workspace_root_is_found_from_a_nested_directory() {
    let tmp = workspace();
    let found = cargo_workspace_root(&tmp.path().join("crates/trusty-audit/ui/src-tauri"))
        .expect("the fixture declares a workspace");
    assert_eq!(
        fs::canonicalize(&found).unwrap(),
        fs::canonicalize(tmp.path()).unwrap()
    );
}

#[test]
fn no_workspace_root_outside_a_workspace() {
    let tmp = TempDir::new().unwrap();
    let dir = tmp.path().join("nothing/here");
    fs::create_dir_all(&dir).unwrap();
    assert_eq!(cargo_workspace_root(&dir), None);
}

#[test]
fn member_glob_expands_one_level() {
    let tmp = workspace();
    let dirs = expand_member(tmp.path(), "crates/*");
    let names: Vec<String> = dirs
        .iter()
        .filter_map(|d| d.file_name()?.to_str().map(ToString::to_string))
        .collect();
    assert_eq!(
        names,
        [
            "trusty-audit",
            "trusty-console",
            "trusty-git-analytics",
            "trusty-mpm",
            "trusty-review"
        ]
    );
}

#[test]
fn member_literal_path_is_kept() {
    let tmp = workspace();
    let dirs = expand_member(tmp.path(), "crates/trusty-audit/ui/src-tauri");
    assert_eq!(dirs, [tmp.path().join("crates/trusty-audit/ui/src-tauri")]);
}

#[test]
fn member_pattern_that_matches_nothing_yields_nothing() {
    let tmp = workspace();
    assert!(expand_member(tmp.path(), "services/*").is_empty());
}

/// #7182 (recurrence of #7123): every OTHER test in this file resolves
/// against a synthetic fixture, so nothing here would notice `resolve` ever
/// drifting from THIS checkout's actual `crates/*` listing — the exact gap a
/// hand-maintained allow-list would also have had. This runs the real
/// derivation against the real repository: every directory `crates/*`
/// contains (this checkout's ground truth, walked fresh — never a hardcoded
/// name) must resolve to an accepted package-name label, `trusty-audit`
/// included.
#[test]
fn resolve_accepts_every_live_workspace_crate() {
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
        .ancestors()
        .nth(2)
        .expect("crates/trusty-mpm/src/core is 3 levels under the workspace root");
    let labels = ComponentLabels::resolve(&ResolvedTicketing::default(), Some(root));
    let crates_dir = root.join("crates");
    let entries = fs::read_dir(&crates_dir)
        .unwrap_or_else(|e| panic!("read_dir({}): {e}", crates_dir.display()));
    for entry in entries {
        let dir = entry.unwrap().path();
        if !dir.is_dir() {
            continue;
        }
        let Some(name) = package_name(&dir) else {
            continue;
        };
        assert!(
            labels.accepts(&name),
            "crates/{} declares package `{name}`, not accepted; set was {:?}",
            dir.file_name().unwrap().to_string_lossy(),
            labels.names()
        );
    }
}

// ── CrateOwnership — which crate owns a changed path (#7274) ──────────────

/// A map for the lookup tests, without touching a filesystem.
fn ownership() -> CrateOwnership {
    CrateOwnership::from_members([
        ("crates/trusty-mpm/".to_string(), "trusty-mpm".to_string()),
        (
            "crates/trusty-audit/".to_string(),
            "trusty-audit".to_string(),
        ),
        (
            "crates/trusty-audit/ui/src-tauri/".to_string(),
            "trusty-audit-ui".to_string(),
        ),
        // A sibling whose directory name has another member's as a prefix.
        (
            "crates/trusty-mpm-gui/".to_string(),
            "trusty-mpm-gui".to_string(),
        ),
    ])
}

#[test]
fn ownership_labels_a_path_under_a_member() {
    let map = CrateOwnership::resolve(Some(workspace().path()));
    assert_eq!(
        map.labels_for_paths(&["crates/trusty-mpm/src/lib.rs"]),
        vec!["trusty-mpm".to_string()]
    );
    // The package name, hyphenated — never the directory name.
    assert_eq!(
        map.labels_for_paths(&["crates/trusty-git-analytics/src/main.rs"]),
        vec!["tga".to_string()]
    );
    assert_eq!(
        map.labels_for_paths(&["crates/trusty-review/src/lib.rs"]),
        vec!["trusty-review".to_string()],
        "a package spelled `trusty_review` is labelled with a hyphen"
    );
}

#[test]
fn ownership_prefers_the_longest_matching_member() {
    // The nested Tauri member sits INSIDE `crates/trusty-audit/`, so a plain
    // first-match scan would label its files `trusty-audit`.
    assert_eq!(
        ownership().labels_for_paths(&["crates/trusty-audit/ui/src-tauri/src/main.rs"]),
        vec!["trusty-audit-ui".to_string()]
    );
}

/// #7274 round 2: `crates/trusty-mpm-gui/` starts with `crates/trusty-mpm`,
/// so the trailing slash on each member key is what keeps a GUI file from
/// earning the `trusty-mpm` label as well.
#[test]
fn ownership_does_not_leak_across_a_sibling_prefix() {
    assert_eq!(
        ownership().labels_for_paths(&["crates/trusty-mpm-gui/src/main.rs"]),
        vec!["trusty-mpm-gui".to_string()],
        "a sibling whose name extends another member's earns only its own label"
    );
    assert_eq!(
        ownership().labels_for_paths(&["crates/trusty-mpm/src/lib.rs"]),
        vec!["trusty-mpm".to_string()],
        "and the shorter sibling is unaffected"
    );
    // With the longer sibling absent from the map, the trailing slash is the
    // ONLY thing standing between the GUI path and a `trusty-mpm` label — the
    // longest-first sort cannot help when there is nothing longer to find.
    let mpm_only = CrateOwnership::from_members([(
        "crates/trusty-mpm/".to_string(),
        "trusty-mpm".to_string(),
    )]);
    assert!(
        mpm_only
            .labels_for_paths(&["crates/trusty-mpm-gui/src/main.rs"])
            .is_empty(),
        "a member directory prefixes a path only at a directory boundary"
    );
}

#[test]
fn ownership_ignores_a_path_no_crate_owns() {
    assert!(
        ownership()
            .labels_for_paths(&["docs/specs/DOC-65.md", ".github/workflows/ci.yml"])
            .is_empty(),
        "a docs-or-CI-only diff earns no component label"
    );
}

#[test]
fn ownership_deduplicates_multi_file_crates() {
    let labels = ownership().labels_for_paths(&[
        "crates/trusty-mpm/src/a.rs",
        "crates/trusty-mpm/src/b.rs",
        "./crates/trusty-audit/src/c.rs",
        "README.md",
    ]);
    assert_eq!(
        labels,
        vec!["trusty-mpm".to_string(), "trusty-audit".to_string()],
        "one label per crate, in first-seen order, `./` prefix tolerated"
    );
}

#[test]
fn ownership_without_a_workspace_is_empty() {
    let tmp = TempDir::new().unwrap();
    assert!(
        CrateOwnership::resolve(Some(tmp.path()))
            .labels_for_paths(&["crates/trusty-mpm/src/lib.rs"])
            .is_empty()
    );
    assert!(
        CrateOwnership::resolve(None)
            .labels_for_paths(&["crates/trusty-mpm/src/lib.rs"])
            .is_empty()
    );
}
