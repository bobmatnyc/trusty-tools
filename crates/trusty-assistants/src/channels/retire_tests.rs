//! The `agent.toml` `[[listeners]]` retirement (#7609 slice 7).
//!
//! Why: every test here fails against `origin/main`, where nothing removes that
//! table from an `agent.toml` at all. The one that matters most is the refusal:
//! a binding the migration could not carry into the channels file must KEEP its
//! legacy entry, because deleting it would destroy configuration with no
//! successor.
//! Test: this module IS the test.

use super::retire_agent_listeners;

/// A manifest with one legacy binding and a comment above it.
const MANIFEST: &str = "\
[agent]
name = \"fixture\"
role = \"assistant\"

# the mailbox this assistant reacts to
[[listeners]]
name = \"gmail-personal\"
enabled = true

[stores]
default = \"fixture-kb\"
";

fn seed(
    manifest: &str,
    channels: Option<&str>,
) -> (tempfile::TempDir, std::path::PathBuf, std::path::PathBuf) {
    let dir = tempfile::tempdir().expect("tempdir");
    let agent_toml = dir.path().join("fixture.toml");
    let channels_json = dir.path().join("fixture.channels.json");
    std::fs::write(&agent_toml, manifest).expect("manifest");
    if let Some(channels) = channels {
        std::fs::write(&channels_json, channels).expect("channels");
    }
    (dir, agent_toml, channels_json)
}

/// A binding the channels file covers loses its legacy table, keeps its comment.
#[test]
fn a_covered_legacy_table_is_removed_with_its_comment_kept() {
    let (_dir, agent_toml, channels_json) = seed(
        MANIFEST,
        Some(
            r#"[{"id":"gmail-personal","name":"gmail-personal","provider":"gworkspace","target":"label:INBOX","enabled":true}]"#,
        ),
    );
    assert!(
        retire_agent_listeners(&agent_toml, &channels_json),
        "a covered binding retires"
    );
    let raw = std::fs::read_to_string(&agent_toml).expect("read back");
    assert!(!raw.contains("[[listeners]]"), "the table is gone:\n{raw}");
    assert!(
        raw.contains("# the mailbox this assistant reacts to"),
        "the comment above it survives:\n{raw}"
    );
    assert!(
        raw.contains("name = \"fixture\"") && raw.contains("default = \"fixture-kb\""),
        "no other key is touched:\n{raw}"
    );
    raw.parse::<toml_edit::DocumentMut>()
        .expect("the manifest still parses");

    // Idempotent: a second sweep finds nothing to do.
    assert!(
        !retire_agent_listeners(&agent_toml, &channels_json),
        "the second pass removes nothing"
    );
}

/// A binding with no record in the channels file KEEPS its legacy entry.
///
/// Why: `migrate_agent_channels_if_absent` deliberately leaves an unstorable
/// binding in `agent.toml`. Retiring the table anyway would delete the only
/// copy of that configuration.
#[test]
fn an_uncovered_binding_keeps_the_legacy_table() {
    let (_dir, agent_toml, channels_json) = seed(MANIFEST, Some("[]"));
    assert!(
        !retire_agent_listeners(&agent_toml, &channels_json),
        "an uncovered binding blocks the retirement"
    );
    assert!(
        std::fs::read_to_string(&agent_toml)
            .expect("read back")
            .contains("[[listeners]]"),
        "the table is untouched"
    );

    // An ABSENT channels file covers nothing either.
    let (_dir, agent_toml, missing) = seed(MANIFEST, None);
    assert!(!retire_agent_listeners(&agent_toml, &missing));
    assert!(
        std::fs::read_to_string(&agent_toml)
            .expect("read back")
            .contains("[[listeners]]")
    );
}

/// A manifest with no legacy table, and one that will not parse, are both no-ops.
#[test]
fn nothing_to_retire_and_nothing_parseable_both_write_nothing() {
    let clean = "[agent]\nname = \"fixture\"\n";
    let (_dir, agent_toml, channels_json) = seed(clean, Some("[]"));
    assert!(!retire_agent_listeners(&agent_toml, &channels_json));
    assert_eq!(
        std::fs::read_to_string(&agent_toml).expect("read back"),
        clean
    );

    let broken = "[agent]\nname = \"fixture\"\n\n[[listeners]]\nname = 5\n";
    let (_dir, agent_toml, channels_json) = seed(broken, Some("[]"));
    assert!(
        !retire_agent_listeners(&agent_toml, &channels_json),
        "a malformed table is reported, never read as covered"
    );
    assert_eq!(
        std::fs::read_to_string(&agent_toml).expect("read back"),
        broken,
        "and the operator's file is untouched"
    );

    // An absent manifest is a no-op, not an error.
    let dir = tempfile::tempdir().expect("tempdir");
    assert!(!retire_agent_listeners(
        &dir.path().join("absent.toml"),
        &dir.path().join("absent.channels.json")
    ));
}
