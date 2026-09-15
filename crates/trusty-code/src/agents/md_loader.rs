//! Markdown+frontmatter agent loader for tcode — the ONLY on-disk agent
//! source format as of #2897 Slice D (epic #2892). It was DARK-LAUNCHED
//! alongside the (now-retired) TOML loader in Slice B, and became the loader
//! for tcode's own EMBEDDED default agents in Slice C.
//!
//! Why: trusty-agents-common's `agents::builder` compose machinery (Slice A,
//! #2952) already resolves `extends:` inheritance chains for trusty-mpm's
//! `.md` agent format and now carries `max_tokens`/`tools` frontmatter fields
//! that mirror tcode's `AgentConfig` schema. Rather than fork a second `.md`
//! parser, tcode reuses that composer and projects its output onto its
//! existing `AgentConfig`. Slice C (#2897) additionally routes
//! `crate::assets::DEFAULT_AGENTS`'s embedded fallback through this module:
//! the embedded strings have no source directory to resolve an `extends:`
//! chain against (they are compiled-in `&'static str`, not files on disk), so
//! [`project_embedded_md`] skips `compose_agent` entirely and calls
//! [`agent_metadata_from_str`]/[`extract_body`] directly on the raw string —
//! both are already string-in, string/struct-out, so no file-path dependency
//! stands in the way. The two entry points ([`load_md_agent`] for disk,
//! [`project_embedded_md`] for embedded strings) share the exact same
//! [`project_to_agent_config`] mapping, so there is only ever one frontmatter
//! -> `AgentConfig` projection to maintain.
//! What: [`load_md_agent`] reads a `.md` agent source file, calls
//! `trusty_agents_common::agents::builder::compose_agent` to resolve its
//! `extends:` chain into one flattened document, projects the composed
//! frontmatter (via `agents::metadata::agent_metadata_from_str`) and prose
//! body onto tcode's `AgentConfig`. [`project_embedded_md`] does the same
//! projection for an in-memory `&'static str` with no `extends:` resolution.
//! Test: `load_md_agent_base_case`, `tools_projection_*` (the
//! `Option<Vec<String>>` direct-map), `hr1_initial_prompt_not_leaked_into_body`
//! (the HR-1 guard), `assets::tests::*` (embedded-default field-identity vs.
//! the retired TOML fixtures, kept as a historical regression pin).

use std::path::Path;

use trusty_agents_common::agents::builder::compose_agent;
use trusty_agents_common::agents::builder_in_memory::{
    build_in_memory_source_map, compose_agent_in_memory,
};
use trusty_agents_common::agents::metadata::{AgentMetadata, agent_metadata_from_str};

use super::config::{AgentConfig, AgentInfo, LlmParams, SystemPrompt, ToolsConfig};

/// Load a tcode [`AgentConfig`] from a `.md` agent source file.
///
/// Why: the primary entry point for the (now sole) agent loader — takes a
/// file path and returns a fully-populated `AgentConfig` or a descriptive
/// error.
/// What: resolves `path`'s parent directory as the `extends:` source dir and
/// its file stem as the agent name, composes the inheritance chain via
/// `compose_agent`, then projects the result via [`project_to_agent_config`].
/// A missing parent directory, an unreadable file stem, or any
/// `AgentBuildError` (not found, cycle, depth exceeded, malformed
/// frontmatter) is surfaced as a descriptive `anyhow::Error` — never a panic.
/// Test: `load_md_agent_base_case`, `load_md_agent_missing_file_errors`,
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
    // #7727: a body carrying `{{TM_SKILLS}}` points at the address `read_file` serves.
    let composed =
        super::skill_refs::resolve_skill_refs(&composed, &super::skill_refs::user_skill_refs_dir());

    let metadata = agent_metadata_from_str(&composed);
    let body = extract_body(&composed);

    let mut config = project_to_agent_config(name, metadata, body);
    // #7948: parse the file's OWN bytes — `compose_agent` does not re-emit
    // `permissions:`, so a block is not inherited through `extends:`. A
    // malformed block fails the load; it never degrades to "no permissions".
    let raw = std::fs::read_to_string(path)
        .map_err(|e| anyhow::anyhow!("failed to read agent file {}: {e}", path.display()))?;
    config.permissions = crate::permissions::parse_permissions(&path.display().to_string(), &raw)?;
    Ok(config)
}

