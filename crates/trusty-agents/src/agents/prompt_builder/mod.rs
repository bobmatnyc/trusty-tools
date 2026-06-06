//! Layered system prompt assembly with CLAUDE.md ancestor walk.
//!
//! Why: An agent's effective system prompt is the sum of several sources:
//! (1) the base TOML `system_prompt.content`, (2) project-level CLAUDE.md
//! files walked from the working directory up to the filesystem root and
//! `$HOME/.claude/CLAUDE.md`, (3) user memory (cross-project preferences and
//! learnings), (4) resolved skills, and (5) any sub-agent context injected by
//! the orchestrator. Concatenating these inline across call sites leads to
//! drift; a builder enforces a single canonical layout.
//! What: `SystemPromptBuilder` accumulates layers in well-defined slots and
//! `build()` concatenates them with a stable separator (`"\n\n---\n\n"`).
//! Test: Unit tests verify the ancestor walk finds CLAUDE.md files, that
//! home-level instructions come before project-level, and that `layer_count`
//! and `build()` produce the expected output (see `tests`).

use std::path::{Path, PathBuf};

#[cfg(test)]
mod harness_tests;
#[cfg(test)]
mod tests;

/// Separator placed between non-project-instruction layers. Markdown horizontal
/// rule keeps boundaries readable when the composed prompt is echoed to logs.
pub const LAYER_SEPARATOR: &str = "\n\n---\n\n";

// INTENT: Build a labeled separator naming the source file of the following layer.
pub fn labeled_separator(path: &Path) -> String {
    format!("\n\n--- [from: {}] ---\n\n", path.display())
}

// Harness protocol content is compiled into the binary via
// `crate::agents::harness_protocol`. The previous file-loading helpers
// (`harness_dir` / `load_harness_layer` / `load_harness_layer_from`) that
// read from `config/harness/*.md` were removed because harness protocol is
// binary-level behavior — not user config. See `harness_protocol.rs`.

/// Builder for a layered system prompt.
///
/// Slot ordering (build output):
///   goal_block → harness_layers → base → project_layers → user_memory_layer → skill_layers → subagent_layers
///
/// User memory sits after project instructions so project-specific knowledge
/// takes precedence, but before skills so that user preferences can influence
/// how skill guidance is interpreted by the model.
///
/// Harness layers sit between the goal block and the base (TOML) prompt so
/// that runtime-protocol rules — how to call `write_file`, when to call
/// `finish_task`, the absolute `out_dir` requirement, the `## Summary`
/// footer — are established BEFORE the agent's role-specific prompt. These
/// are sourced from `crate::agents::harness_protocol` constants (compiled
/// into the binary) and injected by the engine based on the agent's
/// runner/finish_task configuration, replacing ten copy-pasted sections
/// that used to live in individual agent TOMLs.
pub struct SystemPromptBuilder {
    base: String,
    harness_layers: Vec<String>,
    project_layers: Vec<(PathBuf, String)>,
    user_memory_layer: Option<String>,
    skill_layers: Vec<String>,
    subagent_layers: Vec<String>,
    /// Goal block prepended above the base prompt (#68). When `Some` and
    /// non-empty, rendered via `GoalBlock::to_prompt_header` followed by the
    /// standard `LAYER_SEPARATOR` before the base.
    goal_block: Option<crate::context::GoalBlock>,
    /// Project-memory layer (#241). Rendered between harness layers and base
    /// so the agent sees prior project context as preamble before its
    /// role-specific prompt. Distinct from `user_memory_layer`, which holds
    /// cross-project user preferences and sits *after* project instructions.
    memory_layer: Option<String>,
    /// MCP tool listing (#241). Rendered after skills so the model first reads
    /// its role + skills then learns about the external MCP services it can
    /// call. Role-gated by the caller (see `crate::mcp::McpConfig`).
    mcp_layer: Option<String>,
    /// Caveman-style output-compression instruction (#420). When set to a
    /// non-`None` style, the rendered fragment is appended as the final layer
    /// so the rules govern every subsequent response without being diluted
    /// by intervening content.
    output_style: crate::compress::OutputStyle,
}

