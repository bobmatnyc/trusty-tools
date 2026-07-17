//! Markdown+frontmatter agent loader for tcode, DARK-LAUNCHED alongside the
//! TOML loader (issue #2897, epic #2892, Slice B).
//!
//! Why: trusty-agents-common's `agents::builder` compose machinery (Slice A,
//! #2952) already resolves `extends:` inheritance chains for trusty-mpm's
//! `.md` agent format and now carries `max_tokens`/`tools` frontmatter fields
//! that mirror tcode's TOML `AgentConfig` schema. Rather than fork a second
//! `.md` parser, tcode reuses that composer and projects its output onto its
//! existing `AgentConfig`. This is purely additive: the TOML loader
//! (`AgentConfig::load`) is untouched, and both formats coexist through this
//! slice. TOML retirement is Slice D.
//! What: [`load_md_agent`] reads a `.md` agent source file, calls
//! `trusty_agents_common::agents::builder::compose_agent` to resolve its
//! `extends:` chain into one flattened document, projects the composed
//! frontmatter (via `agents::metadata::agent_metadata_from_str`) and prose
//! body onto tcode's `AgentConfig`.
//! Test: `parallel_fixture_equivalence` (same agent authored as `.toml` and
//! `.md` load to field-identical configs), `tools_projection_*` (the
//! `Option<Vec<String>>` direct-map), `hr1_initial_prompt_not_leaked_into_body`
//! (the HR-1 guard).

use std::path::Path;

use trusty_agents_common::agents::builder::compose_agent;
use trusty_agents_common::agents::metadata::{AgentMetadata, agent_metadata_from_str};

use super::config::{AgentConfig, AgentInfo, LlmParams, SystemPrompt, ToolsConfig};

/// Load a tcode [`AgentConfig`] from a `.md` agent source file.
///
/// Why: primary entry point for the `.md` loader, mirroring
/// `AgentConfig::load`'s role for the TOML path — both take a file path and
/// return a fully-populated `AgentConfig` or a descriptive error.
/// What: resolves `path`'s parent directory as the `extends:` source dir and
/// its file stem as the agent name, composes the inheritance chain via
/// `compose_agent`, then projects the result via [`project_to_agent_config`].
/// A missing parent directory, an unreadable file stem, or any
/// `AgentBuildError` (not found, cycle, depth exceeded, malformed
/// frontmatter) is surfaced as a descriptive `anyhow::Error` — never a panic —
/// matching `AgentConfig::load`'s fallibility contract so `load_all_agents`
/// can handle both loaders identically.
/// Test: `parallel_fixture_equivalence`, `load_md_agent_missing_file_errors`,
/// `load_md_agent_with_extends_resolves_chain`.
pub fn load_md_agent(path: &Path) -> anyhow::Result<AgentConfig> {
    let source_dir = path
        .parent()
        .ok_or_else(|| anyhow::anyhow!("agent file {} has no parent directory", path.display()))?;
    let name = path
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| anyhow::anyhow!("agent file {} has no usable file stem", path.display()))?;

    let composed = compose_agent(name, source_dir).map_err(|e| {
        anyhow::anyhow!(
            "failed to compose agent '{name}' from {}: {e}",
            path.display()
        )
    })?;

    let metadata = agent_metadata_from_str(&composed);
    let body = extract_body(&composed);

    Ok(project_to_agent_config(name, metadata, body))
}

/// Strip the leading YAML frontmatter block from a composed agent document.
///
/// Why: `compose_agent`'s merged frontmatter block may carry an HR-1-injected
/// `initialPrompt` (auto-derived from the agent's `role:`, keyed off
/// trusty-mpm's role table — see
/// `trusty_agents_common::agents::builder::merge_frontmatter`'s "HR-1 Part B"
/// enrichment). tcode's own role vocabulary (`engineer`/`qa`) collides with
/// that table, so an mpm-role-derived `initialPrompt` must NEVER leak into
/// tcode's `system_prompt.content` — tcode's system prompt is the composed
/// PROSE BODY only, exactly as the TOML loader's `system_prompt.content` is a
/// hand-authored prompt with no injected preamble. `AgentMetadata` (the
/// public frontmatter projection this module also uses) already omits
/// `initial_prompt`/`resource_tier` entirely, so there is no accessor that
/// could leak it into the projection; this function provides the second,
/// independent guarantee on the body side by never inspecting the
/// frontmatter block's contents at all — it locates the closing fence
/// structurally and returns only what follows.
/// What: `compose_agent`'s output always opens with a `---` line; this scans
/// past it to the next `---` line and returns everything after, trimmed. A
/// document with no opening fence (defensive fallback; `compose_agent` always
/// emits one) returns the whole string trimmed.
/// Test: `hr1_initial_prompt_not_leaked_into_body`, `extract_body_strips_frontmatter`.
fn extract_body(composed: &str) -> String {
    let mut lines = composed.lines();
    match lines.next() {
        Some(first) if first.trim() == "---" => {}
        _ => return composed.trim().to_string(),
    }

    let mut in_frontmatter = true;
    let mut body_lines: Vec<&str> = Vec::new();
    for line in lines {
        if in_frontmatter {
            if line.trim() == "---" {
                in_frontmatter = false;
            }
            continue;
        }
        body_lines.push(line);
    }
    body_lines.join("\n").trim().to_string()
}

