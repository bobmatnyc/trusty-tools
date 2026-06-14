//! Output-style, spinner-tip, hook, and MCP-injection helpers.
//!
//! Why: `prepare_session` must configure several Claude Code settings files
//! before launching a session. Grouping all the settings-file mutations here
//! keeps `mod.rs` focused on the top-level preparation sequence and makes
//! each individual helper easy to test in isolation.
//! What: constants and functions for writing the output style, spinner tips,
//! `trusty-memory` project hooks, `trusty-memory` MCP server injection, and
//! global-hook cleanup.
//! Test: each public(super) function is covered by a dedicated test in `tests.rs`.

use std::path::{Path, PathBuf};

use super::PrepError;

/// Claude Code output style applied to launched sessions.
///
/// Why: the Claude Code status bar renders `style:<outputStyle>`; launched
/// trusty-mpm sessions should advertise themselves as `trusty-mpm`.
pub(super) const OUTPUT_STYLE: &str = "trusty-mpm";

/// trusty-mpm-specific spinner tips shown during Claude Code loading.
///
/// Why: Claude Code's loading spinner renders tips from
/// `spinnerTipsOverride.tips`; the operator's global settings carry generic
/// claude-mpm tips, so trusty-mpm sessions override them with project-relevant
/// guidance (the `tm` CLI, the `make check` gate, the API-first layering rule).
pub(super) const SPINNER_TIPS: &[&str] = &[
    "tm launch — start a configured claude session for this project",
    "make check = cargo test + clippy + fmt — must pass before any PR",
    "API → CLI → TUI: implement at the lowest layer first",
    "Delegate Rust code to rust-engineer — PM never edits .rs files",
    "gh issue create to track work; commits include Closes #N",
    "tmux ls shows all active tmpm-<folder> sessions",
    "tm session list shows daemon-managed sessions",
    "/compact at ~50% context to stay focused",
    "Layer new features behind the HTTP API before wiring CLI or TUI",
];

/// The `trusty-memory` hook block written into the project's
/// `.claude/settings.json`.
///
/// Why: these hooks must fire only for trusty-mpm-managed sessions, not for
/// every Claude Code instance on the machine. Writing them at the project
/// level (rather than `~/.claude/settings.json`) scopes them to projects
/// prepared by trusty-mpm. The block covers the `PostToolUse`, `Stop`, and
/// `UserPromptSubmit` events — the `trusty-memory` binary does not implement a
/// `claude.pre-tool-use` handler, so a `PreToolUse` hook would only error on
/// every tool call. These three events capture the session lifecycle for
/// memory.
pub(super) const TRUSTY_MEMORY_HOOKS: &str = r#"{
  "PostToolUse": [
    {
      "matcher": "Write|Edit|Bash",
      "hooks": [
        {
          "type": "command",
          "command": "trusty-memory hooks fire claude.post-tool-use",
          "timeout": 60
        }
      ]
    }
  ],
  "Stop": [
    {
      "matcher": "",
      "hooks": [
        {
          "type": "command",
          "command": "trusty-memory hooks fire claude.stop",
          "timeout": 60
        }
      ]
    }
  ],
  "UserPromptSubmit": [
    {
      "matcher": "",
      "hooks": [
        {
          "type": "command",
          "command": "trusty-memory hooks fire claude.user-prompt",
          "timeout": 60
        }
      ]
    }
  ]
}"#;

/// The `trusty-memory` MCP server definition injected into a project's
/// `.mcp.json`.
///
/// Why: Claude Code reads MCP servers from `<project>/.mcp.json`; a launched
/// trusty-mpm session needs the `trusty-memory` server registered there so the
/// memory tools (`memory_recall`, `memory_store`, …) are available.
pub(super) const TRUSTY_MEMORY_MCP_SERVER: &str = r#"{
  "type": "stdio",
  "command": "trusty-memory",
  "args": ["mcp", "serve"]
}"#;

/// Hook event types the global `trusty-memory` entries were registered under.
pub(super) const GLOBAL_TRUSTY_MEMORY_EVENTS: &[&str] =
    &["PostToolUse", "Stop", "UserPromptSubmit"];

/// Deploy the bundled `trusty-mpm` output style under `<home>/.claude/output-styles/`.
///
/// Why: [`write_output_style`] only sets `"outputStyle": "trusty-mpm"` in the
/// project settings; Claude Code honours that name only when a matching style
/// file exists in `~/.claude/output-styles/`. This places that file. `home` is
/// passed in (rather than resolved here) so tests can target a temp directory
/// instead of the operator's real home.
/// What: creates `<home>/.claude/output-styles/` if absent, then writes the
/// bundled [`crate::core::bundle::OUTPUT_STYLE`] asset, always overwriting so
/// framework upgrades to the style propagate on the next launch. Returns the
/// path written.
/// Test: `deploy_output_style_writes_file`, `deploy_output_style_overwrites`.
pub(super) fn deploy_output_style(home: &Path) -> Result<PathBuf, PrepError> {
    let style_dir = home.join(".claude").join("output-styles");
    std::fs::create_dir_all(&style_dir).map_err(|source| PrepError::Io {
        path: style_dir.clone(),
        source,
    })?;
    let style_path = style_dir.join("trusty-mpm.md");
    std::fs::write(&style_path, crate::core::bundle::OUTPUT_STYLE).map_err(|source| {
        PrepError::Io {
            path: style_path.clone(),
            source,
        }
    })?;
    Ok(style_path)
}