/// Project an embedded (in-memory, no source directory) `.md` agent document
/// onto tcode's [`AgentConfig`] — the entry point for
/// `crate::assets::DEFAULT_AGENTS`'s embedded fallback (Slice C, #2897).
///
/// Why: `load_md_agent` resolves an `extends:` inheritance chain via
/// `compose_agent`, which requires a real source directory to scan for
/// sibling `.md` files. Embedded default agents are compiled-in
/// `&'static str` constants with no filesystem location and no `extends:`
/// chain, so composing would be both impossible (no directory to resolve
/// against) and unnecessary (none of the three bundled defaults declare
/// `extends:`). This function shares every other step of the projection —
/// [`agent_metadata_from_str`] for the frontmatter and [`extract_body`] for
/// the prose body — with [`load_md_agent`], so the frontmatter -> `AgentConfig`
/// mapping in [`project_to_agent_config`] is written and tested exactly once.
/// What: parses `raw`'s frontmatter and body directly (no `compose_agent`
/// call), then delegates to [`project_to_agent_config`]. `default_name` is
/// used only when `raw`'s frontmatter carries no `name:` field, mirroring
/// `load_md_agent`'s file-stem fallback. `raw`'s own `permissions:` block is
/// parsed exactly as [`load_md_agent`] parses a file's; a malformed block is
/// an `Err`, never "no permissions".
/// Test: `assets::tests::default_agents_field_identical_to_retired_toml`,
/// `assets::tests::default_agents_parse_and_names_match`,
/// `embedded_permissions_block_projects_onto_agent_config`,
/// `malformed_embedded_permissions_block_fails_the_load`.
pub(crate) fn project_embedded_md(default_name: &str, raw: &str) -> anyhow::Result<AgentConfig> {
    let metadata = agent_metadata_from_str(raw);
    let body = extract_body(raw);
    let mut config = project_to_agent_config(default_name, metadata, body);
    // #7948: a bundled agent's block is enforced like a disk agent's.
    config.permissions =
        crate::permissions::parse_permissions(&format!("embedded agent '{default_name}'"), raw)?;
    Ok(config)
}

/// Project an embedded tm-catalog agent onto tcode's [`AgentConfig`],
/// resolving its `extends:` chain entirely against
/// `crate::assets::EMBEDDED_TM_AGENT_SOURCES` -- no filesystem access
/// (Slice E2, #2958).
///
/// Why: [`project_embedded_md`] handles tcode's own 3 defaults, none of
/// which declare `extends:`. The bundled tm agent catalog (5 `BASE-*`
/// templates + 28 coding-relevant roster agents) DOES use `extends:`
/// chains -- e.g. `rust-engineer` extends `base-engineer` extends
/// `base-agent` -- and [`compose_agent`] can't resolve those because it
/// requires a real `source_dir` to scan, which embedded `&'static str`
/// constants don't have. `trusty_agents_common::agents::builder_in_memory`
/// (Slice E1, PR #3013) supplies the disk-free counterpart: an
/// `InMemorySources` map plus `compose_agent_in_memory`, built here from
/// `crate::assets::EMBEDDED_TM_AGENT_SOURCES` and passed the requested
/// `name`.
/// What: builds an `InMemorySources` map from the embedded catalog table,
/// resolves `name`'s `extends:` chain via `compose_agent_in_memory`, then
/// projects the composed document through the same
/// `agent_metadata_from_str` + `extract_body` + [`project_to_agent_config`]
/// pipeline [`load_md_agent`] uses for the disk path -- so embedded and
/// on-disk `.md` agents share one frontmatter -> `AgentConfig` mapping. Any
/// `AgentBuildError` (unknown name, cycle, depth exceeded, malformed
/// frontmatter anywhere in the chain) is surfaced as a descriptive
/// `anyhow::Error`, never a panic. Called from
/// `crate::agents::load_embedded_default_agents` for every
/// `crate::assets::EmbeddedAgent::Composed` entry in `DEFAULT_AGENTS` (Slice
/// E3, #2958) -- the 28 roster agents are dispatchable defaults as of this
/// slice.
/// Test: `project_embedded_md_with_extends_resolves_rust_engineer_from_base_engineer`,
/// `project_embedded_md_with_extends_unknown_name_errors`,
/// `assets::tests::default_agents_parse_and_names_match`.
pub fn project_embedded_md_with_extends(name: &str) -> anyhow::Result<AgentConfig> {
    project_embedded_md_with_extends_at(name, &super::skill_refs::user_skill_refs_dir())
}

