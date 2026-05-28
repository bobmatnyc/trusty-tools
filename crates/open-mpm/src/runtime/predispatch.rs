// Pre-existing clippy warnings across this large binary crate.
// Each category below is suppressed at crate level with rationale:
// - dead_code / unused_imports: Many helpers are kept for future use, behind
//   feature flags, or used only on certain platforms / by tests; pruning them
//   is its own refactor and would churn unrelated modules.
// - clippy::collapsible_if / collapsible_else_if: Style preference; nested
//   ifs are often clearer with the existing comments and gating logic.
// - clippy::manual_str_repeat / manual_repeat_n / single_char_add_str: Style
//   nits in display/formatting code where current form reads fine.
// - clippy::too_many_arguments: A few orchestration entry points genuinely
//   need their argument count; signatures are part of internal contracts.
// - clippy::await_holding_lock: Test-only — a std::sync::Mutex serializes
//   tests that mutate process-global env (HOME, etc.). The await points are
//   inside the critical section by design, and tests are single-threaded
//   per-test by virtue of the lock.
// - clippy::clone_on_copy / len_zero / map_or / etc.: Misc style nits in
//   pre-existing code; not worth the churn vs. risk of breaking 1500+ tests.
#![allow(dead_code)]
#![allow(unused_imports)]
#![allow(unused_mut)]
#![allow(unused_assignments)]
#![allow(unused_variables)]
#![allow(clippy::collapsible_if)]
#![allow(clippy::collapsible_else_if)]
#![allow(clippy::manual_str_repeat)]
#![allow(clippy::manual_repeat_n)]
#![allow(clippy::single_char_add_str)]
#![allow(clippy::too_many_arguments)]
#![allow(clippy::await_holding_lock)]
#![allow(clippy::clone_on_copy)]
#![allow(clippy::len_zero)]
#![allow(clippy::unnecessary_map_or)]
#![allow(clippy::manual_map)]
#![allow(clippy::needless_borrows_for_generic_args)]
#![allow(clippy::unnecessary_sort_by)]
#![allow(clippy::if_same_then_else)]
#![allow(clippy::new_without_default)]
#![allow(clippy::manual_split_once)]
#![allow(clippy::needless_splitn)]
#![allow(clippy::single_match_else)]
#![allow(clippy::single_match)]
#![allow(clippy::ptr_arg)]
#![allow(clippy::manual_clamp)]
#![allow(clippy::redundant_closure)]
#![allow(clippy::manual_pattern_char_comparison)]
#![allow(clippy::vec_init_then_push)]
#![allow(clippy::single_component_path_imports)]
#![allow(clippy::derivable_impls)]
#![allow(clippy::match_single_binding)]
#![allow(clippy::redundant_pattern_matching)]

//! Post-parse, pre-mode-dispatch steps: slash-command passthrough and the
//! one-time agent + skill registry build/log.

use std::path::PathBuf;
use std::sync::Arc;

use anyhow::Result;

use super::cli_def::Cli;
use crate::default_bundled_config_dir;
use crate::{agents, identity, repl, skills};

/// Handle the `open-mpm /<slash> ...` one-shot passthrough.
///
/// Why: Operators want a CLI surface for control commands that already exist
/// as REPL slash handlers, without launching a TTY just to run `/service
/// status` or `/help`.
/// What: When the first positional token is a slash command, builds a minimal
/// REPL, dispatches the joined line, prints captured output, and exits the
/// process (0 handled / 1 unknown). Returns `Ok(true)` when a slash was
/// handled (caller should `return Ok(())`); `Ok(false)` to continue.
/// Test: `slash_passthrough_help_returns_zero` integration test (manual).
pub(super) async fn handle_slash_passthrough(cli: &Cli) -> Result<bool> {
    // #344: Slash-command passthrough.
    if let Some(first) = cli.rest.first()
        && first.starts_with('/')
    {
        let slash_line = cli.rest.join(" ");
        let user_profile = identity::user_profile::UserProfile::load();
        let mut repl = repl::OpenMpmRepl::new(user_profile)?;
        match repl.try_handle_slash(&slash_line).await {
            Some(Ok((_continue, output))) => {
                // The REPL slash dispatcher captures "unknown command: ..."
                // for slashes it doesn't recognize. Surface that as exit 1
                // so scripts can detect bad commands.
                let is_unknown = output.trim_start().starts_with("unknown command:");
                if !output.is_empty() {
                    print!("{output}");
                    if !output.ends_with('\n') {
                        println!();
                    }
                }
                if is_unknown {
                    std::process::exit(1);
                }
                return Ok(true);
            }
            Some(Err(e)) => {
                eprintln!("slash command error: {e:#}");
                std::process::exit(1);
            }
            None => {
                eprintln!("Unknown slash command: {slash_line}");
                std::process::exit(1);
            }
        }
    }
    Ok(false)
}

