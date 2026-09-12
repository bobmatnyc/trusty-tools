//! The `prompt-self-improvement` flag and the two addenda it injects (#7688).
//!
//! Why: nothing in the harness asks the model what was wrong with the prompt it
//! received. Every improvement signal tm collects today is about the WORK — the
//! `Improvement recommendations` block (#6935) reports framework and tooling
//! gaps, and the self-improvement hypotheses (#6937) measure an agent's own
//! approach. The prompt itself, which is the one artifact tm composes and
//! therefore the one it can actually fix, goes unexamined. The owner ruling
//! (2026-09-12) adds a flag that makes every composed prompt ask for that
//! feedback on two axes: what was unclear, and what was unnecessary.
//!
//! What: [`enabled_for`] resolves the flag for one project; [`pm_addendum`] and
//! [`agent_addendum`] are the two texts, held as reviewable markdown beside the
//! instruction sections rather than as string literals. The PM text asks the PM
//! to pass the same request down its dispatch briefs; the agent text does not,
//! because an agent never dispatches ("No Subagent Fan-Out").
//!
//! DEFAULT OFF. Both addenda are appended, never substituted, so with the flag
//! off every byte tm composes is what it was before this module existed — the
//! property `pm_prompt_is_unchanged_when_the_flag_is_off` and
//! `agent_bytes_are_unchanged_when_the_flag_is_off` pin.
//!
//! Test: `prompt_self_improvement_tests.rs`.

use std::path::Path;

/// The `## Prompt feedback` heading both addenda name and the hook extracts.
///
/// Why: the injected text, the ledger extractor, and the read-back CLI must
/// agree on one spelling, or the hook silently harvests nothing from a session
/// that was correctly asked. One constant, three readers.
/// What: the Markdown heading, without a trailing newline.
/// Test: `both_addenda_name_the_extraction_heading`.
pub const FEEDBACK_HEADING: &str = "## Prompt feedback";

/// The PM-side addendum, appended to the composed launch prompt.
///
/// Why: held as an asset file so the delivered wording is reviewed as prose in
/// a PR diff, the same rule the instruction sections follow.
/// Test: `both_addenda_name_the_extraction_heading`.
const PM_ADDENDUM: &str =
    include_str!("../assets/instructions/sections/prompt-self-improvement.md");

/// The agent-side addendum, appended to every composed agent at deploy.
///
/// Why: see [`PM_ADDENDUM`]. Distinct text because an agent must not be told to
/// "append this to every dispatch brief you send" — it sends none.
/// Test: `agent_addendum_does_not_ask_an_agent_to_dispatch`.
const AGENT_ADDENDUM: &str =
    include_str!("../assets/instructions/sections/prompt-self-improvement-agent.md");

/// The PM addendum, separator included, ready to concatenate onto a prompt.
///
/// What: [`SECTION_SEPARATOR`](crate::core::instruction_pipeline::SECTION_SEPARATOR)
/// then the asset, so the appended block reads as one more section rather than
/// running into the previous one.
/// Test: `pm_addendum_is_separated_from_the_prompt_it_follows`.
pub fn pm_addendum() -> String {
    format!(
        "{}{}",
        crate::core::instruction_pipeline::SECTION_SEPARATOR,
        PM_ADDENDUM.trim_end()
    )
}

/// The agent addendum, separator included, ready to concatenate onto an agent.
///
/// What: a Markdown rule then the asset. A composed agent file ends with its
/// body, so the same separator the PM prompt uses keeps the two injections
/// visually identical for anyone reading either artifact.
/// Test: `agent_addendum_is_separated_from_the_body_it_follows`.
pub fn agent_addendum() -> String {
    format!(
        "{}{}\n",
        crate::core::instruction_pipeline::SECTION_SEPARATOR,
        AGENT_ADDENDUM.trim_end()
    )
}

