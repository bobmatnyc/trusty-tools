//! Which configs count as Assistant INSTANCES (#4325).
//!
//! Why: startup provisioning acts on this list, so a wrong answer creates
//! directories for agents that should not have them — `ctrl` above all, which
//! declares `role = "assistant"` but is the concierge, not a selectable
//! instance.
//! What: the shipped lineage (base + `extends`), the exclusions, and the
//! forgiving-parse behaviour.
//! Test: this module IS the test surface.

use std::path::{Path, PathBuf};

use crate::assistants::discover_instances;

/// Write a flat `<name>.toml` agent config.
fn flat(dir: &Path, name: &str, body: &str) {
    std::fs::create_dir_all(dir).unwrap();
    std::fs::write(dir.join(format!("{name}.toml")), body).unwrap();
}

/// Write a package `<name>/agent.toml` agent config.
fn package(dir: &Path, name: &str, body: &str) {
    let pkg = dir.join(name);
    std::fs::create_dir_all(&pkg).unwrap();
    std::fs::write(pkg.join("agent.toml"), body).unwrap();
}

fn ids(dirs: &[PathBuf]) -> Vec<String> {
    discover_instances(dirs)
        .into_iter()
        .map(|id| id.as_str().to_string())
        .collect()
}

/// Why: this mirrors the real shipped roster — the base plus the two personas
/// that extend it, which are exactly milestone 22 pillar (b)'s instances.
#[test]
fn finds_the_shipped_instances() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    package(
        &dir,
        "assistant",
        "[agent]\nname = \"assistant\"\nrole = \"assistant\"\n",
    );
    package(
        &dir,
        "izzie",
        "[agent]\nname = \"izzie\"\nrole = \"assistant\"\nextends = \"assistant\"\n",
    );
    package(
        &dir,
        "cto-assistant",
        "[agent]\nname = \"cto-assistant\"\nrole = \"assistant\"\nextends = \"assistant\"\n",
    );

    assert_eq!(ids(&[dir]), vec!["assistant", "cto-assistant", "izzie"]);
}

/// Why: THE reason this module exists. `ctrl` declares `role = "assistant"`
/// but neither is nor extends the base — the GUI's null selection MEANS ctrl,
/// so provisioning it a selectable instance home would model the one agent
/// that is not an instance as if it were. Selecting on role alone would have
/// silently included it.
#[test]
fn excludes_ctrl_and_non_assistants() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    package(
        &dir,
        "assistant",
        "[agent]\nname = \"assistant\"\nrole = \"assistant\"\n",
    );
    // Declares the role, descends from nothing.
    package(
        &dir,
        "ctrl",
        "[agent]\nname = \"ctrl\"\nrole = \"assistant\"\n",
    );
    flat(
        &dir,
        "engineer",
        "[agent]\nname = \"engineer\"\nrole = \"engineer\"\n",
    );
    // Extends something else entirely.
    flat(
        &dir,
        "gpt-engineer",
        "[agent]\nname = \"gpt-engineer\"\nrole = \"engineer\"\nextends = \"engineer\"\n",
    );

    assert_eq!(ids(&[dir]), vec!["assistant"]);
}

/// Why: a malformed or oddly-named file in the agents directory is a normal
/// state on a user's machine. It must be skipped, never fatal — startup
/// provisioning cannot care.
#[test]
fn skips_unparseable_and_unusable_entries() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    package(
        &dir,
        "izzie",
        "[agent]\nname = \"izzie\"\nrole = \"assistant\"\nextends = \"assistant\"\n",
    );
    flat(&dir, "broken", "this is not [[[ toml");
    // A name that could never be a directory entry of its own.
    flat(
        &dir,
        "Upper",
        "[agent]\nname = \"Upper\"\nrole = \"assistant\"\nextends = \"assistant\"\n",
    );
    // Not a toml file at all, and a directory with no agent.toml.
    std::fs::write(dir.join("notes.md"), "hi").unwrap();
    std::fs::create_dir_all(dir.join("empty-pkg")).unwrap();

    assert_eq!(ids(&[dir]), vec!["izzie"]);
}

#[test]
fn the_base_assistant_is_itself_an_instance() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = tmp.path().to_path_buf();
    flat(
        &dir,
        "assistant",
        "[agent]\nname = \"assistant\"\nrole = \"assistant\"\n",
    );
    assert_eq!(ids(&[dir]), vec!["assistant"]);
}

/// Why: `agents_dir_candidates()` returns several tiers and the same agent can
/// appear in more than one. One instance means one home, so the list is
/// deduplicated.
#[test]
fn deduplicates_across_directory_tiers() {
    let tmp = tempfile::tempdir().unwrap();
    let a = tmp.path().join("a");
    let b = tmp.path().join("b");
    let body = "[agent]\nname = \"izzie\"\nrole = \"assistant\"\nextends = \"assistant\"\n";
    package(&a, "izzie", body);
    flat(&b, "izzie", body);

    assert_eq!(ids(&[a, b]), vec!["izzie"]);
}

/// Why: a missing agents directory is normal on a fresh machine.
#[test]
fn a_missing_directory_is_not_an_error() {
    let tmp = tempfile::tempdir().unwrap();
    assert!(ids(&[tmp.path().join("does-not-exist")]).is_empty());
    assert!(ids(&[]).is_empty());
}