impl SystemPromptBuilder {
    // INTENT: Seed the builder with the agent's configured base prompt.
    pub fn new(base: impl Into<String>) -> Self {
        // Inject runtime context variables into the system prompt (#367).
        // {{TAGENT_VERSION}} → current binary version from Cargo.toml so
        // agents like ctrl can answer "what version?" without needing a tool.
        let base = base
            .into()
            .replace("{{TAGENT_VERSION}}", env!("CARGO_PKG_VERSION"));
        Self {
            base,
            harness_layers: Vec::new(),
            project_layers: Vec::new(),
            user_memory_layer: None,
            skill_layers: Vec::new(),
            subagent_layers: Vec::new(),
            goal_block: None,
            memory_layer: None,
            mcp_layer: None,
            output_style: crate::compress::OutputStyle::None,
        }
    }

    /// Substitute the agent-identity template variables `{{AGENT_MODEL}}` and
    /// `{{AGENT_RUNNER}}` in the base prompt (#478).
    ///
    /// Why: `{{TAGENT_VERSION}}` is substituted in `new()` so agents can
    /// answer "what version?", but agents had no way to learn their own model
    /// or runner — ctrl in particular skips the deployment footer entirely, so
    /// it could not answer "what model am I?". These placeholders let any
    /// agent's TOML embed its model/runner directly in the identity line.
    /// What: Replaces every `{{AGENT_MODEL}}` and `{{AGENT_RUNNER}}` occurrence
    /// in the already-seeded `base` string. Chained after `new()` so the
    /// constructor signature stays non-breaking for the ~30 existing callers.
    /// Test: `agent_model_substituted_in_prompt`,
    /// `agent_runner_substituted_in_prompt`.
    pub fn with_agent_context(mut self, model: &str, runner: &str) -> Self {
        self.base = self
            .base
            .replace("{{AGENT_MODEL}}", model)
            .replace("{{AGENT_RUNNER}}", runner);
        self
    }

    /// Set the caveman-style output-compression instruction (#420).
    ///
    /// Why: Sub-agent output tokens are the largest single cost lever in a
    /// multi-turn session. A short rule block at the end of the system prompt
    /// reduces output verbosity 22–87% with no accuracy loss.
    /// What: Stores the style; `build()` appends the matching fragment as the
    /// final layer. Default is `OutputStyle::None` (no fragment appended).
    /// Test: `output_style_full_appended_at_end`,
    /// `output_style_none_appends_nothing`.
    pub fn with_output_style(mut self, style: crate::compress::OutputStyle) -> Self {
        self.output_style = style;
        self
    }

    // INTENT: Append a harness-protocol layer (write_file / finish_task /
    // out_dir / summary rules) that applies to every agent of the matching
    // runner. Empty content is dropped. Harness layers are injected between
    // the goal block and the base (TOML) prompt at build time.
    pub fn add_harness_layer(mut self, content: impl Into<String>) -> Self {
        let text = content.into();
        if !text.trim().is_empty() {
            self.harness_layers.push(text);
        }
        self
    }

    // INTENT: Attach a goal block that will be prepended to the built prompt.
    #[allow(dead_code)]
    pub fn with_goal_block(mut self, block: Option<crate::context::GoalBlock>) -> Self {
        self.goal_block = block.filter(|b| !b.is_empty());
        self
    }

    // INTENT: Inject user memory (cross-project preferences) after project layers but before skills.
    #[allow(dead_code)]
    pub fn with_user_memory(mut self, content: impl Into<String>) -> Self {
        let text = content.into();
        if !text.trim().is_empty() {
            self.user_memory_layer = Some(text);
        }
        self
    }

