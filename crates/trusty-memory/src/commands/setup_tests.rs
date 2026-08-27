//! Unit tests for `commands::setup`.
//!
//! Why: extracted from setup.rs to keep the production file under the 500-line
//! cap (line-cap gate, issue #610) while retaining full test coverage of the
//! patch_one / hook installation logic. All tests exercise the same items as
//! before via `use super::*`; as a child module this can still reach the
//! parent's private items.
//! What: tests for patch_one (creates file, idempotency, hook install,
//! session-start upgrade, legacy-shape upgrade, unrelated key preservation,
//! absolute-path hook commands).
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
    let entry = mcp_server_entry(MCP_SERVER_KEY, MCP_SERVER_ARGS);

    let outcome = patch_one(&path, &entry, None).expect("patch ok");
    assert!(outcome.mcp_wrote, "first patch writes the MCP entry");
    assert!(outcome.hook_wrote, "first patch installs the hook");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let server = &value["mcpServers"][MCP_SERVER_KEY];
    assert_eq!(server["command"], "trusty-memory");
    // #5265: the vector is exactly `["serve"]` — a client that execs the bare
    // binary never initializes MCP, and `--stdio` no longer selects anything.
    assert_eq!(
        server["args"].as_array().map(Vec::len),
        Some(1),
        "expected exactly one argument, got {}",
        server["args"]
    );
    assert_eq!(server["args"][0], "serve");

    let hook_entries = value["hooks"][HOOK_EVENT].as_array().unwrap();
    assert_eq!(hook_entries.len(), 1, "exactly one matcher block");
    let inner = hook_entries[0]["hooks"].as_array().unwrap();
    // With exe=None the command falls back to the bare name.
    assert_eq!(inner[0]["command"], HOOK_COMMAND_BARE);
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
    assert_eq!(ss_inner[0]["command"], INBOX_CHECK_HOOK_COMMAND_BARE);
    assert_eq!(ss_inner[0]["type"], "command");
    assert_eq!(ss_inner[0]["timeout"], HOOK_TIMEOUT_MS);
}

