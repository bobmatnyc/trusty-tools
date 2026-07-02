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

mod search_index;
mod settings;
#[cfg(test)]
mod tests;

use std::path::{Path, PathBuf};

use crate::core::agent_deployer::{DeployResult, deploy_agents_filtered};
use crate::core::instruction_pipeline::{PipelineInput, PipelineOutput, build_instructions};
use crate::core::paths::FrameworkPaths;
use crate::core::skill_deployer::{DeployStats, deploy_skills_filtered};
use search_index::{inject_trusty_search_mcp, register_project_index};
use settings::{
    deploy_output_style, inject_trusty_memory_mcp, preseed_workspace_trust_home,
    remove_global_trusty_memory_hooks, write_output_style, write_project_hooks, write_status_line,
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
    /// `None` when the deploy write failed; the session still launches in
    /// that case, just with the operator's default style.
    pub output_style: Option<PathBuf>,
    /// Whether the `trusty-memory` hook block was written to the project's
    /// `.claude/settings.json`.
    ///
    /// `false` when writing the project hooks failed; the session still
    /// launches, it just won't fire the trusty-memory hooks.
    pub hooks_written: bool,
    /// Incremental catch-up context to inject as seed context for this session.
    ///
    /// Populated when `config.catchup.auto` is true; `None` otherwise or when
    /// the catch-up runtime fails (fail-open — never blocks session launch).
    /// The caller (session start path) should print this after the normal
    /// launch summary so the operator sees it in the terminal.
    ///
    // CUTOVER BRIDGE — remove post-migration (#1762)
    pub catchup_context: Option<String>,
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
/// loads or creates the project `CLAUDE.md`), writes the launch prompt — the
/// exact override-resolved AND output-style-injected text produced by
/// [`build_system_prompt_for_with_style`] — to
/// `<project_dir>/.trusty-mpm/last-instructions.md` so the inspectable stash
/// matches the live launch prompt byte-for-byte (issue #1409), and returns a
/// [`PrepReport`].
/// Test: `prepare_session_writes_claude_md_and_stash`, `prepare_session_is_idempotent`,
/// `prepare_session_stash_reflects_override`.
pub fn prepare_session(fw: &FrameworkPaths, project_dir: &Path) -> Result<PrepReport, PrepError> {
    prepare_session_with_style(fw, project_dir, None)
}

/// Prepare a session, threading the cloned-from `repo_url` for palace pinning.
///
/// Why (issue #1605): a managed session cloned from `repo_url` lives under a
/// throwaway `<owner>/<repo>/<session-id>/` workspace whose basename is the
/// session-id. Without the originating `repo_url`, the trusty-memory MCP
/// injection would derive (and pin) the WRONG palace from that directory
/// basename. The provisioner knows the `repo_url` it cloned, so threading it
/// here lets the injector pin `env.TRUSTY_MEMORY_PALACE` to the project's
/// canonical `owner-repo` slug. The flag-less [`prepare_session`] delegates with
/// `None`, which falls back to the workspace's own `git remote get-url origin`.
/// What: identical to [`prepare_session`] except the optional `repo_url` (from
/// `LaunchParams`/`SessionRecord`) is threaded down to the trusty-memory MCP
/// injector for palace-slug derivation. Real native-style detection is applied.
/// Test: `prepare_session_repo_url_pins_palace` in this module's tests.
pub fn prepare_session_with_repo_url(
    fw: &FrameworkPaths,
    project_dir: &Path,
    repo_url: Option<&str>,
) -> Result<PrepReport, PrepError> {
    let native = crate::core::output_style::claude_supports_native_output_style();
    prepare_session_inner(fw, project_dir, None, native, repo_url)
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
    // Probe the live Claude Code version ONCE and thread the decision through the
    // stash write so the stashed prompt matches what the launcher will inject
    // (issue #1409). Real detection, fail-safe to injection.
    let native = crate::core::output_style::claude_supports_native_output_style();
    prepare_session_with_style_and_native(fw, project_dir, explicit_style, native)
}

/// Prepare a session with the `native_supported` output-style decision supplied
/// explicitly (no live `claude --version` probe).
///
/// Why: the stash (`last-instructions.md`) must equal the launch prompt
/// byte-for-byte, and that prompt depends on whether Claude Code supports native
/// output styles. Probing `claude --version` inside `prepare_session` couples the
/// stash invariant to the host, which broke `prepare_session_stash_reflects_override`
/// on CI (issue #1409). This seam pins the decision so tests can assert the
/// invariant deterministically under BOTH `native_supported = true` and `false`;
/// [`prepare_session_with_style`] supplies real detection in production.
/// What: identical to [`prepare_session_with_style`] except the stash is written
/// from [`build_system_prompt_for_with_style_and_native`] using the supplied flag,
/// so the stash always reflects the exact injected (or non-injected) launch prompt.
/// Test: `prepare_session_stash_reflects_override`.
pub fn prepare_session_with_style_and_native(
    fw: &FrameworkPaths,
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
) -> Result<PrepReport, PrepError> {
    prepare_session_inner(fw, project_dir, explicit_style, native_supported, None)
}

/// Shared body for every `prepare_session*` entry point.
///
/// Why: the public entry points differ only in how they resolve `explicit_style`,
/// `native_supported`, and (issue #1605) the cloned-from `repo_url`. Funnelling
/// them through one private body keeps the long preparation sequence in a single
/// place so the variants cannot drift. `repo_url` is the only new degree of
/// freedom: it is threaded to the trusty-memory MCP injector for palace pinning
/// and is `None` for the flag-less / style-only entry points (which then fall
/// back to the workspace's own git origin remote).
/// What: identical to the documented [`prepare_session_with_style_and_native`]
/// behaviour, plus it passes `repo_url` to [`inject_trusty_memory_mcp`].
/// Test: covered by every `prepare_session_*` test plus
/// `prepare_session_repo_url_pins_palace`.
fn prepare_session_inner(
    fw: &FrameworkPaths,
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
    repo_url: Option<&str>,
) -> Result<PrepReport, PrepError> {
    // Load the user config ONCE and thread it through both the manifest
    // resolution / catalog-root path AND the style resolution path below. Reading
    // `config.toml` a second time mid-function (the old `MpmConfig::load` just
    // before style resolution) was a redundant filesystem read for the same data.
    let config = crate::core::config::MpmConfig::load(&fw.root);

    // Resolve the effective harness manifest (HR-2 / DOC-17) and materialize the
    // provisioning plan it implies. The NORMATIVE precedence is
    // project override > user config > catalog manifest > compiled-in default;
    // the compiled-in default reproduces today's provisioning exactly, so an
    // absent manifest is a zero-regression no-op. The plan tells us WHICH agents
    // and skills to deploy, from WHICH source (bundled vs synced catalog), which
    // MCP servers to inject, and the manifest's default output style. The
    // existing deploy machinery still does the deployment.
    //
    // The catalog root comes from the ONE shared helper (`catalog_root_for`) the
    // `tm catalog` CLI also uses, so the manifest path the resolver reads is the
    // same `<framework>/catalog` checkout `CatalogSync` populates — honouring the
    // `[manifest]` config / `TRUSTY_MPM_CATALOG_*` env catalog-source overrides.
    let catalog_root = crate::content::catalog_root_for(&fw.root);
    let manifest_sources =
        crate::core::manifest::ManifestSources::resolve(project_dir, &fw.root, &catalog_root);
    let manifest = crate::core::manifest::resolve_manifest(&manifest_sources);
    let plan = crate::core::manifest::HarnessPlan::from_manifest(&manifest, fw, &catalog_root);

    // Deploy composed agents — Claude Code reads `~/.claude/agents/` at startup.
    // The manifest's agent-set selection (include/exclude) restricts WHICH source
    // agents deploy; the default manifest selects all of them.
    let deploy = deploy_agents_filtered(&plan.agent_source, &fw.claude_agents_dir(), |name| {
        plan.agent_selected(name)
    })
    .map_err(|err| PrepError::Deploy(err.to_string()))?;

    // Deploy skill files — Claude Code reads `~/.claude/skills/` at startup.
    // Skills carry no inheritance, so this is a manifest-tracked content copy;
    // the manifest's skill-set selection restricts WHICH source skills deploy.
    let skill_deploy =
        deploy_skills_filtered(&plan.skill_source, &fw.claude_skills_dir(), |name| {
            plan.skill_selected(name)
        })
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

    // Resolve the EFFECTIVE output style, folding the manifest's default in as
    // the lowest precedence below the existing HR-4 sources. Precedence:
    // explicit `--style` flag > `[style] active` config key > manifest `[style]
    // active` > professional default. We compute this as a single `Option<&str>`
    // and pass it as the `explicit` argument to both the prompt-injection seam
    // and the settings.json resolver — when neither the flag nor the config sets
    // a style, the manifest's value applies; otherwise the higher source wins
    // exactly as before (zero regression for the flag/config paths). `config` was
    // loaded ONCE at the top of this function.
    let effective_style: Option<String> = explicit_style
        .map(str::to_owned)
        .or_else(|| config.style.active.clone())
        .or_else(|| plan.style.clone());

    // Stash the EXACT text the launch path passes to
    // `claude --append-system-prompt-file` — including the HR-4 output-style
    // injection — so `tm session instructions` shows what was actually used,
    // including any project-level overrides under `<project>/.trusty-mpm/`.
    //
    // This MUST go through the same `build_system_prompt_for_with_style` seam the
    // launcher uses (issue #1409): that seam resolves the override-layered prompt
    // via `resolve_pm_prompt` AND applies the output-style version-fallback
    // injection. Writing the bare `resolve_pm_prompt` text here (pre-injection)
    // while the launcher injects later made the stash diverge from reality
    // whenever `claude` was absent/old (native unsupported → injection fires),
    // breaking the stash/launch invariant in a host-dependent way. Routing both
    // through the single seam keeps them identical regardless of Claude version
    // (issue #381 / the #382 concern).
    let resolved_prompt = build_system_prompt_for_with_style_and_native(
        project_dir,
        effective_style.as_deref(),
        native_supported,
    );
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

    // Resolve the active output style for settings.json using the same
    // EFFECTIVE style computed above (HR-4 sources + the HR-2 manifest default).
    // An unknown id is logged and falls back to the professional default rather
    // than failing the launch (DOC-17). The resolved id is written into
    // `.claude/settings.json` so a native-capable Claude Code (>= 1.0.83) applies
    // it directly; older builds pick it up via prompt injection at the
    // `build_system_prompt_for` seam.
    let active_style_id = match crate::core::output_style::resolve_active_style(
        &config,
        effective_style.as_deref(),
    ) {
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

    // Inject `tm statusline` into the project's `.claude/settings.json` so
    // Claude Code shows live context in its status bar. Only sets the key when
    // absent (never clobbers the user's existing statusLine). Non-fatal.
    if let Err(err) = write_status_line(project_dir) {
        tracing::warn!("failed to write statusLine config: {err}");
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
    // `memory_store`, …). Gated by the manifest's `[mcp] trusty_memory` toggle
    // (default on). Non-fatal: the session still launches, it just lacks the
    // memory tools.
    if plan.inject_trusty_memory {
        // Pin the project's palace via `env.TRUSTY_MEMORY_PALACE` (issue #1605).
        // `repo_url` (the cloned-from URL, threaded from LaunchParams) is the
        // authoritative identity for repo_url-cloned sessions; the injector
        // falls back to the workspace's own git origin remote when it is `None`,
        // and to the bare stub when no slug can be derived (fail-open).
        if let Err(err) = inject_trusty_memory_mcp(project_dir, repo_url) {
            tracing::warn!("failed to inject trusty-memory MCP server: {err}");
        }
    } else {
        tracing::debug!("manifest disables trusty-memory MCP injection");
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
    // Gated by the manifest's `[mcp] trusty_search` toggle (default on).
    if plan.inject_trusty_search {
        let pinned_index = register_project_index(project_dir);
        if let Err(err) = inject_trusty_search_mcp(project_dir, pinned_index.as_deref()) {
            tracing::warn!("failed to inject trusty-search MCP server: {err}");
        }
    } else {
        tracing::debug!("manifest disables trusty-search MCP injection");
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
    // a missing style file just falls back to the operator's default. Uses
    // `fw.claude_home_dir()` (issue #1860) rather than `dirs::home_dir()`
    // directly so isolated `FrameworkPaths::under(tempdir)` callers (tests)
    // stay confined to the temp dir instead of leaking into the real `$HOME`.
    let output_style = match deploy_output_style(&fw.claude_home_dir()) {
        Ok(path) => Some(path),
        Err(err) => {
            tracing::warn!("failed to deploy trusty-mpm output style file: {err}");
            None
        }
    };

    // DOC-28 cutover bridge — auto-inject catch-up as seed context (#1762).
    // Fail-open: if catch-up fails for any reason (daemon not running, no git
    // repo, runtime error), the session still launches; catchup_context is None.
    // CUTOVER BRIDGE — remove post-migration (#1762)
    let catchup_context = if config.catchup.auto {
        let memory_url = std::env::var("TRUSTY_MEMORY_URL")
            .unwrap_or_else(|_| "http://127.0.0.1:7990".to_string());
        let opts = crate::core::catchup::CatchupOptions {
            project_dir: project_dir.to_path_buf(),
            memory_url,
            include_git: config.catchup.include_git,
            include_palace: config.catchup.include_palace,
            git_limit: config.catchup.git_limit,
            drawer_limit: config.catchup.drawer_limit,
            // Auto-inject always uses the watermark (incremental).
            full: false,
        };
        // Auto-inject advances the watermark so subsequent sessions are incremental.
        let ctx = crate::core::catchup::run_catchup_blocking(opts, true);
        if ctx.is_empty() { None } else { Some(ctx) }
    } else {
        None
    };

    Ok(PrepReport {
        deploy,
        skill_deploy,
        instructions,
        stash,
        output_style,
        hooks_written,
        catchup_context,
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
    let native = crate::core::output_style::claude_supports_native_output_style();
    build_system_prompt_for_with_style_and_native(project_dir, explicit_style, native)
}

/// Build the launch prompt with the `native_supported` output-style decision
/// supplied explicitly (no live `claude --version` probe).
///
/// Why: the public [`build_system_prompt_for_with_style`] probes
/// `claude --version`, so any test (or caller) that exercises the launch prompt
/// is silently coupled to whether `claude` is installed on the host — the
/// host-dependence that broke `prepare_session_stash_reflects_override` on CI
/// (issue #1409). This seam pins the decision so the stash/launch invariant can
/// be asserted deterministically under BOTH `native_supported = true` (no
/// injection) and `false` (injection fires). Production code keeps real
/// detection via the wrapper above.
/// What: resolves the override-layered PM prompt via
/// [`crate::core::instruction_overrides::resolve_pm_prompt`], then applies the
/// injection decision with the caller-supplied flag via
/// [`crate::core::output_style::apply_output_style_to_prompt_with_native`].
/// Test: `prepare_session_stash_reflects_override`.
pub fn build_system_prompt_for_with_style_and_native(
    project_dir: &Path,
    explicit_style: Option<&str>,
    native_supported: bool,
) -> String {
    let prompt = crate::core::instruction_overrides::resolve_pm_prompt(project_dir);
    crate::core::output_style::apply_output_style_to_prompt_with_native(
        project_dir,
        explicit_style,
        prompt,
        native_supported,
    )
}
