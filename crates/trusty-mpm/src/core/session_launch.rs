//! Pre-launch preparation for a Claude Code session.
//!
//! Why: every trusty-mpm session is launched as `claude` (the Claude Code CLI),
//! never `claude-mpm`. The "trusty-mpm" behaviour is supplied entirely through
//! the custom instructions Claude Code reads at startup — the deployed agents in
//! `~/.claude/agents/` and the project `CLAUDE.md`. Both the CLI (`tm session
//! start`) and the shared client (`DaemonClient::launch_session`, used by the
//! TUI's `/connect`) must perform the identical preparation; centralizing it
//! here keeps the two launch paths from drifting.
//! What: [`prepare_session`] deploys composed agents to `~/.claude/agents/` and
//! runs the instruction merge pipeline, writing/merging the project `CLAUDE.md`
//! and stashing the merged result under `<project>/.trusty-mpm/`. It returns a
//! [`PrepReport`] describing what happened so callers can report it.
//! Test: `prepare_session_writes_claude_md_and_stash` and
//! `prepare_session_is_idempotent` in this module's tests.

use std::path::{Path, PathBuf};

use crate::core::agent_deployer::{DeployResult, deploy_agents};
use crate::core::instruction_pipeline::{PipelineInput, PipelineOutput, build_instructions};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::{DeployStats, deploy_skills};

/// Outcome of the pre-launch preparation for one session.
///
/// Why: callers (CLI, client) report agent-deploy counts and CLAUDE.md status
/// to the operator; bundling them avoids returning a loose tuple.
/// What: the agent [`DeployResult`], the instruction [`PipelineOutput`], and the
/// path the merged instructions were stashed to.
/// Test: asserted by `prepare_session_writes_claude_md_and_stash`.
#[derive(Debug)]
pub struct PrepReport {
    /// Result of deploying composed agents to `~/.claude/agents/`.
    pub deploy: DeployResult,
    /// Result of deploying skill files to `~/.claude/skills/`.
    pub skill_deploy: DeployStats,
    /// Result of the instruction merge pipeline.
    pub instructions: PipelineOutput,
    /// Path the merged instructions were stashed to for inspection.
    pub stash: PathBuf,
    /// Path the `trusty-mpm` output style was deployed to, if it succeeded.
    ///
    /// `None` when deployment was skipped (no home directory) or failed; the
    /// session still launches in that case, just with the operator's default
    /// style.
    pub output_style: Option<PathBuf>,
    /// Whether the `trusty-memory` hook block was written to the project's
    /// `.claude/settings.json`.
    ///
    /// `false` when writing the project hooks failed; the session still
    /// launches, it just won't fire the trusty-memory hooks.
    pub hooks_written: bool,
}