    // INTENT: Walk ancestor directories collecting CLAUDE.md and AGENTS.md project instruction files.
    pub fn walk_project_instructions(mut self, start: &Path) -> Self {
        // Home-level instructions go first (outermost scope).
        if let Some(home) = std::env::var_os("HOME") {
            let home_dir = PathBuf::from(home).join(".claude");
            for fname in ["CLAUDE.md", "AGENTS.md"] {
                let p = home_dir.join(fname);
                if p.exists() {
                    self.push_if_nonempty(p);
                }
            }
        }

        // Collect ancestor dirs leaf-to-root, then reverse so root-first.
        let mut ancestors: Vec<PathBuf> = Vec::new();
        let mut cur: Option<&Path> = Some(start);
        while let Some(c) = cur {
            ancestors.push(c.to_path_buf());
            cur = c.parent();
        }
        // Reverse so shallowest (filesystem root) comes first — #31.
        ancestors.reverse();

        for dir in ancestors {
            for fname in ["CLAUDE.md", "AGENTS.md"] {
                let p = dir.join(fname);
                if p.exists() {
                    self.push_if_nonempty(p);
                }
            }
        }

        self
    }

    // INTENT: Read a candidate instruction file and push it as a project layer if non-empty.
    fn push_if_nonempty(&mut self, path: PathBuf) {
        match std::fs::read_to_string(&path) {
            Ok(text) if !text.trim().is_empty() => {
                self.project_layers.push((path, text));
            }
            Ok(_) => {}
            Err(e) => {
                tracing::debug!(path = %path.display(), error = %e, "skipping instruction file: read failed");
            }
        }
    }

    // INTENT: Append one resolved skill's content as its own layer.
    pub fn add_skill(mut self, content: impl Into<String>) -> Self {
        self.skill_layers.push(content.into());
        self
    }

    /// Append an MCP-tool description block as its own layer (#241).
    ///
    /// Why: Coordinating agents (ctrl, PM, research, observe) need to know
    /// which external MCP services they can call. The block is rendered
    /// once by `crate::mcp::McpConfig::render_prompt_section` and injected
    /// here so prompt assembly stays in one place.
    /// What: Stores the rendered Markdown; empty/whitespace input is dropped.
    /// Test: `mcp_layer_appears_after_skills_before_subagent`.
    pub fn add_mcp_layer(mut self, section: impl Into<String>) -> Self {
        let text = section.into();
        if !text.trim().is_empty() {
            self.mcp_layer = Some(text);
        }
        self
    }

    /// Append a project-memory layer with recalled context strings (#241).
    ///
    /// Why: ctrl seeds prompts with relevant memories from kuzu-memory so
    /// the model is grounded in prior project context (decisions, conventions)
    /// without the user re-stating them. Rendered between harness layers and
    /// the base so the agent reads memory as preamble.
    /// What: Formats the input list as a bulleted Markdown section. An empty
    /// input vec is a no-op (no section is added).
    /// Test: `memory_layer_empty_vec_is_noop`,
    /// `memory_layer_renders_bullet_list`.
    pub fn add_memory_layer(mut self, memories: Vec<String>) -> Self {
        let cleaned: Vec<String> = memories
            .into_iter()
            .map(|m| m.trim().to_string())
            .filter(|m| !m.is_empty())
            .collect();
        if cleaned.is_empty() {
            return self;
        }
        let mut section = String::from("## Project Memory\n\n");
        for m in cleaned {
            // Indent multi-line memories so the bullet hierarchy stays clean.
            let indented = m.replace('\n', "\n  ");
            section.push_str(&format!("- {indented}\n"));
        }
        self.memory_layer = Some(section.trim_end().to_string());
        self
    }

    // INTENT: Append one sub-agent-provided context block as its own layer.
    #[allow(dead_code)]
    pub fn add_subagent_context(mut self, content: impl Into<String>) -> Self {
        self.subagent_layers.push(content.into());
        self
    }

