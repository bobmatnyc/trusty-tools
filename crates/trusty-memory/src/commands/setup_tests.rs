//! Unit tests for `commands::setup`.
//!
//! Why: extracted from setup.rs to keep the production file under the 500-line
//! cap while retaining full test coverage of the patch_one / hook installation
//! logic. All tests exercise the same items as before via `use super::*`.
//! What: tests for patch_one (creates file, idempotency, hook install,
//! session-start upgrade, legacy-shape upgrade, unrelated key preservation).
//! Test: this file is the test suite; run with `cargo test -p trusty-memory`.

use super::*;
use serde_json::json;

/// Why: patching a fresh settings file must produce a valid
/// `mcpServers` block with the canonical `trusty-memory` entry AND a
/// `UserPromptSubmit` hook pointing at `trusty-memory prompt-context`.
/// What: writes a minimal settings.json, calls `patch_one`, asserts
/// both edits landed.
#[test]
fn patch_one_creates_missing_file() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve", "--stdio"]);

    let outcome = patch_one(&path, &entry).expect("patch ok");
    assert!(outcome.mcp_wrote, "first patch writes the MCP entry");
    assert!(outcome.hook_wrote, "first patch installs the hook");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let server = &value["mcpServers"][MCP_SERVER_KEY];
    assert_eq!(server["command"], "trusty-memory");
    assert_eq!(server["args"][0], "serve");
    assert_eq!(server["args"][1], "--stdio");

    let hook_entries = value["hooks"][HOOK_EVENT].as_array().unwrap();
    assert_eq!(hook_entries.len(), 1, "exactly one matcher block");
    let inner = hook_entries[0]["hooks"].as_array().unwrap();
    assert_eq!(inner[0]["command"], HOOK_COMMAND);
    assert_eq!(inner[0]["type"], "command");
    assert_eq!(inner[0]["timeout"], HOOK_TIMEOUT_MS);

    // Issue #99: SessionStart hook must also land.
    let ss_entries = value["hooks"][SESSION_START_HOOK_EVENT]
        .as_array()
        .expect("SessionStart hooks installed");
    assert_eq!(
        ss_entries.len(),
        1,
        "exactly one SessionStart matcher block"
    );
    let ss_inner = ss_entries[0]["hooks"].as_array().unwrap();
    assert_eq!(ss_inner[0]["command"], INBOX_CHECK_HOOK_COMMAND);
    assert_eq!(ss_inner[0]["type"], "command");
    assert_eq!(ss_inner[0]["timeout"], HOOK_TIMEOUT_MS);
}

/// Why: regression for issue #99 — when a user has the UserPromptSubmit
/// hook from an earlier release but no SessionStart hook, re-running
/// setup must add the SessionStart block (and not touch any existing
/// UserPromptSubmit hook).
/// What: seeds a settings file with the MCP entry + UserPromptSubmit
/// hook only, patches, and asserts both events are present afterwards.
#[test]
fn patch_one_installs_session_start_hook_when_upgrading() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve", "--stdio"]);
    let seed = json!({
        "mcpServers": {
            MCP_SERVER_KEY: { "command": "trusty-memory", "args": ["serve", "--stdio"] }
        },
        "hooks": {
            HOOK_EVENT: [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": HOOK_COMMAND,
                    "timeout": HOOK_TIMEOUT_MS,
                }]
            }]
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

    let outcome = patch_one(&path, &entry).expect("patch ok");
    assert!(!outcome.mcp_wrote);
    assert!(outcome.hook_wrote, "SessionStart hook must be added");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // Existing UserPromptSubmit is preserved exactly.
    let ups = value["hooks"][HOOK_EVENT].as_array().unwrap();
    assert_eq!(ups.len(), 1);
    assert_eq!(ups[0]["hooks"][0]["command"], HOOK_COMMAND);
    // New SessionStart is present.
    let ss = value["hooks"][SESSION_START_HOOK_EVENT].as_array().unwrap();
    assert_eq!(ss.len(), 1);
    assert_eq!(ss[0]["hooks"][0]["command"], INBOX_CHECK_HOOK_COMMAND);
}