/// Merge trusty-mpm output-style and spinner-tip settings into the project's
/// `.claude/settings.json`.
///
/// Why: Claude Code reads the output style from `.claude/settings.json` under
/// the `outputStyle` key (there is no `--style` CLI flag); writing it in the
/// project directory makes every `claude` launched there show
/// `style:trusty-mpm` without disturbing the operator's global settings. The
/// same file drives the loading-spinner tips, so trusty-mpm-specific tips are
/// written alongside to override the operator's generic claude-mpm tips.
/// What: reads an existing `<project>/.claude/settings.json` (preserving all
/// other keys), sets `outputStyle` to [`OUTPUT_STYLE`], enables
/// `spinnerTipsEnabled`, sets `spinnerTipsOverride.tips` to [`SPINNER_TIPS`],
/// and writes it back pretty-printed. Creates the file and `.claude/` directory
/// when absent.
/// Test: `prepare_session_sets_output_style`,
/// `write_output_style_preserves_existing_keys`,
/// `write_output_style_sets_spinner_tips`.
pub(super) fn write_output_style(project_dir: &Path) -> Result<(), PrepError> {
    let claude_dir = project_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).map_err(|source| PrepError::Io {
        path: claude_dir.clone(),
        source,
    })?;
    let settings_path = claude_dir.join("settings.json");

    // Load existing settings to preserve unrelated keys; tolerate a missing or
    // malformed file by starting from an empty object.
    let mut settings = match std::fs::read_to_string(&settings_path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };

    settings["outputStyle"] = serde_json::Value::String(OUTPUT_STYLE.to_string());
    settings["spinnerTipsEnabled"] = serde_json::Value::Bool(true);
    settings["spinnerTipsOverride"] = serde_json::json!({ "tips": SPINNER_TIPS });

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(&settings_path, serialized).map_err(|source| PrepError::Io {
        path: settings_path.clone(),
        source,
    })?;
    Ok(())
}

/// Write the `trusty-memory` hook block into the project's `.claude/settings.json`.
///
/// Why: `trusty-memory` hooks must fire only for trusty-mpm-managed sessions.
/// Scoping them to the project settings (instead of the operator's global
/// `~/.claude/settings.json`) means they no longer run for unrelated Claude
/// Code sessions such as claude-mpm.
/// What: reads an existing `<project>/.claude/settings.json` (preserving all
/// other keys), *replaces* the entire `hooks` key with [`TRUSTY_MEMORY_HOOKS`],
/// and writes it back pretty-printed. Replacing — rather than merging — the
/// `hooks` key avoids double-firing if this runs twice. Creates the file and
/// `.claude/` directory when absent.
/// Test: `write_project_hooks_writes_all_event_types`,
/// `write_project_hooks_replaces_existing`.
pub(super) fn write_project_hooks(project_dir: &Path) -> Result<(), PrepError> {
    let claude_dir = project_dir.join(".claude");
    std::fs::create_dir_all(&claude_dir).map_err(|source| PrepError::Io {
        path: claude_dir.clone(),
        source,
    })?;
    let settings_path = claude_dir.join("settings.json");

    // Load existing settings to preserve unrelated keys; tolerate a missing or
    // malformed file by starting from an empty object.
    let mut settings = match std::fs::read_to_string(&settings_path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };

    // Replace the entire `hooks` key. The bundled block is a constant and is
    // guaranteed to parse.
    let hooks: serde_json::Value =
        serde_json::from_str(TRUSTY_MEMORY_HOOKS).expect("bundled hook block is valid JSON");
    settings["hooks"] = hooks;

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(&settings_path, serialized).map_err(|source| PrepError::Io {
        path: settings_path.clone(),
        source,
    })?;
    Ok(())
}