/// Why: hooks must embed an absolute binary path so they fire even in
/// environments where ~/.cargo/bin is not on PATH (closes the claude-mpm
/// class of bug where a global hook broke a SAM `make deploy`).
/// What: passes an absolute exe path to `patch_one`, asserts the written
/// hook commands start with '/' and end with the correct sub-command.
#[test]
fn patch_one_installs_hook_with_absolute_path() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let entry = mcp_server_entry(MCP_SERVER_KEY, MCP_SERVER_ARGS);
    let fake_exe = std::path::PathBuf::from("/usr/local/bin/trusty-memory");

    let outcome = patch_one(&path, &entry, Some(&fake_exe)).expect("patch ok");
    assert!(outcome.mcp_wrote);
    assert!(outcome.hook_wrote);

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();

    let prompt_cmd = value["hooks"][HOOK_EVENT][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        prompt_cmd.starts_with('/'),
        "prompt-context command must be absolute, got: {prompt_cmd:?}"
    );
    assert!(
        prompt_cmd.ends_with(" prompt-context"),
        "prompt-context command must end with ' prompt-context', got: {prompt_cmd:?}"
    );

    let inbox_cmd = value["hooks"][SESSION_START_HOOK_EVENT][0]["hooks"][0]["command"]
        .as_str()
        .unwrap();
    assert!(
        inbox_cmd.starts_with('/'),
        "inbox-check command must be absolute, got: {inbox_cmd:?}"
    );
    assert!(
        inbox_cmd.ends_with(" inbox-check"),
        "inbox-check command must end with ' inbox-check', got: {inbox_cmd:?}"
    );
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
    let entry = mcp_server_entry(MCP_SERVER_KEY, MCP_SERVER_ARGS);
    let seed = json!({
        "mcpServers": {
            MCP_SERVER_KEY: { "command": "trusty-memory", "args": ["serve"] }
        },
        "hooks": {
            HOOK_EVENT: [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": HOOK_COMMAND_BARE,
                    "timeout": HOOK_TIMEOUT_MS,
                }]
            }]
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

    let outcome = patch_one(&path, &entry, None).expect("patch ok");
    assert!(!outcome.mcp_wrote);
    assert!(outcome.hook_wrote, "SessionStart hook must be added");

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    // Existing UserPromptSubmit is preserved exactly.
    let ups = value["hooks"][HOOK_EVENT].as_array().unwrap();
    assert_eq!(ups.len(), 1);
    assert_eq!(ups[0]["hooks"][0]["command"], HOOK_COMMAND_BARE);
    // New SessionStart is present.
    let ss = value["hooks"][SESSION_START_HOOK_EVENT].as_array().unwrap();
    assert_eq!(ss.len(), 1);
    assert_eq!(ss[0]["hooks"][0]["command"], INBOX_CHECK_HOOK_COMMAND_BARE);
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
    let entry = mcp_server_entry(MCP_SERVER_KEY, MCP_SERVER_ARGS);
    // Legacy shape: UserPromptSubmit entry without a `timeout` field,
    // which an older trusty-memory release may have written.
    let seed = json!({
        "mcpServers": {
            MCP_SERVER_KEY: { "command": "trusty-memory", "args": ["serve"] }
        },
        "hooks": {
            HOOK_EVENT: [{
                "matcher": "*",
                "hooks": [{
                    "type": "command",
                    "command": HOOK_COMMAND_BARE,
                }]
            }]
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

    let outcome = patch_one(&path, &entry, None).expect("patch ok");
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
    assert_eq!(ss[0]["hooks"][0]["command"], INBOX_CHECK_HOOK_COMMAND_BARE);
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
    let entry = mcp_server_entry(MCP_SERVER_KEY, MCP_SERVER_ARGS);

    let first = patch_one(&path, &entry, None).unwrap();
    assert!(first.mcp_wrote && first.hook_wrote, "first patch writes");
    let after_first = std::fs::read_to_string(&path).unwrap();

    let second = patch_one(&path, &entry, None).unwrap();
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

    let entry = mcp_server_entry(MCP_SERVER_KEY, MCP_SERVER_ARGS);
    let outcome = patch_one(&path, &entry, None).expect("patch ok");
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
    assert_eq!(ups[0]["hooks"][0]["command"], HOOK_COMMAND_BARE);
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
    let entry = mcp_server_entry(MCP_SERVER_KEY, MCP_SERVER_ARGS);
    let seed = json!({
        "mcpServers": {
            MCP_SERVER_KEY: { "command": "trusty-memory", "args": ["serve"] }
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

    let outcome = patch_one(&path, &entry, None).expect("patch ok");
    assert!(!outcome.mcp_wrote, "MCP entry already present");
    assert!(outcome.hook_wrote, "hook freshly installed");
}

/// Why (#5265): the released contract is bare `serve`, so an existing install
/// whose Claude settings file still holds `args: ["serve", "--stdio"]` must be
/// rewritten down to `args: ["serve"]` when `setup` is re-run. Leaving the
/// legacy vector in place keeps a retired transport-selection flag on the
/// command line of every future session.
/// What: seeds a settings file with the legacy vector, calls `patch_one` with
/// the canonical entry, and asserts the file was rewritten to exactly
/// `["serve"]`.
#[test]
fn patch_one_repairs_a_legacy_serve_stdio_entry() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join("settings.json");
    let seed = json!({
        "mcpServers": {
            MCP_SERVER_KEY: { "command": "trusty-memory", "args": ["serve", "--stdio"] }
        }
    });
    std::fs::write(&path, serde_json::to_string_pretty(&seed).unwrap()).unwrap();

    let entry = mcp_server_entry(MCP_SERVER_KEY, MCP_SERVER_ARGS);
    let outcome = patch_one(&path, &entry, None).expect("patch ok");
    assert!(
        outcome.mcp_wrote,
        "legacy args:[\"serve\",\"--stdio\"] must be rewritten to args:[\"serve\"]"
    );

    let value: serde_json::Value =
        serde_json::from_str(&std::fs::read_to_string(&path).unwrap()).unwrap();
    let args: Vec<&str> = value["mcpServers"][MCP_SERVER_KEY]["args"]
        .as_array()
        .expect("args array present after repair")
        .iter()
        .filter_map(serde_json::Value::as_str)
        .collect();
    assert_eq!(args, vec!["serve"], "got {args:?}");
}

/// Why (#5265): the whole defect is a Codex registration that never reaches MCP
/// mode, so the one thing this must prove is that `serve` arrives in the written
/// TOML as its own array element.
/// What: runs `setup_codex` against a tempdir `$HOME` and reads the vector back.
#[test]
fn setup_codex_writes_the_serve_entrypoint() {
    let tmp = tempfile::tempdir().expect("tempdir");
    setup_codex(tmp.path()).expect("codex setup ok");

    let text =
        std::fs::read_to_string(tmp.path().join(".codex").join("config.toml")).expect("read");
    let cfg: toml::Value = text.parse().expect("valid TOML");
    let entry = &cfg["mcp_servers"][MCP_SERVER_KEY];
    assert_eq!(entry["command"].as_str(), Some("trusty-memory"));
    let args: Vec<&str> = entry["args"]
        .as_array()
        .expect("args array")
        .iter()
        .filter_map(toml::Value::as_str)
        .collect();
    assert_eq!(
        args,
        vec!["serve"],
        "#5265: Codex must exec `trusty-memory serve`, not the bare binary"
    );
}

/// Why (#5265): re-running setup must REPAIR the two shapes operators already
/// have on disk — the legacy `serve --stdio` vector, and a whole vector
/// serialized as one JSON-looking string, which launches
/// `trusty-memory '["serve"]'` and never initializes.
/// What: seeds each shape, runs `setup_codex`, asserts the vector is `["serve"]`.
#[test]
fn setup_codex_repairs_a_legacy_stdio_registration() {
    for seeded in [
        "args = [\"serve\", \"--stdio\"]",
        "args = [\"[\\\"serve\\\"]\"]",
        "args = []",
    ] {
        let tmp = tempfile::tempdir().expect("tempdir");
        let path = tmp.path().join(".codex").join("config.toml");
        std::fs::create_dir_all(path.parent().unwrap()).unwrap();
        std::fs::write(
            &path,
            format!("[mcp_servers.trusty-memory]\ncommand = \"trusty-memory\"\n{seeded}\n"),
        )
        .unwrap();

        setup_codex(tmp.path()).expect("codex setup ok");

        let cfg: toml::Value = std::fs::read_to_string(&path).unwrap().parse().unwrap();
        let args: Vec<&str> = cfg["mcp_servers"][MCP_SERVER_KEY]["args"]
            .as_array()
            .expect("args array")
            .iter()
            .filter_map(toml::Value::as_str)
            .collect();
        assert_eq!(args, vec!["serve"], "seeded {seeded}, got {args:?}");
    }
}

/// Why: `setup` is re-run routinely; a second run must leave the operator's
/// Codex config byte-identical.
#[test]
fn setup_codex_is_idempotent() {
    let tmp = tempfile::tempdir().expect("tempdir");
    let path = tmp.path().join(".codex").join("config.toml");
    setup_codex(tmp.path()).expect("first run");
    let before = std::fs::read_to_string(&path).expect("read");
    setup_codex(tmp.path()).expect("second run");
    assert_eq!(std::fs::read_to_string(&path).unwrap(), before);
}

/// Why (#6307): the GUI-client path must reject a client it does not know
/// rather than printing a registration aimed at the wrong app.
/// What: calls `handle_setup_gui_client` with an unsupported name and asserts
/// the error names the supported list.
#[test]
fn handle_setup_gui_client_rejects_an_unknown_client() {
    let err = handle_setup_gui_client("cursor").expect_err("unknown client must be rejected");
    assert!(err.to_string().contains("chatgpt"), "{err}");
}

/// Why (#6307): the registration this crate writes for Claude Code is
/// `{"command": "trusty-memory"}` — a bare name that exits 127 under launchd's
/// `PATH`, which is the whole defect. The GUI registration must not be that
/// shape.
/// What: builds the GUI entry from the same server key and argument vector and
/// asserts its command is an absolute path that is not the bare binary name,
/// and that its working directory exists.
#[test]
fn gui_entry_is_absolute_where_the_claude_entry_is_bare() {
    let bare = mcp_server_entry(MCP_SERVER_KEY, MCP_SERVER_ARGS);
    assert_eq!(
        bare["command"], MCP_SERVER_KEY,
        "the Claude Code entry is the bare name"
    );

    let command = trusty_common::gui_mcp_client::running_binary_path().expect("exe resolves");
    let working_dir = trusty_common::gui_mcp_client::default_working_dir().expect("home resolves");
    let entry = trusty_common::gui_mcp_client::build_entry(
        MCP_SERVER_KEY,
        MCP_SERVER_ARGS,
        &command,
        &working_dir,
    )
    .expect("gui entry builds");
    let rendered = entry.to_json();

    let cmd = rendered["command"].as_str().expect("command is a string");
    assert!(
        Path::new(cmd).is_absolute(),
        "command must be absolute, got {cmd}"
    );
    assert_ne!(
        cmd, MCP_SERVER_KEY,
        "the bare name is what fails under launchd"
    );
    let cwd = rendered["cwd"].as_str().expect("cwd is a string");
    assert!(Path::new(cwd).is_dir(), "cwd must exist, got {cwd}");
}