/// Why (issue #126): regression for the realistic upgrade scenario the
/// user reported — an older trusty-memory installed an UserPromptSubmit
/// hook with a *different* timeout (or omitted the field entirely), and
/// the new binary still has to add the SessionStart entry. The earlier
/// `patch_one_installs_session_start_hook_when_upgrading` test seeded
/// the file with a UserPromptSubmit entry whose shape matched the new
/// additions exactly, so it never exercised the case where the old
/// hook's shape differs from the new one.
/// What: seed a settings file with a UserPromptSubmit entry that has
/// (a) no timeout field, and (b) only the `command` + `type` keys —
/// representative of older trusty-memory installs. Patch it. Assert
/// that SessionStart is installed with the canonical `inbox-check`
/// command and timeout, and that UserPromptSubmit retains the legacy
/// entry alongside an appended canonical-shape one.
/// Test: itself.
#[test]
fn patch_one_adds_session_start_when_legacy_user_prompt_submit_has_different_shape() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve", "--stdio"]);
    // Legacy shape: UserPromptSubmit entry without a `timeout` field,
    // which an older trusty-memory release may have written.
    let seed = json!({
        "mcpServers": {
            MCP_SERVER_KEY: { "command": "trusty-memory", "args": ["serve", "--stdio"] }
        },
        "hooks": {
            HOOK_EVENT: [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": HOOK_COMMAND,
                }]
            }]
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

    let outcome = patch_one(&path, &entry).expect("patch ok");
    assert!(!outcome.mcp_wrote, "MCP entry already canonical");
    assert!(
        outcome.hook_wrote,
        "SessionStart (and a fresh UserPromptSubmit shape) must be added"
    );

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    // SessionStart must be installed with the canonical inbox-check command.
    let ss = value["hooks"][SESSION_START_HOOK_EVENT]
        .as_array()
        .expect("SessionStart array exists");
    assert_eq!(ss.len(), 1, "exactly one SessionStart matcher block");
    assert_eq!(ss[0]["hooks"][0]["command"], INBOX_CHECK_HOOK_COMMAND);
    assert_eq!(ss[0]["hooks"][0]["timeout"], HOOK_TIMEOUT_MS);

    // UserPromptSubmit: the legacy entry is preserved AND the canonical
    // shape is appended (deep-equality dedup considers them distinct).
    let ups = value["hooks"][HOOK_EVENT].as_array().unwrap();
    assert!(
        ups.iter().any(|e| e["hooks"][0].get("timeout").is_none()),
        "legacy timeout-less entry must be preserved"
    );
    assert!(
        ups.iter()
            .any(|e| e["hooks"][0]["timeout"] == HOOK_TIMEOUT_MS),
        "canonical timeout=3000 entry must be appended"
    );
}

/// Why: re-running `setup` must be safe — calling `patch_one` against
/// an already-configured file must not rewrite it.
/// What: writes settings.json, patches twice, asserts the second call
/// reports neither change and the file is byte-identical.
#[test]
fn patch_one_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve", "--stdio"]);

    let first = patch_one(&path, &entry).unwrap();
    assert!(first.mcp_wrote && first.hook_wrote, "first patch writes");
    let after_first = std::fs::read_to_string(&path).unwrap();

    let second = patch_one(&path, &entry).unwrap();
    assert!(
        !second.mcp_wrote && !second.hook_wrote,
        "second patch is no-op"
    );
    let after_second = std::fs::read_to_string(&path).unwrap();

    assert_eq!(after_first, after_second, "file must not change on no-op");
}

/// Why: patching must preserve unrelated keys (theme, other servers,
/// other hooks). Anything else is a regression — `setup` would destroy
/// user config.
/// What: seeds a settings file with extra keys, patches, asserts every
/// pre-existing key still exists alongside the new MCP entry and that
/// pre-existing hooks under other events are left in place.
#[test]
fn patch_one_preserves_unrelated_keys() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let seed = json!({
        "theme": "dark",
        "mcpServers": {
            "some-other-server": { "command": "x", "args": [] }
        },
        "hooks": {
            "Stop": [{ "matcher": "*", "hooks": [
                { "type": "command", "command": "echo bye" }
            ] }]
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

    let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve", "--stdio"]);
    let outcome = patch_one(&path, &entry).expect("patch ok");
    assert!(outcome.mcp_wrote);
    assert!(outcome.hook_wrote);

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    assert_eq!(value["theme"], "dark", "unrelated top-level key dropped");
    let servers = value["mcpServers"].as_object().unwrap();
    assert!(servers.contains_key("some-other-server"));
    assert!(servers.contains_key(MCP_SERVER_KEY));
    // Pre-existing Stop hook must be retained.
    let stop = value["hooks"]["Stop"].as_array().unwrap();
    assert_eq!(stop.len(), 1);
    assert_eq!(stop[0]["hooks"][0]["command"], "echo bye");
    // And our UserPromptSubmit hook was added.
    let ups = value["hooks"][HOOK_EVENT].as_array().unwrap();
    assert_eq!(ups[0]["hooks"][0]["command"], HOOK_COMMAND);
}

/// Why: when the MCP entry is already present but the hook is new (a
/// user upgrading from an older trusty-memory release), the patch must
/// install only the hook and report that distinction.
/// What: seeds a settings file with the MCP entry already present but
/// no hook, runs `patch_one`, asserts `mcp_wrote = false, hook_wrote
/// = true`.
#[test]
fn patch_one_installs_hook_when_mcp_already_present() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let entry = mcp_server_entry(MCP_SERVER_KEY, &["serve", "--stdio"]);
    let seed = json!({
        "mcpServers": {
            MCP_SERVER_KEY: { "command": "trusty-memory", "args": ["serve", "--stdio"] }
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

    let outcome = patch_one(&path, &entry).expect("patch ok");
    assert!(!outcome.mcp_wrote, "MCP entry already present");
    assert!(outcome.hook_wrote, "hook freshly installed");
}