/// Inject the `trusty-memory` MCP server into the project's `.mcp.json`.
///
/// Why: `prepare_session` configures hooks and instructions but, without this,
/// the launched `claude` process has no access to the memory tools because the
/// `trusty-memory` MCP server is never registered in `<project>/.mcp.json`.
/// What: reads an existing `<project_path>/.mcp.json` (starting from `{}` when
/// absent or malformed), adds/updates the `trusty-memory` entry under
/// `mcpServers` with [`TRUSTY_MEMORY_MCP_SERVER`], and writes the merged JSON
/// back pretty-printed — preserving all other MCP servers. Idempotent: if the
/// entry already matches, the file is left untouched.
/// Test: `inject_trusty_memory_mcp_adds_server`,
/// `inject_trusty_memory_mcp_preserves_existing`,
/// `inject_trusty_memory_mcp_is_idempotent`.
pub(super) fn inject_trusty_memory_mcp(project_path: &Path) -> Result<(), PrepError> {
    let mcp_path = project_path.join(".mcp.json");

    // Load existing config to preserve unrelated servers; tolerate a missing or
    // malformed file by starting from an empty object.
    let mut config = match std::fs::read_to_string(&mcp_path) {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .filter(serde_json::Value::is_object)
            .unwrap_or_else(|| serde_json::Value::Object(serde_json::Map::new())),
        Err(_) => serde_json::Value::Object(serde_json::Map::new()),
    };

    // The bundled server block is a constant and is guaranteed to parse.
    let server: serde_json::Value = serde_json::from_str(TRUSTY_MEMORY_MCP_SERVER)
        .expect("bundled trusty-memory MCP server block is valid JSON");

    // Ensure `mcpServers` is an object we can insert into.
    let servers = config
        .as_object_mut()
        .expect("config starts as an object")
        .entry("mcpServers")
        .or_insert_with(|| serde_json::Value::Object(serde_json::Map::new()));
    if !servers.is_object() {
        *servers = serde_json::Value::Object(serde_json::Map::new());
    }
    let servers = servers
        .as_object_mut()
        .expect("mcpServers normalized to an object");

    // Idempotent: skip the write when the entry already matches.
    if servers.get("trusty-memory") == Some(&server) {
        return Ok(());
    }
    servers.insert("trusty-memory".to_string(), server);

    let serialized =
        serde_json::to_string_pretty(&config).map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(&mcp_path, serialized).map_err(|source| PrepError::Io {
        path: mcp_path.clone(),
        source,
    })?;
    Ok(())
}

/// Remove the `trusty-memory` hook entries from `~/.claude/settings.json`.
///
/// Why: `trusty-memory` hooks were previously registered globally, so they
/// fired for every Claude Code session. Now that [`write_project_hooks`]
/// scopes them to trusty-mpm projects, the global entries must be removed to
/// stop them double-firing (and firing for unrelated sessions like claude-mpm).
/// What: reads `~/.claude/settings.json`, and for each event in
/// [`GLOBAL_TRUSTY_MEMORY_EVENTS`] filters out handler groups whose `hooks`
/// array contains a command matching `trusty-memory hooks fire`. An event key
/// whose array becomes empty is removed entirely. Writes the file back. A
/// missing or malformed file is treated as success (nothing to clean up).
/// Test: `remove_global_hooks_removes_trusty_memory_entries`.
pub(super) fn remove_global_trusty_memory_hooks() -> Result<(), PrepError> {
    let home = match dirs::home_dir() {
        Some(home) => home,
        None => {
            tracing::warn!("skipping global trusty-memory hook removal: home unresolved");
            return Ok(());
        }
    };
    let settings_path = home.join(".claude").join("settings.json");
    clean_global_trusty_memory_hooks(&settings_path)
}

/// Filter `trusty-memory` hook entries out of the settings file at `settings_path`.
///
/// Why: split from [`remove_global_trusty_memory_hooks`] so tests can target a
/// temp file instead of the operator's real `~/.claude/settings.json`.
/// What: see [`remove_global_trusty_memory_hooks`]. A missing or malformed
/// file is a no-op success.
/// Test: `remove_global_hooks_removes_trusty_memory_entries`.
pub(super) fn clean_global_trusty_memory_hooks(settings_path: &Path) -> Result<(), PrepError> {
    let text = match std::fs::read_to_string(settings_path) {
        Ok(text) => text,
        // Missing file: nothing to clean up.
        Err(_) => return Ok(()),
    };
    let mut settings = match serde_json::from_str::<serde_json::Value>(&text) {
        Ok(value) if value.is_object() => value,
        // Malformed or non-object: leave it untouched rather than risk loss.
        _ => return Ok(()),
    };

    let Some(hooks) = settings.get_mut("hooks").and_then(|h| h.as_object_mut()) else {
        return Ok(());
    };

    for event in GLOBAL_TRUSTY_MEMORY_EVENTS {
        let Some(groups) = hooks.get_mut(*event).and_then(|g| g.as_array_mut()) else {
            continue;
        };
        groups.retain(|group| !group_is_trusty_memory(group));
        if groups.is_empty() {
            hooks.remove(*event);
        }
    }

    let serialized = serde_json::to_string_pretty(&settings)
        .map_err(|err| PrepError::Deploy(err.to_string()))?;
    std::fs::write(settings_path, serialized).map_err(|source| PrepError::Io {
        path: settings_path.to_path_buf(),
        source,
    })?;
    Ok(())
}

/// Whether a hook handler group is a `trusty-memory` entry.
///
/// Why: identifies the groups [`clean_global_trusty_memory_hooks`] must drop.
/// What: returns `true` when any command in the group's `hooks` array contains
/// the substring `trusty-memory hooks fire`.
/// Test: covered indirectly by `remove_global_hooks_removes_trusty_memory_entries`.
pub(super) fn group_is_trusty_memory(group: &serde_json::Value) -> bool {
    group
        .get("hooks")
        .and_then(|h| h.as_array())
        .is_some_and(|handlers| {
            handlers.iter().any(|handler| {
                handler
                    .get("command")
                    .and_then(|c| c.as_str())
                    .is_some_and(|cmd| cmd.contains("trusty-memory hooks fire"))
            })
        })
}
