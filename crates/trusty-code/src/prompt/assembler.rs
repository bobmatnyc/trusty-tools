//! System-prompt assembler — composes the fixed-order parity prompt.
//!
//! Why: The cross-model harness must assemble a system prompt whose only
//! intentional per-model variation is the optional tool-use fallback guidance
//! (parity-spec §1, §2). Centralising assembly here guarantees the stable,
//! fixed section order the spec requires (§2) and the trusty-mpm-style
//! `\n\n---\n\n` separators (§2), so two runs of the same task differ only by
//! the model.
//! What: [`PromptAssembler`] and the free function [`assemble_system_prompt`],
//! which concatenate up to four sections — BASE preamble, agent prompt,
//! project/`CLAUDE.md` context, and fallback guidance — joining only the
//! non-empty ones with the section separator.
//! Test: `prompt::tests` covers stable order, idempotence, separator collapse,
//! empty-section skipping, and fallback-guidance ordering.

use crate::agents::AgentConfig;
use crate::prompt::preamble::BASE_PREAMBLE;

/// Section separator between assembled prompt blocks (parity-spec §2).
///
/// Why: The spec fixes the separator as the same Markdown horizontal rule that
/// trusty-mpm uses, for visual and diff consistency across the two harnesses.
/// What: The literal `"\n\n---\n\n"`.
/// Test: `prompt::tests::omitted_sections_produce_no_double_separator` depends
/// on this exact value.
const SECTION_SEPARATOR: &str = "\n\n---\n\n";

/// Assembles the parity system prompt from its constituent sections.
///
/// Why: A named type gives call sites (the agent loop, the parity report) a
/// single, discoverable entry point and room to grow — e.g. to also surface the
/// per-section hashes the parity report records (spec D5) — without changing the
/// free-function signature callers already use.
/// What: A zero-sized struct whose [`PromptAssembler::assemble`] method forwards
/// to [`assemble_system_prompt`]. Holds no state, honouring the crate's
/// no-global-state convention.
/// Test: `prompt::tests::assembler_struct_matches_free_function`.
#[derive(Debug, Clone, Copy, Default)]
pub struct PromptAssembler;

impl PromptAssembler {
    /// Assemble the system prompt for the given agent.
    ///
    /// Why: Provides the OO-style entry point that mirrors the free function so
    /// callers may hold an assembler instance if they prefer.
    /// What: Delegates verbatim to [`assemble_system_prompt`].
    /// Test: `prompt::tests::assembler_struct_matches_free_function`.
    pub fn assemble(
        &self,
        config: &AgentConfig,
        project_context: Option<&str>,
        fallback_guidance: Option<&str>,
    ) -> String {
        assemble_system_prompt(config, project_context, fallback_guidance)
    }
}

/// Assemble the parity system prompt in the spec's stable, fixed order.
///
/// Why: The model-comparison harness (parity-spec §1) requires that, for a
/// given agent and task, every byte of the assembled prompt except the
/// documented fallback exception is identical across models. A single
/// deterministic assembler with a fixed section order is how that guarantee is
/// realised.
/// What: Concatenates, in this order (spec §2): (1) [`BASE_PREAMBLE`],
/// (2) `config.system_prompt.content` if non-empty, (3) `project_context`
/// (verbatim `CLAUDE.md`, spec §2c/D1) if `Some` and non-empty, (4)
/// `fallback_guidance` (the §4 per-tier exception, supplied by #1023) if `Some`
/// and non-empty. Sections are joined by [`SECTION_SEPARATOR`]; empty sections
/// contribute neither text nor a separator, so the common native-tool case is
/// exactly sections 1→2→3 (spec D4) with no dangling rules.
/// Test: `prompt::tests::assembled_order_is_stable`,
/// `base_identical_across_calls`,
/// `omitted_sections_produce_no_double_separator`,
/// `empty_agent_prompt_skipped`, `fallback_guidance_is_last`.
pub fn assemble_system_prompt(
    config: &AgentConfig,
    project_context: Option<&str>,
    fallback_guidance: Option<&str>,
) -> String {
    // Collect the sections in fixed order, dropping any that are empty so the
    // separator never doubles up (spec §2c: "the separator collapses").
    let agent_prompt = config.system_prompt.content.trim();
    let project = project_context.map(str::trim).unwrap_or("");
    let fallback = fallback_guidance.map(str::trim).unwrap_or("");

    let mut sections: Vec<&str> = Vec::with_capacity(4);
    sections.push(BASE_PREAMBLE);
    if !agent_prompt.is_empty() {
        sections.push(agent_prompt);
    }
    if !project.is_empty() {
        sections.push(project);
    }
    if !fallback.is_empty() {
        sections.push(fallback);
    }

    sections.join(SECTION_SEPARATOR)
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
