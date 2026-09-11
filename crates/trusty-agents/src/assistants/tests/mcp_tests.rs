//! Tests for the assistant tier of MCP configuration (#7454).
//!
//! Why: the `[mcp]` table has the same two failure modes `[memory]` has — a
//! read that fails hard would break a chat turn over a typo, and a write that
//! re-serialises the whole struct would silently delete whatever the user
//! hand-added to their own `config.toml`.
//! What: the tolerant read (absent, malformed, well-formed), the override
//! conversion's disable-then-set ordering, and the comment-preserving write.
//! Test: this module IS the test surface.

use std::path::PathBuf;

use trusty_mcp::config::{McpServerConfig, McpServerOverride, McpTransport};

use crate::assistants::home::AssistantHome;
use crate::assistants::instance::AssistantInstanceId;
use crate::assistants::mcp::{McpOverrides, read_overrides, write_overrides};

fn home(tag: &str) -> (PathBuf, AssistantHome) {
    let root =
        std::env::temp_dir().join(format!("trusty-agents-amcp-{tag}-{}", uuid::Uuid::new_v4()));
    std::fs::create_dir_all(&root).unwrap();
    let home = AssistantHome::under(&root, AssistantInstanceId::new("izzie").unwrap());
    home.ensure().unwrap();
    (root, home)
}

fn server(name: &str, command: &str) -> McpServerConfig {
    McpServerConfig::new(
        name,
        McpTransport::Stdio {
            command: command.to_string(),
            args: Vec::new(),
            env: Default::default(),
        },
    )
}

#[test]
fn overrides_default_when_the_table_is_absent() {
    let (_root, home) = home("absent");
    let read = read_overrides(&home);
    assert!(read.overrides.is_empty());
    assert_eq!(read.error, None, "an absent table is not a fault");
    assert_eq!(read.path, home.config_path());
}

#[test]
fn reads_servers_and_disabled() {
    let (_root, home) = home("read");
    std::fs::write(
        home.config_path(),
        "id = \"izzie\"\n\n[mcp]\ndisabled = [\"github\"]\n\n[[mcp.servers]]\nname = \"extra\"\nenabled = true\n\n[mcp.servers.transport]\ntype = \"stdio\"\ncommand = \"extra-bin\"\n",
    )
    .unwrap();

    let read = read_overrides(&home);
    assert_eq!(read.error, None);
    assert_eq!(read.overrides.disabled, ["github"]);
    assert_eq!(read.overrides.servers.len(), 1);
    assert_eq!(read.overrides.servers[0].name, "extra");
}

/// The read never fails, but it must SAY the table is broken — a silent
/// fallback would show an assistant that quietly lost its overrides.
#[test]
fn a_malformed_table_reads_as_no_overrides_with_an_error() {
    let (_root, home) = home("malformed");
    std::fs::write(home.config_path(), "[mcp]\ndisabled = 7\n").unwrap();

    let read = read_overrides(&home);
    assert!(read.overrides.is_empty());
    assert!(read.error.is_some(), "a schema mismatch must be reported");
}

/// A `config.toml` that is not TOML at all is reported as that, not as a
/// schema mismatch — the two need different advice.
#[test]
fn a_file_that_is_not_toml_is_reported_as_such() {
    let (_root, home) = home("not-toml");
    std::fs::write(home.config_path(), "id = \n").unwrap();

    let read = read_overrides(&home);
    assert!(
        read.error
            .as_deref()
            .is_some_and(|e| e.contains("valid TOML")),
        "got: {:?}",
        read.error
    );
}

/// Disables are emitted first, so a name in both lists resolves as the `Set` —
/// the more specific instruction.
#[test]
fn a_set_after_a_disable_wins() {
    let overrides = McpOverrides {
        servers: vec![server("github", "override-bin")],
        disabled: vec!["github".to_string()],
    };
    let list = overrides.as_overrides();
    assert_eq!(list.len(), 2);
    assert!(matches!(list[0], McpServerOverride::Disable { .. }));
    assert!(matches!(list[1], McpServerOverride::Set(_)));
}

/// The file is the user's: a write replaces `[mcp]` and nothing else.
#[test]
fn writing_overrides_preserves_other_keys() {
    let (_root, home) = home("write-preserve");
    std::fs::write(
        home.config_path(),
        "# my notes\nid = \"izzie\"\ndisplay_name = \"Izzie\"\n\n[memory]\nfan_out = [\"cto-assistant\"]\n",
    )
    .unwrap();

    write_overrides(
        &home,
        &McpOverrides {
            servers: Vec::new(),
            disabled: vec!["github".to_string()],
        },
    )
    .unwrap();

    let raw = std::fs::read_to_string(home.config_path()).unwrap();
    assert!(raw.contains("# my notes"), "comments must survive: {raw}");
    assert!(
        raw.contains("display_name"),
        "unknown keys must survive: {raw}"
    );
    assert!(
        raw.contains("cto-assistant"),
        "[memory] must survive: {raw}"
    );
    assert_eq!(read_overrides(&home).overrides.disabled, ["github"]);
}

/// Clearing the list must be durable — writing nothing would leave the old
/// selection on disk and make the edit a no-op.
#[test]
fn writing_an_empty_disable_list_clears_it() {
    let (_root, home) = home("write-clear");
    write_overrides(
        &home,
        &McpOverrides {
            servers: Vec::new(),
            disabled: vec!["github".to_string()],
        },
    )
    .unwrap();
    assert_eq!(read_overrides(&home).overrides.disabled, ["github"]);

    write_overrides(&home, &McpOverrides::default()).unwrap();
    assert!(read_overrides(&home).overrides.disabled.is_empty());
}

/// A written server round-trips through TOML with its transport intact.
#[test]
fn a_written_server_round_trips() {
    let (_root, home) = home("write-server");
    write_overrides(
        &home,
        &McpOverrides {
            servers: vec![server("extra", "extra-bin")],
            disabled: Vec::new(),
        },
    )
    .unwrap();

    let read = read_overrides(&home);
    assert_eq!(read.error, None, "round trip must parse: {:?}", read.error);
    assert_eq!(read.overrides.servers.len(), 1);
    assert!(matches!(
        &read.overrides.servers[0].transport,
        McpTransport::Stdio { command, .. } if command == "extra-bin"
    ));
}

/// #4325's tolerant contract still holds: the whole home config parses with an
/// `[mcp]` table present, and an unknown key beside it is still ignored.
#[test]
fn the_home_config_still_tolerates_unknown_keys_beside_mcp() {
    let raw = "id = \"izzie\"\nsomething_new = 7\n\n[mcp]\ndisabled = [\"github\"]\n";
    let parsed: crate::assistants::AssistantHomeConfig = toml::from_str(raw).unwrap();
    assert_eq!(parsed.id, "izzie");
    assert_eq!(parsed.mcp.disabled, ["github"]);
}