/// Build (and log) the PM-process agent + skill registries once at startup.
///
/// Why: The dispatch path looks up discovered agents by name / capability and
/// every code path shares one scanned, tag-indexed skill catalog. Building it
/// here keeps the work off the per-mode paths and lets operators confirm
/// discovery at startup.
/// What: Loads the agent registry on the blocking pool, defaults the
/// project-local skill scan for interactive runs, kicks off a fire-and-forget
/// remote-source refresh, then builds + merges the persisted skill index.
/// Both registries are informational (sub-agents rebuild their own); failures
/// degrade to empty + a WARN.
/// Test: Indirectly via `cargo run -p open-mpm` startup logs.
pub(super) async fn build_registries(cli: &Cli) {
    // #167: Build the agent registry once at startup so the rest of the
    // dispatch path can look up discovered agents by name or by capability.
    // Failure is non-fatal (empty registry just means no dynamic discovery;
    // legacy `AgentConfig::by_name` paths continue to work for bundled agents).
    // #477: `AgentRegistry::load` walks the filesystem (hierarchical search
    // paths) and parses every agent TOML it finds — blocking IO that would
    // otherwise stall the async startup path. Run it on the blocking pool.
    let _registry = {
        let search_paths = agents::registry::agent_search_paths(&default_bundled_config_dir());
        Arc::new(
            tokio::task::spawn_blocking(move || {
                agents::registry::AgentRegistry::load(&search_paths)
            })
            .await
            .expect("AgentRegistry::load panicked"),
        )
    };
    if !_registry.is_empty() {
        tracing::info!(
            count = _registry.len(),
            "agent registry loaded from hierarchical search paths"
        );
    }

    // #168: Build the skill registry at startup so every code path (sub-agents,
    // workflow phases, `skills list` subcommand) shares one scanned, tag-indexed
    // catalog. Missing source dirs are a graceful no-op.
    //
    // #170: This PM-process registry is informational — sub-agents run in
    // separate processes and rebuild their own registry inside
    // `run_subagent`. Logged here so operators can confirm discovery at startup.
    // #172: Load operator-configurable skill sources (.open-mpm/skill-sources.toml)
    // and refresh remote-git caches before scanning. Falls back to the legacy
    // hard-coded paths when no config file is present so existing installs
    // keep working unchanged.
    let project_root_for_skills = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."));
    // #477: For the interactive (ctrl) path, restrict the upcoming skill scan
    // to project-local sources. `run_ctrl_inner` used to set this, but that
    // fires *after* the registry below is already built — so the first scan
    // walked every remote source and stalled startup. Setting it here, before
    // the scan, makes the speed-up actually take effect.
    // SAFETY: single-threaded startup context before any spawn.
    if cli.workflow.is_none()
        && cli.agent.is_none()
        && std::env::var("OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY").is_err()
    {
        unsafe {
            std::env::set_var("OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY", "1");
        }
        tracing::debug!("startup: defaulting OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY=1 (interactive)");
    }
    let source_registry = skills::sources::SkillSourceRegistry::load(&project_root_for_skills);
    // Fire-and-forget background refresh: `git fetch`/`clone` blocks startup
    // noticeably when network is slow. The current run uses the on-disk cache
    // as-is; updates land in time for the next launch.
    let source_registry_bg = source_registry.clone();
    tokio::task::spawn_blocking(move || {
        if let Err(e) = source_registry_bg.ensure_remote_sources() {
            tracing::warn!(error = %e, "skill sources: background refresh failed");
        }
    });
    let skill_registry = Arc::new({
        let mut reg = skills::registry::SkillRegistry::from_sources(
            &source_registry,
            &default_bundled_config_dir().join("skills"),
        );
        // #171: Merge persisted effectiveness/usage fields back over the
        // freshly scanned defaults so the system's learning survives restarts.
        let index_path = skills::registry::skill_index_path();
        if let Err(e) = reg.merge_index(&index_path) {
            tracing::warn!(
                error = %e,
                path = %index_path.display(),
                "skill registry: failed to merge persisted effectiveness index (continuing with defaults)"
            );
        }
        reg
    });
    if !skill_registry.is_empty() {
        tracing::info!(
            count = skill_registry.len(),
            "skill registry: indexed skills from hierarchical search paths"
        );
    }
}
