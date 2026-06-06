//! Per-phase prompt assembly for the workflow engine.
//!
//! Why: Before each agent runs, the engine layers several context sources onto
//! the rendered template — QA-failure feedback, a claude-code path hint, the
//! project self-init prefix, pre-plan skill summaries, skills-first / legacy
//! skill injection, the parsed goal block, and the user-memory suffix. Pulling
//! that ~170-line assembly out of the phase loop (#359) keeps `run` readable
//! while preserving the exact ordering and gating each layer relies on.
//! What: `WorkflowEngine::assemble_phase_prompt` renders `phase.context_template`
//! against the context, then applies every injection in priority order and
//! returns the final prompt string.
//! Test: Covered end-to-end via the engine `tests` submodule
//! (`init_context_is_prepended_to_phase_template`,
//! `plan_agent_context_includes_skill_summaries`,
//! `qa_receives_correct_path_for_claude_code_runner`).

use crate::workflow::config::PhaseDef;
use crate::workflow::context::WorkflowContext;

use super::{DiscoveredSkill, WorkflowEngine};

impl WorkflowEngine {
    /// Build the fully-assembled prompt for a single phase.
    ///
    /// Why: Centralizes the layered context injection so the phase loop only
    /// needs one call. Each layer is gated exactly as it was inline, so the
    /// emitted prompt is byte-for-byte identical to the pre-split engine.
    /// What: Renders the phase template, then (in order) prepends QA feedback
    /// for a retried `code` phase, a claude-code path hint for the `qa` phase,
    /// the project self-init prefix, the `## Available Skills` block for the
    /// `plan` phase, skills-loader / legacy skill bodies, the goal block, and
    /// finally appends the user-memory suffix. Records used skills into `perf`.
    /// Test: See the module-level doc — covered by the engine `tests` submodule.
    #[allow(clippy::too_many_arguments)]
    pub(super) async fn assemble_phase_prompt(
        &self,
        phase: &PhaseDef,
        ctx: &WorkflowContext,
        out_dir: &Option<std::path::PathBuf>,
        code_dir: &Option<std::path::PathBuf>,
        discovered_skills: &[DiscoveredSkill],
        code_phase_used_claude_code: bool,
        qa_failure_feedback: &mut Option<String>,
        perf: &mut crate::perf::PerfCollector,
    ) -> String {
        let mut rendered = ctx.render_template(&phase.context_template);

        // Fix 2 (claude-mpm parity): If the previous QA phase emitted a
        // structured `status: "fail"` envelope and we have not yet
        // exhausted the retry cap, prepend the QA feedback block to the
        // next code-phase render so the engineer agent can address the
        // failure. Consume (take) the feedback so it only fires once per
        // failure.
        if phase.name == "code"
            && let Some(feedback) = qa_failure_feedback.take()
        {
            tracing::info!(
                "code phase: prepending QA feedback to engineer prompt (one-shot retry)"
            );
            rendered = format!("{feedback}{rendered}");
        }

        // #123: When the code phase ran under a `claude-code` runner,
        // prepend a path-search hint to the QA prompt so QA knows to
        // also look at the git repository root if files are missing
        // from `out_dir` / `project_dir`. The post-code reconcile step
        // already moved files listed in `assignments.json`, but this
        // catches anything not declared (e.g. ad-hoc test fixtures).
        if phase.name == "qa" && code_phase_used_claude_code {
            let project_root = std::env::current_dir()
                .map(|p| p.display().to_string())
                .unwrap_or_default();
            let hint = format!(
                "## Note: claude-code runner was used for the code phase\n\n\
                 If the project files are not visible at the expected location, \
                 also check the project root: `{project_root}`. The claude CLI \
                 occasionally writes relative paths against the git repository \
                 root instead of the workflow `out_dir`. Files matching the \
                 assignments plan have already been relocated, but ad-hoc \
                 fixtures may still live at `{project_root}`.\n\n---\n\n"
            );
            rendered = format!("{hint}{rendered}");
        }

        // #108/#109: Prepend the project self-init prefix (project index
        // summary) to every phase's task. Empty when init produced no
        // content, so this is a no-op otherwise.
        //
        // Relevance-first context injection: filter the project_summary
        // bullet list by keyword overlap with the user task before
        // injection so we don't burn tokens on irrelevant index entries
        // for narrowly-scoped tasks. Falls back to first-N entries when
        // no keywords match anything (preserves prior wholesale-injection
        // semantics as a safety net).
        if let Some(init) = &self.init_context {
            let mut filtered_init = init.clone();
            filtered_init.project_summary = crate::agents::context_filter::filter_index_entries(
                &init.project_summary,
                &ctx.task,
                15,
            );
            let prefix = filtered_init.to_prompt_prefix();
            if !prefix.is_empty() {
                rendered = format!("{prefix}{rendered}");
            }
        }

        // #173: For the `plan` phase, prepend a `## Available Skills`
        // summary block listing every skill the engine matched against
        // the task signals. Replaces the manual `list_skills` tool call
        // the plan-agent used to make — the planner now sees every
        // candidate skill (with a one-line summary + tags) up front so
        // it can reason about ecosystem compatibility before writing
        // `assignments.json`. No-op when discovery returned nothing or
        // the phase isn't `plan`.
        if phase.name == "plan" && !discovered_skills.is_empty() {
            let mut block = String::from("## Available Skills (auto-matched for this task)\n\n");
            for skill in discovered_skills {
                let tag_csv = skill.tags.join(", ");
                block.push_str(&format!(
                    "- **{}** [tags: {}] — {}\n",
                    skill.name, tag_csv, skill.summary
                ));
            }
            block.push_str(
                "\nSkills have been pre-loaded above. Use them. \
                 Call `load_skill(name=\"<name>\")` only if you need the full body.\n",
            );
            tracing::debug!(
                phase = %phase.name,
                count = discovered_skills.len(),
                "pre-plan skill discovery injected into plan-agent prompt"
            );
            rendered = format!("{block}\n---\n\n{rendered}");
        }

        // Skills-first: If a SkillsLoader is attached, use it to detect and inject
        // language/framework skills based on the project directory and task text.
        // This supersedes the legacy skill_registry auto_inject path when both are present.
        if let Some(loader) = self.skills_loader.as_ref() {
            // #233: Scope language/framework skill detection to the task's
            // own output directory (where the agent's source files land),
            // not the harness's CWD. Using `current_dir()` resolved to the
            // trusty-agents repo root, which contains `Cargo.toml`, causing the
            // "rust" skill to be injected into every task regardless of
            // the task's actual language. Prefer `code_dir`, fall back to
            // `out_dir`, and only use CWD if neither is configured (the
            // legacy in-process test path).
            let project_dir = code_dir
                .clone()
                .or_else(|| out_dir.clone())
                .unwrap_or_else(|| {
                    std::env::current_dir().unwrap_or_else(|_| std::path::PathBuf::from("."))
                });
            let explicit_skills: Vec<String> = phase.skills.as_deref().unwrap_or(&[]).to_vec();
            // #171: Use the tracked variant so the perf collector can
            // record exactly which skills were injected for post-run
            // effectiveness/usage updates.
            let (skills_prefix, used_names) = loader
                .build_skills_prefix_tracked(&explicit_skills, &project_dir, &rendered)
                .await;
            if !skills_prefix.is_empty() {
                tracing::debug!(
                    phase = %phase.name,
                    skills_chars = skills_prefix.len(),
                    skills = ?used_names,
                    "skills-loader injected skills into phase task"
                );
                for name in &used_names {
                    perf.record_skill_used(name);
                }
                rendered = format!("{skills_prefix}\n\n---\n\n{rendered}");
            }
        }

        // #81: Auto-inject relevant skill bodies as a prompt prefix so the
        // agent has domain knowledge without having to discover it via a
        // tool call. Gracefully empty when the registry has no matches.
        if let Some(reg) = self.skill_registry.as_ref()
            && !reg.is_empty()
        {
            let skills_prefix = reg.auto_inject(&rendered, self.max_auto_skills).await;
            if !skills_prefix.is_empty() {
                tracing::debug!(
                    phase = %phase.name,
                    skills_chars = skills_prefix.len(),
                    "auto-injected skills into phase task"
                );
                rendered = format!("{skills_prefix}\n\n---\n\n{rendered}");
            }
        }

        // #68: If we have a parsed goal block (emitted by the planner),
        // prepend its header to the task text for every downstream phase.
        // This keeps the goals anchored at the top of each agent's input
        // without requiring IPC protocol changes.
        if let Some(goal) = &ctx.goal_block
            && !goal.is_empty()
        {
            rendered = format!("{}\n\n---\n\n{}", goal.to_prompt_header(), rendered);
        }

        // #118: Append user-scoped memory at the lowest priority (after
        // project context, skills, and goal block). This ensures
        // cross-project learnings from ~/.trusty-agents/memory/ enrich the
        // prompt without displacing higher-priority project signals.
        if let Some(suffix) = &self.user_memory_suffix
            && !suffix.is_empty()
        {
            rendered.push_str(suffix);
        }

        rendered
    }
}