    // INTENT: Count all non-empty layers including base, goal, user memory, project, skill, and subagent.
    #[allow(dead_code)]
    pub fn layer_count(&self) -> usize {
        let mut n = 0;
        if self.goal_block.as_ref().is_some_and(|g| !g.is_empty()) {
            n += 1;
        }
        n += self.harness_layers.len();
        if self.memory_layer.is_some() {
            n += 1;
        }
        if !self.base.trim().is_empty() {
            n += 1;
        }
        n += self.project_layers.len();
        if self.user_memory_layer.is_some() {
            n += 1;
        }
        n += self.skill_layers.len();
        if self.mcp_layer.is_some() {
            n += 1;
        }
        n += self.subagent_layers.len();
        if crate::compress::output_compression_prompt(self.output_style).is_some() {
            n += 1;
        }
        n
    }

    // INTENT: Concatenate all layers in canonical order with labeled/generic separators.
    pub fn build(self) -> String {
        let mut out = String::new();

        // #68: Goal block comes FIRST so the model can't miss it.
        if let Some(goal) = &self.goal_block
            && !goal.is_empty()
        {
            out.push_str(&goal.to_prompt_header());
            out.push_str(LAYER_SEPARATOR);
        }

        // Harness layers: runtime-protocol rules (write_file / finish_task /
        // out_dir / summary) that apply to every agent of the matching
        // runner. Inserted between the goal block and base so protocol
        // expectations are established before the agent's role prompt.
        for layer in &self.harness_layers {
            if !layer.trim().is_empty() {
                out.push_str(layer);
                out.push_str(LAYER_SEPARATOR);
            }
        }

        // Project-memory layer (#241): kuzu-memory recall results sit between
        // harness protocol and base so the agent reads memory before its
        // role-specific prompt.
        if let Some(mem) = &self.memory_layer {
            out.push_str(mem);
            out.push_str(LAYER_SEPARATOR);
        }

        if !self.base.trim().is_empty() {
            out.push_str(&self.base);
        }

        // Project layers: each preceded by a labeled separator carrying the
        // source file path so downstream debugging is trivial.
        for (path, content) in self.project_layers {
            out.push_str(&labeled_separator(&path));
            out.push_str(content.trim_end());
        }

        // User memory: injected after project layers but before skills so
        // project-specific knowledge takes precedence while user preferences
        // still influence skill interpretation.
        if let Some(memory) = self.user_memory_layer {
            if !out.is_empty() {
                out.push_str(LAYER_SEPARATOR);
            }
            out.push_str(&memory);
        }

        // Skill and subagent layers use the generic section separator.
        for content in self.skill_layers {
            if !out.is_empty() {
                out.push_str(LAYER_SEPARATOR);
            }
            out.push_str(&content);
        }
        // MCP layer (#241): rendered after skills so the model first reads
        // its role + skills, then learns about the external MCP services it
        // can call.
        if let Some(mcp) = self.mcp_layer {
            if !out.is_empty() {
                out.push_str(LAYER_SEPARATOR);
            }
            out.push_str(&mcp);
        }
        for content in self.subagent_layers {
            if !out.is_empty() {
                out.push_str(LAYER_SEPARATOR);
            }
            out.push_str(&content);
        }

        // #420: Caveman-style output-compression rules. Appended last so the
        // rules govern every subsequent response.
        if let Some(rules) = crate::compress::output_compression_prompt(self.output_style) {
            if !out.is_empty() {
                out.push_str(LAYER_SEPARATOR);
            }
            out.push_str(rules);
        }

        // #420: Section dedup pass — removes `## Heading` blocks that appear
        // more than once across the layer concatenation (e.g. two skills both
        // bundling the same `## Testing` section). Cheap (single FNV-1a pass)
        // and idempotent.
        crate::compress::dedup_sections(&out)
    }
}