/// [`project_embedded_md_with_extends`] with the skill-refs root supplied.
///
/// Why (#7727): the hermetic core, so a test resolves `{{TM_SKILLS}}` against
/// a temp dir instead of the developer's real `~/.trusty-code`.
/// What: composes as the wrapper does, then
/// [`super::skill_refs::resolve_skill_refs`] against `skill_refs_root`, which
/// never writes and never fails, so the refs root cannot drop an agent.
/// Test: `embedded_agent_skill_pointers_open_with_read_file`,
/// `embedded_agent_composes_when_refs_dir_is_unwritable`,
/// `embedded_agent_composes_when_refs_dir_is_relative`.
pub fn project_embedded_md_with_extends_at(
    name: &str,
    skill_refs_root: &Path,
) -> anyhow::Result<AgentConfig> {
    project_in_memory_catalog(
        name,
        crate::assets::EMBEDDED_TM_AGENT_SOURCES,
        skill_refs_root,
    )
}

/// [`project_embedded_md_with_extends_at`] over a supplied
/// `(filename, markdown)` catalog.
///
/// Why (#7948): no bundled catalog agent declares `permissions:` today, so a
/// test needs its own catalog to prove a block is enforced and not inherited.
/// What: composes `name` against `entries`, resolves skill refs, and projects
/// the result. It then parses the `permissions:` block from `name`'s OWN
/// catalog entry, never the composed document, so a block is not inherited
/// through `extends:`. A malformed block is an `Err`.
/// Test: `embedded_extends_permissions_block_projects_onto_agent_config`,
/// `malformed_embedded_extends_permissions_block_fails_the_load`.
fn project_in_memory_catalog(
    name: &str,
    entries: &[(&str, &str)],
    skill_refs_root: &Path,
) -> anyhow::Result<AgentConfig> {
    let sources = build_in_memory_source_map(entries.iter().copied());

    let composed = compose_agent_in_memory(name, &sources).map_err(|e| {
        anyhow::anyhow!("failed to compose embedded tm-catalog agent '{name}': {e}")
    })?;
    let composed = super::skill_refs::resolve_skill_refs(&composed, skill_refs_root);

    let metadata = agent_metadata_from_str(&composed);
    let body = extract_body(&composed);

    let mut config = project_to_agent_config(name, metadata, body);
    // #7948: the agent's own bytes, as `load_md_agent` reads the file's own.
    let own = own_catalog_source(name, entries).ok_or_else(|| {
        anyhow::anyhow!("embedded tm-catalog agent '{name}' composed but has no source entry")
    })?;
    config.permissions =
        crate::permissions::parse_permissions(&format!("embedded tm-catalog agent '{name}'"), own)?;
    Ok(config)
}

