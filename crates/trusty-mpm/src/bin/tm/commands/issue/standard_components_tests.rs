//! Tests for [`super`] — the component-label section of `tm issue standard`
//! (#7837).
//!
//! Every test builds a throwaway workspace in a `TempDir` and drives a
//! scripted `gh`, so none of them reads the checkout they run in or touches
//! the network.

use std::cell::RefCell;
use std::fs;
use std::path::Path;

use tempfile::TempDir;
use trusty_mpm::core::trusty_tools_config::ResolvedTicketing;

use super::render_component_labels;
use crate::commands::ticket::runner::{CommandOutput, CommandRunner};

/// A scripted `gh label list`.
///
/// Why: the section's only I/O is that one call; a fake that answers it keeps
/// the tests off the network.
/// What: `Some(json)` is returned as a successful run, `None` fails the call.
struct FakeGh(RefCell<Option<String>>);

impl CommandRunner for FakeGh {
    fn run(&self, _program: &str, _args: &[&str]) -> anyhow::Result<CommandOutput> {
        match self.0.borrow().as_ref() {
            Some(json) => Ok(CommandOutput {
                success: true,
                stdout: json.clone(),
                stderr: String::new(),
            }),
            None => anyhow::bail!("gh auth login required"),
        }
    }
}

/// A `gh` whose label list carries exactly `names`.
fn gh_with_labels(names: &[&str]) -> FakeGh {
    let rows: Vec<String> = names
        .iter()
        .map(|n| format!(r#"{{"name":"{n}","color":"BFD4F2","description":"Crate: {n}"}}"#))
        .collect();
    FakeGh(RefCell::new(Some(format!("[{}]", rows.join(",")))))
}

/// A `gh` that cannot answer at all.
fn broken_gh() -> FakeGh {
    FakeGh(RefCell::new(None))
}

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

/// A workspace with more crates than the seed table names — the shape #7837
/// reported, in miniature.
fn workspace() -> TempDir {
    let tmp = TempDir::new().unwrap();
    let root = tmp.path();
    fs::write(
        root.join("Cargo.toml"),
        "[workspace]\nmembers = [\"crates/*\"]\nresolver = \"2\"\n",
    )
    .unwrap();
    crate_at(root, "crates/trusty-common", "trusty-common");
    crate_at(root, "crates/trusty-git-analytics", "tga");
    crate_at(root, "crates/trusty-mpm", "trusty-mpm");
    crate_at(root, "crates/trusty-search", "trusty-search");
    tmp
}

/// The line naming `label`, for a test that asserts on one row.
fn line_for<'a>(text: &'a str, label: &str) -> &'a str {
    text.lines()
        .find(|l| l.trim_start().starts_with(&format!("{label}  ")))
        .unwrap_or_else(|| panic!("no line for `{label}` in:\n{text}"))
}

#[test]
fn component_labels_name_every_workspace_crate() {
    // #7837: the seeded set named two labels on a 29-crate workspace. Every
    // crate in the workspace is a component, whether the harness seeds it or
    // not.
    let tmp = workspace();
    let text = render_component_labels(
        &ResolvedTicketing::default(),
        Some("tm-dogfood"),
        Some(tmp.path()),
        &gh_with_labels(&["trusty-mpm", "trusty-common", "tga", "trusty-search"]),
    );
    for expected in ["trusty-common", "tga", "trusty-mpm", "trusty-search"] {
        assert!(text.contains(expected), "missing `{expected}`:\n{text}");
    }
    // 2 seeded (`trusty-mpm`, `ws/tm-dogfood`) + 3 crates `trusty-mpm` does
    // not already cover.
    assert!(text.contains("component labels (5)"), "{text}");
}

#[test]
fn a_label_the_repo_lacks_is_flagged_missing() {
    // #7837: `gh label list` is the cross-reference — a crate whose label
    // nobody created must not read as usable.
    let tmp = workspace();
    let text = render_component_labels(
        &ResolvedTicketing::default(),
        None,
        Some(tmp.path()),
        &gh_with_labels(&["trusty-mpm", "trusty-common"]),
    );
    assert!(
        line_for(&text, "trusty-search").contains("MISSING"),
        "{text}"
    );
    assert!(line_for(&text, "tga").contains("MISSING"), "{text}");
    assert!(
        !line_for(&text, "trusty-common").contains("MISSING"),
        "a label the repo carries is not flagged:\n{text}"
    );
}

#[test]
fn a_crate_absent_from_the_workspace_is_not_a_component() {
    // The live list widens nothing: a `Crate:`-described label the workspace
    // has no member for names no component.
    let tmp = workspace();
    let text = render_component_labels(
        &ResolvedTicketing::default(),
        None,
        Some(tmp.path()),
        &gh_with_labels(&["trusty-mpm", "ghost-crate"]),
    );
    assert!(!text.contains("ghost-crate"), "{text}");
}

#[test]
fn a_failed_crate_derivation_is_reported() {
    // Fail-open check: the seeded-only list is exactly the #7837 symptom, so
    // it may never be printed as if it were the workspace's components.
    let tmp = TempDir::new().unwrap();
    let text = render_component_labels(
        &ResolvedTicketing::default(),
        None,
        Some(tmp.path()),
        &gh_with_labels(&["trusty-mpm"]),
    );
    assert!(text.contains("crate labels: unavailable ("), "{text}");
    assert!(
        text.contains("no readable Cargo workspace manifest"),
        "{text}"
    );
    assert!(text.contains("SEEDED set only"), "{text}");
}

#[test]
fn a_failed_label_list_is_reported() {
    // Fail-open check: an unreadable label list is its own answer, never
    // "every label is missing" and never silence.
    let tmp = workspace();
    let text = render_component_labels(
        &ResolvedTicketing::default(),
        None,
        Some(tmp.path()),
        &broken_gh(),
    );
    assert!(text.contains("gh label list: unavailable ("), "{text}");
    assert!(text.contains("gh auth login required"), "{text}");
    assert!(text.contains("[presence unknown]"), "{text}");
    assert!(!text.contains("MISSING"), "{text}");
}