/// Append the PM addendum to `prompt` when the flag is on for `project_dir`.
///
/// Why: the injection RULE lives here rather than at the launch composer that
/// calls it, so both composer seams state the decision once and neither can
/// drift into its own idea of when the addendum applies. It also keeps
/// `session_launch::mod` under the 500-SLOC production cap.
///
/// This is deliberately one layer ABOVE
/// [`crate::core::instruction_overrides::resolve_pm_prompt`]: putting it in the
/// resolver would make the committed PM-prompt goldens depend on the operator's
/// own `~/.trusty-mpm/config.toml`, so a machine with the flag on would fail
/// `pm_prompt_golden_tests` for a reason unrelated to the change under test.
/// Appending at the launch seam keeps the goldens a pure function of the
/// section sources and still reaches every delivered prompt — the compiled
/// prompt, `tm launch`, and `/connect` all compose through a caller of this.
/// What: `prompt` unchanged when the flag is off; otherwise `prompt` plus
/// [`pm_addendum`].
/// Test: `pm_prompt_is_unchanged_when_the_flag_is_off`,
/// `pm_prompt_carries_the_addendum_when_the_flag_is_on`.
pub fn append_to_pm_prompt(project_dir: &Path, prompt: String) -> String {
    if enabled_for(project_dir) {
        format!("{prompt}{}", pm_addendum())
    } else {
        prompt
    }
}

/// The agent-deploy suffix for `project_dir`, or `None` when the flag is off.
///
/// Why: the deploy call sites pass `Option<&str>` to
/// [`deploy_agents_filtered_with_suffix`](crate::core::agent_deployer::deploy_agents_filtered_with_suffix),
/// and `None` is the byte-identical pre-#7688 path. One resolver keeps the
/// launch and the `sync-assets` redeploy from disagreeing — a disagreement
/// would show up as `tm sessions sync-assets` silently stripping the addendum
/// back off every agent it refreshed.
/// What: `Some(`[`agent_addendum`]`)` when on, `None` when off.
/// Test: `agent_suffix_is_none_when_the_flag_is_off`,
/// `agent_suffix_is_the_addendum_when_the_flag_is_on`.
pub fn agent_deploy_suffix(project_dir: &Path) -> Option<String> {
    enabled_for(project_dir).then(agent_addendum)
}

/// Is prompt self-improvement on for `project_dir`?
///
/// Why: one resolver, so the PM prompt, the deployed agents, and the hook
/// matcher can never disagree about whether the feature is on — a session told
/// to emit the addendum but whose hook was not registered would produce
/// feedback nobody collects, and the reverse would register a hook that never
/// fires.
///
/// What: the same two-layer precedence every other project-overridable key
/// uses, top down —
///
/// 1. `<project_dir>/.trusty-mpm.toml`'s `prompt_self_improvement`
///    ([`ProjectLevelConfig`](crate::core::project_config::ProjectLevelConfig)),
///    committed and therefore the project's own answer;
/// 2. `~/.trusty-mpm/config.toml`'s `[pm] prompt_self_improvement`
///    ([`PmConfig`](crate::core::config::PmConfig)), the operator's host default;
/// 3. `false`.
///
/// A malformed project config contributes nothing rather than aborting, exactly
/// as [`load_or_report`](crate::core::project_config::load_or_report) defines.
/// Test: `disabled_by_default`, `host_config_enables_it`,
/// `project_config_true_overrides_host_false`,
/// `project_config_false_overrides_host_true`.
pub fn enabled_for(project_dir: &Path) -> bool {
    enabled_for_with_host(
        project_dir,
        crate::core::config::MpmConfig::load_default()
            .pm
            .prompt_self_improvement,
    )
}

/// [`enabled_for`] with the host-level answer supplied by the caller.
///
/// Why (#5544): [`enabled_for`] reads `~/.trusty-mpm/config.toml` through the
/// process home directory, so a test asserting the precedence would have to
/// write the operator's real config or mutate the PROCESS-GLOBAL `$HOME` — the
/// flake class #5544 tracks, and one `cargo nextest`'s per-test processes make
/// worse rather than better. Naming the host layer removes the read entirely.
/// What: the project layer as [`enabled_for`] reads it, falling through to
/// `host` and then `false`. Production calls the wrapper above.
/// Test: `project_config_true_overrides_host_false`,
/// `project_config_false_overrides_host_true`, `host_config_enables_it`.
pub fn enabled_for_with_host(project_dir: &Path, host: Option<bool>) -> bool {
    crate::core::project_config::load_or_report(project_dir)
        .and_then(|project| project.prompt_self_improvement)
        .or(host)
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "prompt_self_improvement_tests.rs"]
mod tests;