/// Project a composed `.md` agent's metadata + body onto tcode's `AgentConfig`.
///
/// Why: centralises the frontmatter -> TOML-schema field mapping so
/// `load_md_agent` stays a thin IO/compose wrapper.
/// What:
/// - `name` -> `agent.name` (falls back to the file-stem `default_name` when
///   the frontmatter declares no `name:`, mirroring how a source file is
///   identified by its filename).
/// - `role`/`description` -> `agent.role`/`agent.description` (direct map).
/// - `model` -> `agent.model` (the higher-precedence slot per
///   `provider::routing::resolve_model`; `llm.model_override` is left unset —
///   the `.md` frontmatter has exactly one model concept, matching how the
///   TOML fixtures set `[agent].model` and leave `[llm].model_override` unset).
/// - `max_tokens` -> `llm.max_tokens` (direct map).
/// - `tools: Option<Vec<String>>` -> `ToolsConfig.allowed` (DIRECT map — both
///   sides share identical `None`=all-allowed / `Some([])`=deny-all /
///   `Some(list)`=allowlist semantics, per `Frontmatter::tools`'s doc comment
///   and `runner::in_process`'s `agent.tools.as_ref().and_then(|t|
///   t.allowed.as_ref())` consumer, which treats an absent `[tools]` section
///   (outer `None`) and a present-but-unset `allowed` identically — so always
///   wrapping in `Some(ToolsConfig { allowed })` is safe and matches the
///   instructed direct-map).
/// - composed prose body -> `system_prompt.content`.
/// - `skills:` -> intentionally DROPPED. `SystemPrompt::append_skills` exists
///   in tcode's schema but has no consumer anywhere in the codebase (verified:
///   `grep -rn append_skills crates/trusty-code/src` only shows the field
///   declaration and TOML asset literals that set it — nothing reads it). The
///   shared frontmatter's `skills:` list is real and populated
///   (DOC-42/#2889), but mapping it into a dead field would fabricate a
///   consumer that doesn't exist. Left as a gap for a future slice that adds
///   an actual skills consumer to tcode.
fn project_to_agent_config(default_name: &str, meta: AgentMetadata, body: String) -> AgentConfig {
    AgentConfig {
        agent: AgentInfo {
            name: meta.name.unwrap_or_else(|| default_name.to_string()),
            role: meta.role,
            model: meta.model,
            description: meta.description,
        },
        llm: LlmParams {
            temperature: None,
            max_tokens: meta.max_tokens,
            model_override: None,
        },
        system_prompt: SystemPrompt {
            content: body,
            // Gap: no tcode consumer for agent-declared skills yet — see the
            // doc comment above. Left empty rather than mapping into a dead
            // field.
            append_skills: Vec::new(),
        },
        tools: Some(ToolsConfig {
            allowed: meta.tools,
        }),
        runner: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A `.md` agent with no `extends:` composes and projects cleanly.
    ///
    /// Why: the base case — a self-contained agent, no inheritance chain.
    /// What: writes a single `.md` file, loads it, asserts every projected
    /// field.
    /// Test: this test.
    #[test]
    fn load_md_agent_base_case() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("solo.md"),
            "---\nname: solo\nrole: engineer\ndescription: A lone agent\nmodel: sonnet\nmax_tokens: 4096\ntools: [read_file, grep]\n---\n\nYou are a solo agent.\n",
        )
        .expect("write");

        let cfg = load_md_agent(&tmp.path().join("solo.md")).expect("load");
        assert_eq!(cfg.agent.name, "solo");
        assert_eq!(cfg.agent.role.as_deref(), Some("engineer"));
        assert_eq!(cfg.agent.description.as_deref(), Some("A lone agent"));
        assert_eq!(cfg.agent.model.as_deref(), Some("sonnet"));
        assert_eq!(cfg.llm.max_tokens, Some(4096));
        assert_eq!(cfg.system_prompt.content, "You are a solo agent.");
        assert_eq!(
            cfg.tools.and_then(|t| t.allowed),
            Some(vec!["read_file".to_string(), "grep".to_string()])
        );
    }

    /// `tools:` projects with identical `Option<Vec<String>>` semantics on
    /// both sides: absent key -> `None` (all allowed), `tools: []` ->
    /// `Some([])` (deny-all), `tools: [a, b]` -> `Some([a, b])` (allowlist).
    ///
    /// Why: this is the load-bearing contract from Slice A (#2952) — a naive
    /// `is_empty()` check would collapse deny-all into "inherit"/"allow-all".
    /// What: three fixtures, one per case.
    /// Test: this test.
    #[test]
    fn tools_projection_absent_is_none() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("a.md"),
            "---\nname: a\nrole: engineer\n---\n\nBody.\n",
        )
        .expect("write");
        let cfg = load_md_agent(&tmp.path().join("a.md")).expect("load");
        assert_eq!(cfg.tools.and_then(|t| t.allowed), None);
    }

    #[test]
    fn tools_projection_empty_list_is_deny_all() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("a.md"),
            "---\nname: a\nrole: engineer\ntools: []\n---\n\nBody.\n",
        )
        .expect("write");
        let cfg = load_md_agent(&tmp.path().join("a.md")).expect("load");
        assert_eq!(cfg.tools.and_then(|t| t.allowed), Some(vec![]));
    }

    #[test]
    fn tools_projection_list_is_allowlist() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("a.md"),
            "---\nname: a\nrole: engineer\ntools: [read_file, grep]\n---\n\nBody.\n",
        )
        .expect("write");
        let cfg = load_md_agent(&tmp.path().join("a.md")).expect("load");
        assert_eq!(
            cfg.tools.and_then(|t| t.allowed),
            Some(vec!["read_file".to_string(), "grep".to_string()])
        );
    }

    /// HR-1 GUARD: a `.md` agent declaring `role: engineer` must NOT get an
    /// mpm-role-derived `initialPrompt` injected into its `system_prompt.content`.
    ///
    /// Why: `merge_frontmatter`'s HR-1 Part B enrichment auto-injects
    /// `initialPrompt: "Begin implementation. ..."` for `role: engineer` (and
    /// a distinct prompt for `role: qa`) because trusty-mpm agents rely on
    /// that injection. tcode's roles collide with that same table by name,
    /// but tcode's system prompt is the composed PROSE BODY only — the
    /// injected `initialPrompt` lives in the frontmatter block, which
    /// `extract_body` never inspects, and `AgentMetadata` (the frontmatter
    /// projection) has no `initial_prompt` field at all. This test proves
    /// both guarantees hold end-to-end.
    /// What: an `engineer`-role agent whose body is a short known string;
    /// asserts `system_prompt.content` equals exactly that string with no
    /// HR-1 preamble prepended or appended.
    /// Test: this test.
    #[test]
    fn hr1_initial_prompt_not_leaked_into_body() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("eng.md"),
            "---\nname: eng\nrole: engineer\n---\n\nYou write Rust code.\n",
        )
        .expect("write");

        let cfg = load_md_agent(&tmp.path().join("eng.md")).expect("load");
        assert_eq!(
            cfg.system_prompt.content, "You write Rust code.",
            "HR-1's role-derived initialPrompt must not leak into the body"
        );
        assert!(
            !cfg.system_prompt.content.contains("Begin implementation"),
            "HR-1's engineer-role initialPrompt text must not appear anywhere in the body"
        );
    }

    /// `extends:` chains resolve through `compose_agent` exactly as
    /// trusty-mpm's own `.md` agents do.
    ///
    /// Why: proves the composer is genuinely reused, not reimplemented.
    /// What: a base + child fixture; the child's `AgentConfig` carries the
    /// base's body concatenated before the child's, and the child's
    /// `max_tokens` wins (scalar child-wins merge).
    /// Test: this test.
    #[test]
    fn load_md_agent_with_extends_resolves_chain() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("base-x.md"),
            "---\nname: base-x\nmax_tokens: 1000\n---\n\nBase instructions.\n",
        )
        .expect("write");
        std::fs::write(
            tmp.path().join("child.md"),
            "---\nname: child\nextends: base-x\nmax_tokens: 2000\n---\n\nChild instructions.\n",
        )
        .expect("write");

        let cfg = load_md_agent(&tmp.path().join("child.md")).expect("load");
        assert_eq!(cfg.agent.name, "child");
        assert_eq!(cfg.llm.max_tokens, Some(2000), "child max_tokens wins");
        assert!(cfg.system_prompt.content.contains("Base instructions."));
        assert!(cfg.system_prompt.content.contains("Child instructions."));
    }

    /// A missing `.md` file surfaces a descriptive error, never a panic.
    ///
    /// Why: matches `AgentConfig::load`'s fallibility contract so
    /// `load_all_agents` can `filter_map` over both loaders identically.
    /// What: `load_md_agent` on a nonexistent path returns `Err`.
    /// Test: this test.
    #[test]
    fn load_md_agent_missing_file_errors() {
        let result = load_md_agent(Path::new("/nonexistent/agents/dir/ghost.md"));
        assert!(result.is_err());
    }

    /// Parallel-fixture equivalence: the SAME agent authored as `.toml` and
    /// as `.md` load to field-identical `AgentConfig`s.
    ///
    /// Why: the acceptance criterion for Slice B — both formats must produce
    /// the same runtime behavior for an operator migrating a config.
    /// What: hand-authors matching TOML and `.md` fixtures (same name, model,
    /// max_tokens, tools, and system-prompt body), loads both, and asserts
    /// `model`, `max_tokens`, `tools.allowed`, and `system_prompt.content`
    /// are equal.
    /// Test: this test.
    #[test]
    fn parallel_fixture_equivalence() {
        let tmp = tempfile::tempdir().expect("tempdir");

        std::fs::write(
            tmp.path().join("twin.toml"),
            "[agent]\nname = \"twin\"\nrole = \"engineer\"\nmodel = \"sonnet\"\n\n[llm]\nmax_tokens = 8192\n\n[system_prompt]\ncontent = \"You are a twin agent.\"\n\n[tools]\nallowed = [\"read_file\", \"grep\"]\n",
        )
        .expect("write toml");
        std::fs::write(
            tmp.path().join("twin.md"),
            "---\nname: twin\nrole: engineer\nmodel: sonnet\nmax_tokens: 8192\ntools: [read_file, grep]\n---\n\nYou are a twin agent.\n",
        )
        .expect("write md");

        let toml_cfg = AgentConfig::load(&tmp.path().join("twin.toml")).expect("load toml");
        let md_cfg = load_md_agent(&tmp.path().join("twin.md")).expect("load md");

        assert_eq!(toml_cfg.agent.model, md_cfg.agent.model);
        assert_eq!(toml_cfg.llm.max_tokens, md_cfg.llm.max_tokens);
        assert_eq!(
            toml_cfg.tools.and_then(|t| t.allowed),
            md_cfg.tools.and_then(|t| t.allowed)
        );
        assert_eq!(toml_cfg.system_prompt.content, md_cfg.system_prompt.content);
    }

    /// `extract_body` closes the frontmatter block on the FIRST `---` fence
    /// only and never re-enters — a `---` line appearing later, inside the
    /// prose body (a markdown horizontal rule between two paragraphs), must
    /// survive verbatim in `system_prompt.content` rather than being mistaken
    /// for a second frontmatter delimiter.
    ///
    /// Why: code-critic finding on PR #2954 — this is the highest-risk edge
    /// case in the fence-scanning loop (`in_frontmatter` flips to `false` and
    /// stays `false`), and had no regression pin.
    /// What: a `.md` fixture whose body contains a paragraph, a bare `---`
    /// horizontal-rule line, then a second paragraph. Loads through the real
    /// `compose_agent` -> `load_md_agent` path (not `extract_body` in
    /// isolation) so the assertion exercises the actual composed output.
    /// Asserts the horizontal rule is present verbatim as its own line in the
    /// resulting body.
    /// Test: this test.
    #[test]
    fn interior_horizontal_rule_survives_in_body() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("hr.md"),
            "---\nname: hr\nrole: engineer\n---\n\nFirst paragraph.\n\n---\n\nSecond paragraph.\n",
        )
        .expect("write");

        let cfg = load_md_agent(&tmp.path().join("hr.md")).expect("load");
        assert!(
            cfg.system_prompt
                .content
                .lines()
                .any(|line| line.trim() == "---"),
            "interior horizontal rule must survive verbatim as its own line; got: {:?}",
            cfg.system_prompt.content
        );
        assert!(cfg.system_prompt.content.contains("First paragraph."));
        assert!(cfg.system_prompt.content.contains("Second paragraph."));
    }

    /// A `.md` agent with a valid frontmatter block and NO prose body after
    /// the closing fence projects to an EMPTY `system_prompt.content` — no
    /// panic, no leftover blank lines, no stray `---`.
    ///
    /// Why: code-critic finding on PR #2954 — the frontmatter-only/empty-body
    /// case is the other edge of the fence-scanning loop and had no
    /// regression pin.
    /// What: a `.md` fixture whose closing fence is the last line of the
    /// file. Loads through the real `compose_agent` -> `load_md_agent` path.
    /// Asserts `system_prompt.content == ""`.
    /// Test: this test.
    #[test]
    fn frontmatter_only_agent_has_empty_body() {
        let tmp = tempfile::tempdir().expect("tempdir");
        std::fs::write(
            tmp.path().join("empty.md"),
            "---\nname: empty\nrole: engineer\n---\n",
        )
        .expect("write");

        let cfg = load_md_agent(&tmp.path().join("empty.md")).expect("load");
        assert_eq!(
            cfg.system_prompt.content, "",
            "frontmatter-only agent must project to an empty body, got: {:?}",
            cfg.system_prompt.content
        );
    }
}
