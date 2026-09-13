//! Tests for the bounded nested-manifest walk (#7781).
//!
//! Why: every bound in `super` fails closed by returning "nothing found", which
//! is indistinguishable from a genuinely empty tree unless each one is pinned.
//! The anchor derivation carries the same risk in reverse: it silently stops
//! finding a stack when a manifest marker gains a shape it does not handle.
//! What: anchor derivation against the BUNDLED manifest, the depth and skip
//! rules, `.gitignore` parsing, determinism, and the shared member cap.
//! Test: this file.

use super::*;
use crate::core::manifest::framework::framework_agent_categories;
use std::fs;
use tempfile::TempDir;

fn write(dir: &Path, rel: &str, body: &str) {
    let path = dir.join(rel);
    fs::create_dir_all(path.parent().unwrap()).unwrap();
    fs::write(path, body).unwrap();
}

/// The bundled manifest's anchors — the set every detection call really uses.
fn bundled_anchors() -> MarkerAnchors {
    let categories = framework_agent_categories().expect("bundled manifest must be valid");
    MarkerAnchors::from_categories(&categories)
}

/// Nested roots relative to `root`, for readable assertions.
fn nested(root: &Path) -> Vec<String> {
    let budget = ProbeBudget::new();
    nested_probe_roots(root, &bundled_anchors(), &budget, &[root.to_path_buf()])
        .into_iter()
        .map(|p| {
            p.strip_prefix(root)
                .unwrap()
                .to_string_lossy()
                .replace('\\', "/")
        })
        .collect()
}

/// Every marker the bundled manifest declares yields an anchor that matches it.
///
/// Why: the walk finds a nested project only if the marker's file name is an
/// anchor. A marker shape this derivation mishandles would silently stop the
/// stack from ever being detected below the root — the failure mode #7781 is
/// closing, reintroduced one marker at a time.
/// What: for every declared marker, strips the content-probe tail, takes the
/// first path segment, and asserts [`MarkerAnchors::matches`] accepts it.
/// Test: this function IS the test.
#[test]
fn anchors_cover_every_bundled_marker() {
    let categories = framework_agent_categories().expect("bundled manifest must be valid");
    let anchors = MarkerAnchors::from_categories(&categories);
    let declared = categories
        .language
        .iter()
        .chain(categories.framework.iter())
        .chain(categories.platform.iter());
    for entry in declared {
        for marker in &entry.markers {
            let head = marker
                .split("::")
                .next()
                .unwrap()
                .split('/')
                .next()
                .unwrap()
                .to_string();
            assert!(
                anchors.matches(&head),
                "`{marker}` (declared by {}) must yield a matching anchor",
                entry.stem
            );
        }
    }
}

/// The `*.csproj` family anchors as a glob, not as a literal name.
#[test]
fn anchor_globs_match_dotnet() {
    let anchors = bundled_anchors();
    assert!(anchors.matches("MyApp.csproj"));
    assert!(anchors.matches("MyApp.sln"));
    assert!(
        !anchors.matches("MyApp.csproj.bak"),
        "the glob is suffix-anchored, as `marker_present` is"
    );
}

/// An undeclared nested UI package is found (the trusty-tools shape).
///
/// Why: the concrete case behind the owner's 2026-09-13 ruling — a Cargo
/// workspace whose Svelte app sits in `crates/<crate>/ui`, declared by no root
/// manifest key, so nothing but a walk can see it.
/// What: builds that layout and asserts the UI directory is returned.
/// Test: this function IS the test.
#[test]
fn nested_ui_package_is_found() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "Cargo.toml", "[workspace]\nmembers = []\n");
    write(tmp.path(), "crates/app/Cargo.toml", "[package]\n");
    write(tmp.path(), "crates/app/ui/package.json", "{}");

    assert_eq!(nested(tmp.path()), vec!["crates/app", "crates/app/ui"]);
}

/// The walk stops at `MAX_NESTED_DEPTH`.
#[test]
fn depth_bound_stops_the_walk() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "a/b/c/d/package.json", "{}");
    write(tmp.path(), "a/b/c/d/e/package.json", "{}");

    let found = nested(tmp.path());
    assert!(found.contains(&"a/b/c/d".to_string()), "depth 4 is walked");
    assert!(
        !found.contains(&"a/b/c/d/e".to_string()),
        "depth 5 is past the bound and must not be probed"
    );
}

/// Build output, dependency, and fixture trees are never descended into.
///
/// Why: a marker under any of them describes a third party or a test fixture,
/// not this project's stack. Measured on trusty-tools, `testdata/csharp` alone
/// put `dotnet-engineer` in a Rust workspace's roster (#7781).
/// What: seeds a `package.json` one level inside each skipped name and asserts
/// the walk returns nothing at all.
/// Test: this function IS the test.
#[test]
fn skipped_directories_are_never_probed() {
    let tmp = TempDir::new().unwrap();
    for skipped in [
        "node_modules",
        "target",
        "dist",
        "build",
        "vendor",
        "testdata",
        "test-data",
        "fixtures",
        ".git",
    ] {
        write(tmp.path(), &format!("{skipped}/pkg/package.json"), "{}");
    }
    assert!(
        nested(tmp.path()).is_empty(),
        "no skipped tree may contribute a probe root"
    );
}

/// A directory the root `.gitignore` names is skipped.
#[test]
fn gitignored_directory_is_skipped() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), ".gitignore", "generated/\n");
    write(tmp.path(), "generated/app/package.json", "{}");
    write(tmp.path(), "kept/package.json", "{}");

    assert_eq!(nested(tmp.path()), vec!["kept"]);
}

/// Only bare `.gitignore` names are honoured; path and glob rules are left to git.
#[test]
fn gitignore_path_rules_are_ignored() {
    let names = gitignore_dir_names(
        "# comment\n\n!kept\n**/target/\n/out\nbuild-cache/\nsrc/generated\n*.log\n",
    );
    assert_eq!(names, vec!["out".to_string(), "build-cache".to_string()]);
}

/// The walk returns a stable order regardless of `read_dir` order.
#[test]
fn walk_is_deterministic() {
    let tmp = TempDir::new().unwrap();
    for name in ["zeta", "alpha", "mid"] {
        write(tmp.path(), &format!("{name}/package.json"), "{}");
    }
    let first = nested(tmp.path());
    assert_eq!(first, vec!["alpha", "mid", "zeta"]);
    assert_eq!(nested(tmp.path()), first, "repeat runs agree");
}

/// An unreadable directory is absence, never an error.
#[test]
fn unreadable_directory_is_not_an_error() {
    let tmp = TempDir::new().unwrap();
    let missing = tmp.path().join("gone");
    let budget = ProbeBudget::new();
    assert!(nested_probe_roots(&missing, &bundled_anchors(), &budget, &[]).is_empty());
}

/// The member cap counts declared members and nested finds together.
///
/// Why: the two discovery paths share one ceiling, so a monorepo cannot exceed
/// it by splitting its projects across both.
/// What: passes an `already` list at the cap and asserts the walk adds nothing
/// even though an anchored directory is present.
/// Test: this function IS the test.
#[test]
fn member_cap_is_shared() {
    let tmp = TempDir::new().unwrap();
    write(tmp.path(), "pkg/package.json", "{}");
    let already: Vec<PathBuf> = (0..=MAX_WORKSPACE_MEMBERS)
        .map(|i| tmp.path().join(format!("filler{i}")))
        .collect();
    let budget = ProbeBudget::new();
    assert!(
        nested_probe_roots(tmp.path(), &bundled_anchors(), &budget, &already).is_empty(),
        "at the shared cap the walk contributes nothing"
    );
}
