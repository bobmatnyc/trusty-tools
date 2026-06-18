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

mod settings;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use crate::core::agent_deployer::{DeployResult, deploy_agents};
use crate::core::instruction_pipeline::{PipelineInput, PipelineOutput, build_instructions};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::{DeployStats, deploy_skills};
use settings::{
    deploy_output_style, inject_trusty_memory_mcp, inject_trusty_search_mcp,
    preseed_workspace_trust_home, register_project_index, remove_global_trusty_memory_hooks,
    write_output_style, write_project_hooks,
};

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
    prepare_session_with_style(fw, project_dir, None)
}

/// Prepare a session, selecting an explicit output style (HR-4).
///
/// Why: `tm launch --style <id>` lets the operator override the configured
/// active output style for a single launch; the override must reach the
/// `outputStyle` settings key (for native-capable Claude Code) and the
/// prompt-injection seam (for older builds). The flag-less [`prepare_session`]
/// delegates here with `None`.
/// What: identical to [`prepare_session`] except the active output-style id is
/// resolved via [`crate::core::output_style::resolve_active_style`] with
/// `explicit_style` taking precedence over the `[style] active` config key and
/// the professional default. An unknown id is logged and falls back to the
/// default (DOC-17) rather than failing the launch.
/// Test: `prepare_session_writes_configured_style`,
/// `prepare_session_explicit_style_overrides_config`.
pub fn prepare_session_with_style(
    fw: &FrameworkPaths,
    project_dir: &Path,
    explicit_style: Option<&str>,
) -> Result<PrepReport, PrepError> {
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

    // Resolve the active output style (HR-4): explicit `--style` override >
    // `[style] active` config key > professional default. An unknown id is
    // logged and falls back to the default rather than failing the launch
    // (DOC-17). The resolved id is written into `.claude/settings.json` so a
    // native-capable Claude Code (>= 1.0.83) applies it directly; older builds
    // pick it up via prompt injection at the `build_system_prompt_for` seam.
    let config = crate::core::config::MpmConfig::load(&fw.root);
    let active_style_id =
        match crate::core::output_style::resolve_active_style(&config, explicit_style) {
            Ok(style) => style.id,
            Err(err) => {
                tracing::warn!(
                    %err,
                    "falling back to the default output style for settings.json"
                );
                crate::core::bundle::DEFAULT_OUTPUT_STYLE_ID
            }
        };

    // Set the Claude Code output style so the launched session's status bar
    // reads `style:<active_style_id>`. A failure here is non-fatal: the session
    // still launches, it just shows the operator's default style.
    if let Err(err) = write_output_style(project_dir, Some(active_style_id)) {
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

    // Register + pin the project's trusty-search index (issue #1373). Derive
    // the project's canonical index id (git-root basename, via the shared
    // `trusty_common::derive_index_id`), best-effort find-or-create it in the
    // running daemon, then inject the `trusty-search` MCP stub PINNED to that id
    // (`serve --index <id>`). Pinning makes a bare `search`/`grep` resolve to
    // the session's OWN project index instead of letting the LLM guess (and
    // routinely pick the wrong `claude-mpm` index). The daemon-unreachable case
    // is handled inside `register_project_index` (logged, non-fatal) and still
    // returns the id so the stub is pinned; a `None` id (empty derivation) falls
    // back to the unpinned stub. Either way the session launches.
    let pinned_index = register_project_index(project_dir);
    if let Err(err) = inject_trusty_search_mcp(project_dir, pinned_index.as_deref()) {
        tracing::warn!("failed to inject trusty-search MCP server: {err}");
    }

    // Pre-seed per-directory trust for this workspace in `~/.claude.json`
    // (issue #1269) so the interactive tmux Claude session does not stall on the
    // "Do you trust this folder?" dialog and the injected task prompt is
    // received. tm owns this workspace path, so marking it trusted is safe.
    // Non-fatal: a trust-seed failure only means the operator may see the dialog.
    if let Err(err) = preseed_workspace_trust_home(project_dir) {
        tracing::warn!("failed to pre-seed workspace trust: {err}");
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
/// non-overridable `BASE_PM` floor last, then applies HR-4 output-style
/// version-fallback injection via [`build_system_prompt_for_with_style`] with no
/// explicit style override.
/// Test: `build_system_prompt_for_applies_project_override`.
pub fn build_system_prompt_for(project_dir: &Path) -> String {
    build_system_prompt_for_with_style(project_dir, None)
}

/// Build the launch prompt for `project_dir`, applying overrides AND HR-4
/// output-style version-fallback injection with an explicit style override.
///
/// Why: on Claude Code builds older than `1.0.83` the native `outputStyle`
/// settings key is ignored, so the active output style only takes effect if its
/// content is folded into the `--append-system-prompt` text. This is the single
/// launch seam where that injection happens, so both the CLI (`tm launch`) and
/// the client (`/connect`) get identical behaviour. The `--style` flag flows in
/// as `explicit_style`.
/// What: resolves the override-layered PM prompt via
/// [`crate::core::instruction_overrides::resolve_pm_prompt`], then calls
/// [`crate::core::output_style::apply_output_style_to_prompt`], which is a no-op
/// on native-capable Claude Code and prepends the active style otherwise. The
/// active style is `explicit_style` > `[style] active` config > professional
/// default; an unknown id falls back to the default.
/// Test: the injection logic is unit-tested in
/// `crate::core::output_style::tests`; this composition is covered by
/// `build_system_prompt_for_applies_project_override` (which asserts the PM
/// prompt is preserved regardless of the version gate).
pub fn build_system_prompt_for_with_style(
    project_dir: &Path,
    explicit_style: Option<&str>,
) -> String {
    let prompt = crate::core::instruction_overrides::resolve_pm_prompt(project_dir);
    crate::core::output_style::apply_output_style_to_prompt(project_dir, explicit_style, prompt)
}