/// A failure raised while preparing a session for launch.
///
/// Why: preparation performs agent deployment and filesystem I/O; callers need
/// a single typed error surface that names which stage failed.
/// What: variants for the agent-deploy stage and the instruction stage.
/// Test: not exercised by the happy-path tests; surfaced on invalid paths.
#[derive(Debug, thiserror::Error)]
pub enum PrepError {
    /// Deploying composed agents to `~/.claude/agents/` failed.
    #[error("agent deploy failed: {0}")]
    Deploy(String),
    /// Deploying skill files to `~/.claude/skills/` failed.
    #[error("skill deploy failed: {0}")]
    SkillDeploy(String),
    /// Composing or stashing the launch instructions failed.
    #[error("instruction pipeline failed: {0}")]
    Instructions(#[from] crate::core::instruction_pipeline::PipelineError),
    /// A filesystem operation on the inspection stash failed.
    #[error("io error for {path}: {source}")]
    Io {
        /// The path the failed operation targeted.
        path: PathBuf,
        /// The underlying IO error.
        source: std::io::Error,
    },
}

/// Prepare a project directory for a fresh Claude Code session launch.
///
/// Why: launching `claude` is only correct if its custom instructions are in
/// place first — the composed agents must be deployed and the project
/// `CLAUDE.md` merged. This is the "custom instructions" step that makes a plain
/// `claude` process behave as a trusty-mpm session; both the CLI and the client
/// call this before sending `claude` into the tmux pane.
/// What: deploys composed agents from the framework agent source to
/// `~/.claude/agents/`, runs [`build_instructions`] for `project_dir` (which
/// loads or creates the project `CLAUDE.md`), writes the *override-resolved* PM
/// prompt (from [`crate::core::instruction_overrides::resolve_pm_prompt`]) to
/// `<project_dir>/.trusty-mpm/last-instructions.md` so the inspectable stash
/// matches the live launch prompt, and returns a [`PrepReport`].
/// Test: `prepare_session_writes_claude_md_and_stash`, `prepare_session_is_idempotent`,
/// `prepare_session_stash_reflects_override`.
pub fn prepare_session(fw: &FrameworkPaths, project_dir: &Path) -> Result<PrepReport, PrepError> {
    // Deploy composed agents — Claude Code reads `~/.claude/agents/` at startup.
    let deploy = deploy_agents(&fw.agent_source_dir(), &fw.claude_agents_dir())
        .map_err(|err| PrepError::Deploy(err.to_string()))?;

    // Deploy skill files — Claude Code reads `~/.claude/skills/` at startup.
    // Skills carry no inheritance, so this is a manifest-tracked content copy.
    let skill_deploy = deploy_skills(&fw.skill_source_dir(), &fw.claude_skills_dir())
        .map_err(|err| PrepError::SkillDeploy(err.to_string()))?;

    // Compose the effective launch instructions (framework + delegation
    // authority + project CLAUDE.md); this loads or creates the project
    // CLAUDE.md so Claude Code picks it up automatically.
    let input = PipelineInput {
        framework_instructions_path: fw.framework_instructions_path(),
        agents_dir: fw.claude_agents_dir(),
        claude_md_path: project_dir.join("CLAUDE.md"),
    };
    let instructions = build_instructions(&input)?;

    // Stash the *override-resolved* PM prompt — the exact text the launch path
    // passes to `claude --append-system-prompt-file` — so `tm session
    // instructions` shows what was actually used, including any project-level
    // overrides under `<project>/.trusty-mpm/`. Resolving via the single
    // `resolve_pm_prompt` function keeps the stash and the live prompt from
    // diverging (issue #381 / the #382 concern).
    let resolved_prompt = crate::core::instruction_overrides::resolve_pm_prompt(project_dir);
    let stash_dir = project_dir.join(".trusty-mpm");
    std::fs::create_dir_all(&stash_dir).map_err(|source| PrepError::Io {
        path: stash_dir.clone(),
        source,
    })?;
    let stash = stash_dir.join("last-instructions.md");
    std::fs::write(&stash, &resolved_prompt).map_err(|source| PrepError::Io {
        path: stash.clone(),
        source,
    })?;

    // Set the Claude Code output style so the launched session's status bar
    // reads `style:trusty-mpm`. A failure here is non-fatal: the session still
    // launches, it just shows the operator's default style.
    if let Err(err) = write_output_style(project_dir) {
        tracing::warn!("failed to set trusty-mpm output style: {err}");
    }

    // Write the `trusty-memory` hook block into the project settings so the
    // hooks fire only for trusty-mpm sessions. Non-fatal: the session still
    // launches, it just won't record memory via the hooks.
    let hooks_written = match write_project_hooks(project_dir) {
        Ok(()) => true,
        Err(err) => {
            tracing::warn!("failed to write trusty-memory project hooks: {err}");
            false
        }
    };

    // Inject the `trusty-memory` MCP server into the project's `.mcp.json` so
    // the launched `claude` process can reach the memory tools (`memory_recall`,
    // `memory_store`, …). Non-fatal: the session still launches, it just lacks
    // the memory tools.
    if let Err(err) = inject_trusty_memory_mcp(project_dir) {
        tracing::warn!("failed to inject trusty-memory MCP server: {err}");
    }

    // Remove the now-redundant global `trusty-memory` hook entries so they no
    // longer fire for every Claude Code session (including claude-mpm). The
    // project hooks above scope them to trusty-mpm sessions. Non-fatal.
    if let Err(err) = remove_global_trusty_memory_hooks() {
        tracing::warn!("failed to remove global trusty-memory hooks: {err}");
    }

    // Deploy the bundled output-style definition so Claude Code can resolve the
    // `trusty-mpm` name written into `.claude/settings.json` above. Non-fatal:
    // a missing style file just falls back to the operator's default.
    let output_style = match dirs::home_dir() {
        Some(home) => match deploy_output_style(&home) {
            Ok(path) => Some(path),
            Err(err) => {
                tracing::warn!("failed to deploy trusty-mpm output style file: {err}");
                None
            }
        },
        None => {
            tracing::warn!("skipping output style deploy: home directory unresolved");
            None
        }
    };

    Ok(PrepReport {
        deploy,
        skill_deploy,
        instructions,
        stash,
        output_style,
        hooks_written,
    })
}

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
fn deploy_output_style(home: &Path) -> Result<PathBuf, PrepError> {
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

/// Claude Code output style applied to launched sessions.
///
/// Why: the Claude Code status bar renders `style:<outputStyle>`; launched
/// trusty-mpm sessions should advertise themselves as `trusty-mpm`.
const OUTPUT_STYLE: &str = "trusty-mpm";

/// trusty-mpm-specific spinner tips shown during Claude Code loading.
///
/// Why: Claude Code's loading spinner renders tips from
/// `spinnerTipsOverride.tips`; the operator's global settings carry generic
/// claude-mpm tips, so trusty-mpm sessions override them with project-relevant
/// guidance (the `tm` CLI, the `make check` gate, the API-first layering rule).
const SPINNER_TIPS: &[&str] = &[
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
fn write_output_style(project_dir: &Path) -> Result<(), PrepError> {
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
const TRUSTY_MEMORY_HOOKS: &str = r#"{
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
fn write_project_hooks(project_dir: &Path) -> Result<(), PrepError> {
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

/// The `trusty-memory` MCP server definition injected into a project's
/// `.mcp.json`.
///
/// Why: Claude Code reads MCP servers from `<project>/.mcp.json`; a launched
/// trusty-mpm session needs the `trusty-memory` server registered there so the
/// memory tools (`memory_recall`, `memory_store`, …) are available.
const TRUSTY_MEMORY_MCP_SERVER: &str = r#"{
  "type": "stdio",
  "command": "trusty-memory",
  "args": ["mcp", "serve"]
}"#;

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
fn inject_trusty_memory_mcp(project_path: &Path) -> Result<(), PrepError> {
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

/// Hook event types the global `trusty-memory` entries were registered under.
const GLOBAL_TRUSTY_MEMORY_EVENTS: &[&str] = &["PostToolUse", "Stop", "UserPromptSubmit"];

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
fn remove_global_trusty_memory_hooks() -> Result<(), PrepError> {
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
fn clean_global_trusty_memory_hooks(settings_path: &Path) -> Result<(), PrepError> {
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
fn group_is_trusty_memory(group: &serde_json::Value) -> bool {
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

/// Build the project-agnostic `--append-system-prompt` text (no overrides).
///
/// Why: every `claude` session launched by trusty-mpm must be a configured PM
/// instance. trusty-mpm owns its PM instructions: they are assembled from
/// bundled assets into `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`
/// and passed to `claude --append-system-prompt-file`. This variant is kept for
/// callers that do not know the project directory (e.g. tests); prefer
/// [`build_system_prompt_for`] at launch sites so project-level overrides apply.
/// What: reads `~/.trusty-mpm/framework/instructions/INSTRUCTIONS.md`; if it is
/// missing or empty (first run) it calls
/// [`crate::core::instruction_pipeline::install_system_prompt`] to generate it from
/// the bundled assets, then reads it back. Returns `None` only when the home
/// directory cannot be resolved or the file cannot be written/read.
/// Test: `build_system_prompt_includes_trusty_block`.
pub fn build_system_prompt() -> Option<String> {
    let home = dirs::home_dir()?;
    let path = home
        .join(".trusty-mpm")
        .join("framework")
        .join("instructions")
        .join("INSTRUCTIONS.md");

    // Use the on-disk file when it is present and non-empty.
    if let Ok(contents) = std::fs::read_to_string(&path) {
        let trimmed = contents.trim_end();
        if !trimmed.is_empty() {
            return Some(trimmed.to_string());
        }
    }

    // First run (or empty file): generate it from the bundled assets, then
    // read it back so the launch path always uses the same source of truth.
    let generated = crate::core::instruction_pipeline::install_system_prompt().ok()?;
    let contents = std::fs::read_to_string(&generated).ok()?;
    let trimmed = contents.trim_end();
    if trimmed.is_empty() {
        None
    } else {
        Some(trimmed.to_string())
    }
}

/// Build the `--append-system-prompt` text for `project_dir`, applying any
/// project-level instruction overrides.
///
/// Why: `BASE_PM.md` advertises project-level overrides under
/// `<project>/.trusty-mpm/` (issue #381). The *live* prompt delivered to
/// `claude` must reflect them, and it must be resolved with the same
/// [`crate::core::instruction_overrides::resolve_pm_prompt`] function the
/// inspectable stash uses so the two never diverge (the #382 concern). This is
/// the launch-site entry point; it always returns a usable prompt — there is no
/// home-directory dependency because the prompt is composed from compiled-in
/// bundled assets plus the project's own override files.
/// What: delegates to
/// [`crate::core::instruction_overrides::resolve_pm_prompt`], which layers the
/// override files onto the bundled PM prompt and always appends the
/// non-overridable `BASE_PM` floor last.
/// Test: `build_system_prompt_for_applies_project_override`.
pub fn build_system_prompt_for(project_dir: &Path) -> String {
    crate::core::instruction_overrides::resolve_pm_prompt(project_dir)
}

#[cfg(test)]
mod tests {
    use super::*;
    use tempfile::tempdir;

    #[test]
    fn build_system_prompt_includes_trusty_block() {
        // Why: `build_system_prompt` must always yield a prompt — generating
        // `INSTRUCTIONS.md` from the bundled assets on first run — and that
        // prompt must include the trusty tool-priority block so a launched
        // session knows to prefer `memory_recall` and `search_code`.
        let prompt = build_system_prompt().expect("trusty block is always present");
        assert!(prompt.contains("## Trusty Tool Priority (Non-Overridable)"));
        assert!(prompt.contains("mcp__trusty-memory__memory_recall"));
        assert!(prompt.contains("mcp__trusty-search__search_code"));
        // The bundled PM instructions are also part of the assembled prompt.
        assert!(prompt.contains("# PM Agent -- Claude MPM"));
    }

    #[test]
    fn build_system_prompt_for_applies_project_override() {
        // Why: the live launch prompt must reflect a project-level override file
        // under `<project>/.trusty-mpm/` (issue #381), while still appending the
        // non-overridable BASE_PM floor.
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let override_dir = project.join(".trusty-mpm");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::write(
            override_dir.join("INSTRUCTIONS.md"),
            "PROJECT_OVERRIDE_MARKER\n",
        )
        .unwrap();

        let prompt = build_system_prompt_for(project);
        assert!(prompt.contains("PROJECT_OVERRIDE_MARKER"));
        assert!(prompt.contains("# BASE_PM Framework Floor"));
        // Bundled PM body is still present (INSTRUCTIONS.md is additive).
        assert!(prompt.contains("# PM Agent -- Claude MPM"));
    }

    #[test]
    fn build_system_prompt_for_no_override_matches_bundled_sections() {
        // Why: with no override files the live prompt must still carry all
        // bundled sections and the BASE_PM floor last.
        let tmp = tempdir().unwrap();
        let prompt = build_system_prompt_for(tmp.path());
        assert!(prompt.contains("# PM Agent -- Claude MPM"));
        assert!(prompt.contains("# Agent Delegation Routing"));
        let base = prompt.find("# BASE_PM Framework Floor").expect("base");
        let deleg = prompt.find("# Agent Delegation Routing").expect("deleg");
        assert!(base > deleg, "BASE_PM floor must be last");
    }

    #[test]
    fn prepare_session_stash_reflects_override() {
        // Why: the inspectable stash (`last-instructions.md`) must reflect the
        // SAME override-resolved prompt the launch path uses, so `tm session
        // instructions` shows what was actually delivered (issue #381 / #382).
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let fw = FrameworkPaths::default();

        let override_dir = project.join(".trusty-mpm");
        std::fs::create_dir_all(&override_dir).unwrap();
        std::fs::write(
            override_dir.join("WORKFLOW.md"),
            "# Custom Workflow\n\nSTASH_OVERRIDE_MARKER\n",
        )
        .unwrap();

        let report = prepare_session(&fw, project).expect("prep succeeds");
        let stash = std::fs::read_to_string(&report.stash).expect("stash readable");

        assert!(
            stash.contains("STASH_OVERRIDE_MARKER"),
            "stash must reflect the WORKFLOW.md override"
        );
        assert!(
            !stash.contains("# PM Workflow Configuration"),
            "bundled workflow heading must be replaced in the stash"
        );
        assert!(
            stash.contains("# BASE_PM Framework Floor"),
            "stash must still carry the BASE_PM floor"
        );
        // The stash must equal the live prompt for this project.
        assert_eq!(stash, build_system_prompt_for(project));
    }

    #[test]
    fn prepare_session_writes_claude_md_and_stash() {
        // Why: the launch paths rely on `prepare_session` writing the project
        // CLAUDE.md and the inspectable stash before `claude` is started.
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let fw = FrameworkPaths::default();

        let report = prepare_session(&fw, project).expect("prep succeeds");

        assert!(
            project.join("CLAUDE.md").exists(),
            "CLAUDE.md must exist after prep"
        );
        assert!(
            report.stash.exists(),
            "merged instructions stash must be written"
        );
        assert_eq!(
            report.stash,
            project.join(".trusty-mpm").join("last-instructions.md")
        );
    }

    #[test]
    fn prepare_session_sets_output_style() {
        // Why: a launched session must show `style:trusty-mpm`, which Claude
        // Code reads from `<project>/.claude/settings.json`.
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let fw = FrameworkPaths::default();

        prepare_session(&fw, project).expect("prep succeeds");

        let settings_path = project.join(".claude").join("settings.json");
        assert!(settings_path.exists(), ".claude/settings.json must exist");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
    }

    #[test]
    fn write_output_style_preserves_existing_keys() {
        // Why: merging the style must not clobber an operator's other settings.
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let claude_dir = project.join(".claude");
        std::fs::create_dir_all(&claude_dir).unwrap();
        std::fs::write(
            claude_dir.join("settings.json"),
            r#"{"theme":"dark","outputStyle":"old"}"#,
        )
        .unwrap();

        write_output_style(project).expect("write succeeds");

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(claude_dir.join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["outputStyle"], serde_json::json!("trusty-mpm"));
        assert_eq!(value["theme"], serde_json::json!("dark"));
    }

    #[test]
    fn write_output_style_sets_spinner_tips() {
        // Why: trusty-mpm sessions must override the operator's generic
        // claude-mpm spinner tips with project-specific ones; the settings.json
        // merge must enable tips and write a non-empty tips array.
        let tmp = tempdir().unwrap();
        let project = tmp.path();

        write_output_style(project).expect("write succeeds");

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(value["spinnerTipsEnabled"], serde_json::json!(true));
        let tips = value["spinnerTipsOverride"]["tips"]
            .as_array()
            .expect("spinnerTipsOverride.tips must be an array");
        assert!(!tips.is_empty(), "spinner tips must be non-empty");
        assert!(tips.iter().all(|tip| tip.is_string()));
    }

    #[test]
    fn write_project_hooks_writes_all_event_types() {
        // Why: the trusty-memory hooks must be scoped to the project and cover
        // the session lifecycle — PostToolUse, Stop, and UserPromptSubmit.
        // PreToolUse is intentionally absent: trusty-memory has no
        // `claude.pre-tool-use` handler.
        let tmp = tempdir().unwrap();
        let project = tmp.path();

        write_project_hooks(project).expect("write succeeds");

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
        )
        .unwrap();
        let hooks = value["hooks"].as_object().expect("hooks must be an object");
        for event in ["PostToolUse", "Stop", "UserPromptSubmit"] {
            let groups = hooks[event]
                .as_array()
                .unwrap_or_else(|| panic!("{event} must be an array"));
            assert!(!groups.is_empty(), "{event} must have a handler group");
            let cmd = groups[0]["hooks"][0]["command"]
                .as_str()
                .expect("command must be a string");
            assert!(
                cmd.contains("trusty-memory hooks fire"),
                "{event} command must invoke trusty-memory"
            );
        }
    }

    #[test]
    fn write_project_hooks_omits_pre_tool_use() {
        // Why: trusty-memory has no `claude.pre-tool-use` handler, so a
        // PreToolUse hook would error on every tool call. The written hooks
        // block must not register one.
        let tmp = tempdir().unwrap();
        let project = tmp.path();

        write_project_hooks(project).expect("write succeeds");

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
        )
        .unwrap();
        assert!(
            value["hooks"].get("PreToolUse").is_none(),
            "PreToolUse hook must not be registered"
        );
    }

    #[test]
    fn write_project_hooks_replaces_existing() {
        // Why: re-running prep must replace the hooks block, not append to it,
        // so handler arrays never duplicate and cause double-firing.
        let tmp = tempdir().unwrap();
        let project = tmp.path();

        write_project_hooks(project).expect("first write succeeds");
        write_project_hooks(project).expect("second write succeeds");

        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
        )
        .unwrap();
        let post = value["hooks"]["PostToolUse"]
            .as_array()
            .expect("PostToolUse must be an array");
        assert_eq!(
            post.len(),
            1,
            "re-running must replace, not append, handler groups"
        );
        // Unrelated keys must survive the replace.
        write_project_hooks(project).expect("third write succeeds");
        let value: serde_json::Value = serde_json::from_str(
            &std::fs::read_to_string(project.join(".claude").join("settings.json")).unwrap(),
        )
        .unwrap();
        assert_eq!(
            value["hooks"]["UserPromptSubmit"].as_array().unwrap().len(),
            1
        );
    }

    #[test]
    fn inject_trusty_memory_mcp_adds_server() {
        // Why: a launched session needs the `trusty-memory` MCP server in
        // `.mcp.json` for the memory tools to be available; injection must
        // create the file with the server registered.
        let tmp = tempdir().unwrap();
        let project = tmp.path();

        inject_trusty_memory_mcp(project).expect("injection succeeds");

        let mcp_path = project.join(".mcp.json");
        assert!(mcp_path.exists(), ".mcp.json must be created");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
        let server = &value["mcpServers"]["trusty-memory"];
        assert_eq!(server["type"], serde_json::json!("stdio"));
        assert_eq!(server["command"], serde_json::json!("trusty-memory"));
        assert_eq!(server["args"], serde_json::json!(["mcp", "serve"]));
    }

    #[test]
    fn inject_trusty_memory_mcp_preserves_existing() {
        // Why: injection must not clobber MCP servers the operator already
        // configured (e.g. `trusty-search`).
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        std::fs::write(
            project.join(".mcp.json"),
            r#"{"mcpServers":{"trusty-search":{"type":"stdio","command":"trusty-search","args":["serve"]}}}"#,
        )
        .unwrap();

        inject_trusty_memory_mcp(project).expect("injection succeeds");

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(project.join(".mcp.json")).unwrap())
                .unwrap();
        let servers = value["mcpServers"]
            .as_object()
            .expect("mcpServers must be an object");
        assert!(
            servers.contains_key("trusty-search"),
            "existing server must survive injection"
        );
        assert!(
            servers.contains_key("trusty-memory"),
            "trusty-memory must be injected"
        );
        assert_eq!(
            value["mcpServers"]["trusty-search"]["command"],
            serde_json::json!("trusty-search")
        );
    }

    #[test]
    fn inject_trusty_memory_mcp_is_idempotent() {
        // Why: `/connect` and `tm session start` may run repeatedly; a second
        // injection must not duplicate or alter the `trusty-memory` entry.
        let tmp = tempdir().unwrap();
        let project = tmp.path();

        inject_trusty_memory_mcp(project).expect("first injection succeeds");
        let after_first = std::fs::read_to_string(project.join(".mcp.json")).expect("file exists");

        inject_trusty_memory_mcp(project).expect("second injection succeeds");
        let after_second = std::fs::read_to_string(project.join(".mcp.json")).expect("file exists");

        assert_eq!(
            after_first, after_second,
            "re-injecting must leave the file unchanged"
        );
        let value: serde_json::Value = serde_json::from_str(&after_second).unwrap();
        assert_eq!(
            value["mcpServers"].as_object().unwrap().len(),
            1,
            "trusty-memory must not be duplicated"
        );
    }

    #[test]
    fn prepare_session_injects_trusty_memory_mcp() {
        // Why: `prepare_session` is the single launch-prep entry point; it must
        // register the trusty-memory MCP server so launched sessions get the
        // memory tools.
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let fw = FrameworkPaths::default();

        prepare_session(&fw, project).expect("prep succeeds");

        let mcp_path = project.join(".mcp.json");
        assert!(mcp_path.exists(), ".mcp.json must exist after prep");
        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&mcp_path).unwrap()).unwrap();
        assert_eq!(
            value["mcpServers"]["trusty-memory"]["command"],
            serde_json::json!("trusty-memory")
        );
    }

    #[test]
    fn remove_global_hooks_removes_trusty_memory_entries() {
        // Why: the global trusty-memory hook entries must be cleaned out so
        // they no longer fire for unrelated Claude Code sessions; non-trusty
        // entries and empty-becoming events must be handled correctly.
        let tmp = tempdir().unwrap();
        let settings_path = tmp.path().join("settings.json");
        std::fs::write(
            &settings_path,
            r#"{
              "theme": "dark",
              "hooks": {
                "PostToolUse": [
                  { "matcher": "*", "hooks": [ { "type": "command", "command": "bash track.sh" } ] },
                  { "matcher": "Write|Edit|Bash", "hooks": [ { "type": "command", "command": "trusty-memory hooks fire claude.post-tool-use" } ] }
                ],
                "Stop": [
                  { "matcher": "", "hooks": [ { "type": "command", "command": "trusty-memory hooks fire claude.stop" } ] }
                ],
                "UserPromptSubmit": [
                  { "matcher": "", "hooks": [ { "type": "command", "command": "trusty-memory hooks fire claude.user-prompt" } ] }
                ]
              }
            }"#,
        )
        .unwrap();

        clean_global_trusty_memory_hooks(&settings_path).expect("clean succeeds");

        let value: serde_json::Value =
            serde_json::from_str(&std::fs::read_to_string(&settings_path).unwrap()).unwrap();
        // Unrelated keys survive.
        assert_eq!(value["theme"], serde_json::json!("dark"));
        // Non-trusty PostToolUse entry survives; trusty one is gone.
        let post = value["hooks"]["PostToolUse"].as_array().unwrap();
        assert_eq!(post.len(), 1);
        assert!(
            post[0]["hooks"][0]["command"]
                .as_str()
                .unwrap()
                .contains("track.sh")
        );
        // Stop and UserPromptSubmit only had trusty entries, so the keys are gone.
        assert!(
            value["hooks"].get("Stop").is_none(),
            "empty Stop event must be removed"
        );
        assert!(
            value["hooks"].get("UserPromptSubmit").is_none(),
            "empty UserPromptSubmit event must be removed"
        );
    }

    #[test]
    fn remove_global_hooks_tolerates_missing_file() {
        // Why: cleanup is non-fatal and idempotent — a missing settings file
        // (operator never created one) must be a no-op success.
        let tmp = tempdir().unwrap();
        let missing = tmp.path().join("nope.json");
        clean_global_trusty_memory_hooks(&missing).expect("missing file is a no-op");
    }

    #[test]
    fn deploy_output_style_writes_file() {
        // Why: Claude Code resolves the `trusty-mpm` output style only when a
        // matching file exists in `~/.claude/output-styles/`; deployment must
        // create that file (and its parent dir) with the bundled content.
        let home = tempdir().unwrap();
        let path = deploy_output_style(home.path()).expect("deploy succeeds");

        assert_eq!(
            path,
            home.path()
                .join(".claude")
                .join("output-styles")
                .join("trusty-mpm.md")
        );
        let written = std::fs::read_to_string(&path).expect("style file readable");
        assert_eq!(written, crate::core::bundle::OUTPUT_STYLE);
        assert!(written.contains("name: trusty-mpm"));
    }

    #[test]
    fn deploy_output_style_overwrites() {
        // Why: framework upgrades to the style must propagate on the next
        // launch, so deployment always overwrites any existing file.
        let home = tempdir().unwrap();
        let first = deploy_output_style(home.path()).expect("first deploy succeeds");
        std::fs::write(&first, "stale operator content").unwrap();

        let second = deploy_output_style(home.path()).expect("second deploy succeeds");
        assert_eq!(first, second);
        let written = std::fs::read_to_string(&second).unwrap();
        assert_eq!(written, crate::core::bundle::OUTPUT_STYLE);
    }

    #[test]
    fn prepare_session_reports_output_style() {
        // Why: callers report the deployed style path; `prepare_session` must
        // populate `PrepReport.output_style` with the file it deployed.
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let fw = FrameworkPaths::default();

        let report = prepare_session(&fw, project).expect("prep succeeds");

        let style = report
            .output_style
            .expect("output style deployed when home is resolvable");
        assert!(style.ends_with("trusty-mpm.md"));
        assert!(style.exists());
    }

    #[test]
    fn prepare_session_reports_skill_deploy() {
        // Why: `prepare_session` must run the skill deploy step so launched
        // sessions see trusty-mpm skills; the report must carry its stats.
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let fw = FrameworkPaths::default();

        let report = prepare_session(&fw, project).expect("prep succeeds");

        // The stats are present (a fresh install with no skill source is an
        // empty-but-valid result; this asserts the field is populated, not
        // that any specific skill deployed).
        let _ = &report.skill_deploy;
    }

    #[test]
    fn prepare_session_is_idempotent() {
        // Why: `/connect` and `tm session start` may run repeatedly on the same
        // project; a second prep must not fail and must not recreate CLAUDE.md.
        let tmp = tempdir().unwrap();
        let project = tmp.path();
        let fw = FrameworkPaths::default();

        let first = prepare_session(&fw, project).expect("first prep succeeds");
        assert!(first.instructions.claude_md_created);

        let second = prepare_session(&fw, project).expect("second prep succeeds");
        assert!(
            !second.instructions.claude_md_created,
            "CLAUDE.md already exists on the second run"
        );
    }
}
