//! The `prompt-self-improvement` flag and the PM addendum it injects (#7688).
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
//! What: [`enabled_for`] resolves the flag for one project and [`pm_addendum`]
//! is the text, held as reviewable markdown beside the instruction sections
//! rather than as a string literal. The PM text asks the PM to append the same
//! request to every dispatch brief it sends, which is how a dispatched agent
//! receives it — there is deliberately no second injection at agent deploy. The
//! deployed agent files are one machine-global set shared by every project on
//! the host (#4409), so a per-project flag written into them has two projects
//! rewriting each other's copies on alternate launches; the brief is per
//! dispatch and carries the flag correctly.
//!
//! DEFAULT OFF. The addendum is appended, never substituted, so with the flag
//! off every byte tm composes is what it was before this module existed — the
//! property `pm_prompt_is_unchanged_when_the_flag_is_off` pins.
//!
//! Test: `prompt_self_improvement_tests.rs`.

use std::path::Path;

/// The `## Prompt feedback` heading both addenda name and the hook extracts.
///
/// Why: the injected text, the ledger extractor, and the read-back CLI must
/// agree on one spelling, or the hook silently harvests nothing from a session
/// that was correctly asked. One constant, three readers.
/// What: the Markdown heading, without a trailing newline.
/// Test: `the_addendum_names_the_extraction_heading`.
pub const FEEDBACK_HEADING: &str = "## Prompt feedback";

/// The PM-side addendum, appended to the composed launch prompt.
///
/// Why: held as an asset file so the delivered wording is reviewed as prose in
/// a PR diff, the same rule the instruction sections follow.
///
/// ONE SHORT PARAGRAPH, deliberately. The delivered text says what to emit and
/// to pass the request down, and nothing else: it is resident in every composed
/// prompt on every turn, so a sentence that only justifies the feature is paid
/// for forever. The justification is the module header above — the addendum is
/// about the PROMPT, not the work, and it complements rather than replaces the
/// `Improvement recommendations` block (#6935), which reports on the framework.
/// A reader of the prompt never needed that distinction; a reader of this file
/// does.
/// Test: `the_addendum_names_the_extraction_heading`, `the_addendum_is_short`.
const PM_ADDENDUM: &str =
    include_str!("../assets/instructions/sections/prompt-self-improvement.md");

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

/// Is prompt self-improvement on for `project_dir`?
///
/// Why: one resolver, so the PM prompt and the hook matcher can never disagree
/// about whether the feature is on — a session told to emit the addendum but
/// whose hook was not registered would produce feedback nobody collects, and
/// the reverse would register a hook that never fires.
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