/// The raw markdown `entries` holds for `name`, keyed the way
/// `InMemorySources::insert` keys it: lowercased, one `.md` suffix stripped.
/// The last matching entry wins, as it does on insert.
fn own_catalog_source<'a>(name: &str, entries: &[(&'a str, &'a str)]) -> Option<&'a str> {
    fn key(s: &str) -> String {
        let lower = s.to_lowercase();
        lower.strip_suffix(".md").unwrap_or(&lower).to_string()
    }
    let wanted = key(name);
    entries
        .iter()
        .rev()
        .find(|(file, _)| key(file) == wanted)
        .map(|(_, md)| *md)
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
/// Test: `hr1_initial_prompt_not_leaked_into_body`,
/// `interior_horizontal_rule_survives_in_body`,
/// `frontmatter_only_agent_has_empty_body`.
///
/// `pub(crate)`: also reused by `plugins::agents::load_plugin_agent` (#3539)
/// so a plugin agent's body is stripped identically to a disk agent's,
/// without a second frontmatter-fence scanner.
pub(crate) fn extract_body(composed: &str) -> String {
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
/// - `tcode_tools: Option<Vec<String>>` -> `ToolsConfig.allowed` (DIRECT map,
///   #7683 — `tools:` is Claude Code's vocabulary and is IGNORED here; both
///   sides share identical `None`=all-allowed / `Some([])`=deny-all /
///   `Some(list)`=allowlist semantics, per `Frontmatter::tools`'s doc comment
///   and `runner::in_process`'s `agent.tools.as_ref().and_then(|t|
///   t.allowed.as_ref())` consumer, which treats an absent `[tools]` section
///   (outer `None`) and a present-but-unset `allowed` identically — so always
///   wrapping in `Some(ToolsConfig { allowed })` is safe and matches the
///   instructed direct-map).
/// - composed prose body -> `system_prompt.content`.
/// - `skills:` -> `system_prompt.append_skills` (direct map, #2074). This was
///   deliberately DROPPED until `crate::agents::describe` gave it a consumer:
///   an agent's declared skills are part of the effective configuration
///   `agents.describe` reports. Nothing INJECTS them into the prompt yet, so
///   the field is a reporting surface rather than a runtime one — a
///   distinction the describe payload's own docs state, so no caller can read
///   a reported skill as an enforced one.
///
/// `pub(crate)`: also reused by `plugins::agents::load_plugin_agent` (#3539),
/// which calls this with the plugin's LOCAL agent name (not yet namespaced)
/// and overrides `agent.name` with the namespaced form afterward — so the
/// frontmatter -> `AgentConfig` mapping stays written exactly once across
/// disk, embedded, and plugin agents.
pub(crate) fn project_to_agent_config(
    default_name: &str,
    meta: AgentMetadata,
    body: String,
) -> AgentConfig {
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
            // #2074: `agents.describe` reports an agent's declared skills, so
            // this is no longer a dead field. Nothing INJECTS them into the
            // prompt yet — see the doc comment above.
            append_skills: meta.skills,
        },
        // #7683: `tcode_tools`, never `tools`. The shared roster's `tools:`
        // now carries CLAUDE CODE's vocabulary (`Read`, `Bash`,
        // `mcp__trusty-search`), which intersects this runtime's registry at
        // zero tools — `ToolRegistry::gated` matches by exact name, so
        // honouring it here would leave every embedded roster agent unable to
        // call anything, `finish_task` included.
        tools: Some(ToolsConfig {
            allowed: meta.tcode_tools,
        }),
        runner: None,
        // #7948: nested YAML the flat metadata reader cannot represent; the
        // fallible loading paths attach it from the document bytes.
        permissions: None,
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
            "---\nname: solo\nrole: engineer\ndescription: A lone agent\nmodel: sonnet\nmax_tokens: 4096\ntcode_tools: [read_file, grep]\n---\n\nYou are a solo agent.\n",
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

    /// `tcode_tools:` projects with identical `Option<Vec<String>>` semantics
    /// on both sides: absent key -> `None` (all allowed), `tcode_tools: []` ->
    /// `Some([])` (deny-all), `tcode_tools: [a, b]` -> `Some([a, b])`.
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
            "---\nname: a\nrole: engineer\ntcode_tools: []\n---\n\nBody.\n",
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
            "---\nname: a\nrole: engineer\ntcode_tools: [read_file, grep]\n---\n\nBody.\n",
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
    /// Why: `load_all_agents` needs a fallible, never-panicking contract so
    /// it can `filter_map` a directory listing without one bad path
    /// crashing the whole scan.
    /// What: `load_md_agent` on a nonexistent path returns `Err`.
    /// Test: this test.
    #[test]
    fn load_md_agent_missing_file_errors() {
        let result = load_md_agent(Path::new("/nonexistent/agents/dir/ghost.md"));
        assert!(result.is_err());
    }

    /// A full-fidelity `.md` fixture projects every field the schema exposes
    /// in one document (name, role, model, max_tokens, tools, system-prompt
    /// body) — the successor to Slice B's retired
    /// `parallel_fixture_equivalence` (which additionally compared against a
    /// hand-authored TOML twin; #2897 Slice D removed the TOML loader that
    /// comparison depended on, so this test keeps the full-field-set
    /// assertion on the `.md` fixture alone).
    ///
    /// Why: pins that every projected field — not just the ones individual
    /// narrower tests each cover — round-trips together from one realistic
    /// agent document.
    /// What: one `.md` fixture setting every field; asserts `model`,
    /// `max_tokens`, `tools.allowed`, and `system_prompt.content` all match
    /// the authored values.
    /// Test: this test.
    #[test]
    fn full_fidelity_md_fixture_projects_every_field() {
        let tmp = tempfile::tempdir().expect("tempdir");

        std::fs::write(
            tmp.path().join("twin.md"),
            "---\nname: twin\nrole: engineer\nmodel: sonnet\nmax_tokens: 8192\ntcode_tools: [read_file, grep]\n---\n\nYou are a twin agent.\n",
        )
        .expect("write md");

        let cfg = load_md_agent(&tmp.path().join("twin.md")).expect("load md");

        assert_eq!(cfg.agent.name, "twin");
        assert_eq!(cfg.agent.role.as_deref(), Some("engineer"));
        assert_eq!(cfg.agent.model.as_deref(), Some("sonnet"));
        assert_eq!(cfg.llm.max_tokens, Some(8192));
        assert_eq!(
            cfg.tools.and_then(|t| t.allowed),
            Some(vec!["read_file".to_string(), "grep".to_string()])
        );
        assert_eq!(cfg.system_prompt.content, "You are a twin agent.");
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

    /// `project_embedded_md_with_extends` resolves a real 3-level
    /// `extends:` chain (`rust-engineer` -> `base-engineer` -> `base-agent`)
    /// entirely against the embedded tm catalog table, with no filesystem
    /// access.
    ///
    /// Why: this is the acceptance criterion for Slice E2 (#2958) -- the
    /// embedded-extends entry point must actually compose a chain, not just
    /// parse a single flat document. `rust-engineer.md`'s frontmatter
    /// declares `extends: base-engineer`, which itself declares
    /// `extends: base-agent` (see `assets/agents/rust-engineer.md` and
    /// `assets/agents/BASE-ENGINEER.md`), so a correct compose must pull
    /// prose from all three tiers into the final body.
    /// What: calls `project_embedded_md_with_extends("rust-engineer")` and
    /// asserts the name/role/model project correctly and that the composed
    /// body contains marker text unique to each of the three tiers
    /// (BASE-AGENT's "Foundation for all trusty-mpm agents", BASE-ENGINEER's
    /// "Foundation for all engineer agents", and rust-engineer's own
    /// "toolchains-rust-core").
    /// Test: this test.
    #[test]
    fn project_embedded_md_with_extends_resolves_rust_engineer_from_base_engineer() {
        let cfg = project_embedded_md_with_extends("rust-engineer").expect("compose rust-engineer");

        assert_eq!(cfg.agent.name, "rust-engineer");
        assert_eq!(cfg.agent.role.as_deref(), Some("engineer"));
        assert_eq!(cfg.agent.model.as_deref(), Some("sonnet"));
        assert!(
            cfg.system_prompt
                .content
                .contains("Foundation for all trusty-mpm agents"),
            "composed body must include BASE-AGENT tier content"
        );
        assert!(
            cfg.system_prompt
                .content
                .contains("Foundation for all engineer agents"),
            "composed body must include BASE-ENGINEER tier content"
        );
        assert!(
            cfg.system_prompt.content.contains("toolchains-rust-core"),
            "composed body must include rust-engineer's own content"
        );
    }

    /// `project_embedded_md_with_extends` surfaces an unknown agent name as
    /// a descriptive `anyhow::Error`, never a panic.
    ///
    /// Why: mirrors `load_md_agent`'s error-surfacing contract for the fs
    /// path -- a caller iterating the embedded catalog with a typo'd name
    /// must get a message it can log, not an unwind.
    /// What: requests a name absent from `EMBEDDED_TM_AGENT_SOURCES` and
    /// asserts the call errors.
    /// Test: this test.
    #[test]
    fn project_embedded_md_with_extends_unknown_name_errors() {
        let result = project_embedded_md_with_extends("does-not-exist");
        assert!(result.is_err(), "unknown embedded agent name must error");
    }

    /// #7727 review HIGH 1: every pointer an embedded agent carries opens with
    /// the real `read_file`, rooted at a bound project and at a projectless
    /// scratch dir, although the refs root is outside both and never written.
    #[tokio::test]
    async fn embedded_agent_skill_pointers_open_with_read_file() {
        use crate::agents::skill_refs::REFERENCED_SKILL_FILES;
        use crate::tools::ReadFileTool;
        use crate::tools::traits::ToolExecutor;
        use trusty_agents_common::agents::skill_root::SKILLS_ROOT_PLACEHOLDER;

        let refs = tempfile::tempdir().expect("tempdir");
        let refs_root = refs.path().join("skill-refs");
        let cfg = project_embedded_md_with_extends_at("rust-engineer", &refs_root)
            .expect("compose rust-engineer");
        let body = &cfg.system_prompt.content;
        assert!(
            !body.contains(SKILLS_ROOT_PLACEHOLDER),
            "raw placeholder leaked"
        );

        let project = tempfile::tempdir().expect("project");
        let scratch = tempfile::tempdir().expect("projectless scratch");
        for root in [project.path(), scratch.path()] {
            let tool = ReadFileTool::new(root).with_skill_refs(&refs_root);
            for (relative, content) in REFERENCED_SKILL_FILES {
                let pointer = refs_root.join(relative).display().to_string();
                assert!(body.contains(&format!("Read `{pointer}`")), "{pointer}");
                let out = tool.execute(serde_json::json!({ "path": pointer })).await;
                assert!(!out.is_error(), "{pointer}: {}", out.content());
                assert_eq!(out.content(), *content);
            }
        }
        assert!(
            !refs_root.exists(),
            "compose and read must not write the refs dir"
        );
    }

    /// #7727 review HIGH 2: a refs root that cannot be written never drops an
    /// agent; the pointer still resolves.
    #[cfg(unix)]
    #[test]
    fn embedded_agent_composes_when_refs_dir_is_unwritable() {
        use std::os::unix::fs::PermissionsExt;

        let home = tempfile::tempdir().expect("tempdir");
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o555))
            .expect("chmod");
        let refs_root = home.path().join(".trusty-code").join("skill-refs");
        let composed = project_embedded_md_with_extends_at("rust-engineer", &refs_root);
        std::fs::set_permissions(home.path(), std::fs::Permissions::from_mode(0o755))
            .expect("chmod back");
        let cfg = composed.expect("an unwritable refs dir must not drop the agent");
        assert!(cfg.system_prompt.content.contains(&format!(
            "{}/self-improvement-loop/SKILL.md",
            refs_root.display()
        )));
    }

    /// #7727 review HIGH 2: the no-home relative root keeps the agent, with the
    /// pointer left unresolved (and a warning naming the path).
    #[test]
    fn embedded_agent_composes_when_refs_dir_is_relative() {
        use trusty_agents_common::agents::skill_root::SKILLS_ROOT_PLACEHOLDER;

        let cfg = project_embedded_md_with_extends_at(
            "rust-engineer",
            Path::new(".trusty-code/skill-refs"),
        )
        .expect("a relative refs dir must not drop the agent");
        assert!(cfg.system_prompt.content.contains(SKILLS_ROOT_PLACEHOLDER));
    }

    /// (#7948) A disk agent's `permissions:` block reaches `AgentConfig`.
    ///
    /// Why: `compose_agent` drops the key, so this proves the loader reads the
    /// file's own bytes — the regression that would leave every map inert.
    /// Test: this test.
    #[test]
    fn permissions_block_projects_onto_agent_config() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("solo.md");
        std::fs::write(
            &path,
            "---\nname: solo\nrole: engineer\npermissions:\n  read_file: allow\n  bash:\n    \"rm *\": deny\n---\n\nBody.\n",
        )
        .expect("write");

        let cfg = load_md_agent(&path).expect("loads");
        assert_eq!(
            cfg.permissions
                .expect("the block must reach the config")
                .len(),
            2
        );
    }

    /// (#7948 fail-open check) A malformed `permissions:` block FAILS the load.
    ///
    /// Why: fails if the error arm ever becomes a default — a map silently
    /// degraded to "no permissions" allows every tool.
    /// Test: this test.
    #[test]
    fn malformed_permissions_block_fails_the_load() {
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("broken.md");
        std::fs::write(
            &path,
            "---\nname: broken\nrole: engineer\npermissions:\n  bash: allowed\n---\n\nBody.\n",
        )
        .expect("write");

        let err = load_md_agent(&path).expect_err("a typo'd decision must fail the load");
        let text = err.to_string();
        assert!(text.contains("broken.md"), "must name the file: {text}");
        assert!(text.contains("allowed"), "must quote the value: {text}");
    }

    /// A valid two-rule `permissions:` block on an agent named `solo`.
    const PERMISSIONS_MD: &str = "---\nname: solo\nrole: engineer\npermissions:\n  read_file: allow\n  bash:\n    \"rm *\": deny\n---\n\nBody.\n";

    /// A `permissions:` block with a typo'd decision word.
    const MALFORMED_PERMISSIONS_MD: &str =
        "---\nname: broken\nrole: engineer\npermissions:\n  bash: allowed\n---\n\nBody.\n";

    /// A catalog whose `child` and `heir` both extend a base declaring its own
    /// block; `child` declares one too, `heir` does not.
    fn permissions_catalog(child_md: &'static str) -> [(&'static str, &'static str); 3] {
        [
            (
                "BASE-FIXTURE.md",
                "---\nname: base-fixture\nrole: engineer\npermissions:\n  bash: deny\n---\n\nBase.\n",
            ),
            ("child.md", child_md),
            (
                "heir.md",
                "---\nname: heir\nextends: base-fixture\n---\n\nHeir.\n",
            ),
        ]
    }

    /// (#7948) A bundled `Direct` agent's `permissions:` block reaches
    /// `AgentConfig`, the embedded twin of
    /// `permissions_block_projects_onto_agent_config`.
    /// Test: this test.
    #[test]
    fn embedded_permissions_block_projects_onto_agent_config() {
        let cfg = project_embedded_md("solo", PERMISSIONS_MD).expect("loads");
        assert_eq!(
            cfg.permissions
                .expect("the block must reach the config")
                .len(),
            2
        );
    }

    /// (#7948) A catalog agent's OWN block reaches `AgentConfig`; a base
    /// template's block is not inherited by an agent that declares none.
    /// Test: this test.
    #[test]
    fn embedded_extends_permissions_block_projects_onto_agent_config() {
        let catalog = permissions_catalog(
            "---\nname: child\nextends: base-fixture\npermissions:\n  read_file: allow\n  bash:\n    \"rm *\": deny\n---\n\nChild.\n",
        );
        let refs = Path::new("unused-skill-refs");
        let child = project_in_memory_catalog("child", &catalog, refs).expect("child loads");
        assert_eq!(
            child
                .permissions
                .expect("the block must reach the config")
                .len(),
            2
        );
        let heir = project_in_memory_catalog("heir", &catalog, refs).expect("heir loads");
        assert!(heir.permissions.is_none(), "a base block is not inherited");
    }

    /// (#7948 fail-open check) A malformed block FAILS a `Direct` embedded load.
    /// Test: this test.
    #[test]
    fn malformed_embedded_permissions_block_fails_the_load() {
        let err = project_embedded_md("broken", MALFORMED_PERMISSIONS_MD)
            .expect_err("a typo'd decision must fail the load");
        let text = err.to_string();
        assert!(text.contains("broken"), "must name the agent: {text}");
        assert!(text.contains("allowed"), "must quote the value: {text}");
    }

    /// (#7948 fail-open check) A malformed block FAILS a composed embedded load.
    /// Test: this test.
    #[test]
    fn malformed_embedded_extends_permissions_block_fails_the_load() {
        let catalog = permissions_catalog(
            "---\nname: child\nextends: base-fixture\npermissions:\n  bash: allowed\n---\n\nChild.\n",
        );
        let err = project_in_memory_catalog("child", &catalog, Path::new("unused-skill-refs"))
            .expect_err("a typo'd decision must fail the load");
        let text = err.to_string();
        assert!(text.contains("child"), "must name the agent: {text}");
        assert!(text.contains("allowed"), "must quote the value: {text}");
    }
}
