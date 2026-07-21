---
spec_refs:
  - id: SPEC-TCPLUGIN-01~draft
    path: docs/specs/DOC-51-tcode-plugin-support-phase1.md
    anchor: SPEC-TCPLUGIN-01~draft
---

# DOC-51 — trusty-code Claude Code Plugin Support, Phase 1: Local-Directory Agents + Skills

**Status:** Draft
**Subsystem:** trusty-code — agent/skill catalog, discovery, dispatch
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-07-20
**Spec ID:** `SPEC-TCPLUGIN-01~draft` (DOC-51)
**Linked issues:** [#3539](https://github.com/bobmatnyc/trusty-tools/issues/3539) (Phase 1 scope decision); [#3542](https://github.com/bobmatnyc/trusty-tools/issues/3542) (base-agent listing filter, consumed unchanged); [#3465](https://github.com/bobmatnyc/trusty-tools/pull/3465) (skills whole-catalog-replacement threshold, consumed unchanged); [PR #3547 code-critic review](https://github.com/bobmatnyc/trusty-tools/pull/3547) (hostile-plugin-input hardening + dispatch wiring, §2.5)
**Cross-ref:** `crates/trusty-code/src/plugins/{mod,agents,skills}.rs` (this spec's implementation, incl. `is_valid_namespaced_name`/`safe_plugin_subdir`); `crates/trusty-code/src/agents/{mod,md_loader,protocol}.rs` (agent discovery/loading/catalog, reused not forked); `crates/trusty-code/src/skills/{mod,protocol}.rs` (skill discovery/catalog, reused not forked); `crates/trusty-code/src/tools/{delegate,skill}.rs`, `crates/trusty-code/src/runner/in_process.rs`, `crates/trusty-code/src/main.rs` (namespaced-dispatch validation gates, §2.5)

> **Scope note.** This is a **behavior contract** for Phase 1 of Claude Code
> plugin support: ingesting a plugin's `agents/` and `skills/` from a **local
> directory** only. It explicitly does **not** cover marketplace/git-fetched
> plugins, commands, hooks, or MCP servers — those are later phases, tracked
> separately against #3539. This spec does not re-specify tcode's existing
> agent/skill formats (Markdown+frontmatter, progressive-disclosure
> `SKILL.md`) — it specifies only the discovery, namespacing, and precedence
> layer a plugin adds on top of them.

---

## 1. Motivation

tcode already parses the exact on-disk formats a Claude Code plugin ships
agents (`.md`+frontmatter) and skills (`SKILL.md`, progressive disclosure)
in. A plugin is conceptually just a third source of agents/skills, alongside
the embedded/bundled roster and a project's own `.claude/agents|skills/`.
Phase 1 answers one question narrowly: given a local plugin directory, how
does tcode discover its agents/skills, name them so they never collide with
anything else, and dispatch/resolve them — without touching the later-phase
surfaces (commands, hooks, MCP) a real Claude Code plugin manifest may also
declare.

## 2. Behavior Contract {#SPEC-TCPLUGIN-01~draft}

### 2.1 Source and discovery

- **Input:** a project root. Plugins are auto-scanned from
  `<project_root>/.claude/plugins/<plugin>/` — every immediate subdirectory
  of `.claude/plugins/` is one plugin root. No marketplace or git-fetch
  source is consulted in Phase 1; a plugin not physically present on disk
  under this path does not exist as far as tcode is concerned.
- **Manifest (optional):** if `<plugin_dir>/.claude-plugin/plugin.json`
  exists and parses, its `name` field overrides the resolved plugin
  identity (falling back to the directory name), and its `agents`/`skills`
  fields override the default `agents/`/`skills` subdirectory convention
  (relative to the plugin root). A missing or malformed manifest degrades
  to the directory-name + convention fallback — never an error.
- **Later-phase manifest keys** (`commands`, `hooks`, `mcpServers`, `mcp`):
  detected and logged at `DEBUG`, never acted on and never a hard failure.
- **Output:** a `PluginRoot` per discovered plugin (resolved name, agents
  dir, skills dir), sorted by name.
- **Implementing module:** `plugins::{PluginRoot, discover_plugin_roots}`.

### 2.2 Agent ingestion and namespacing

- **Input:** each `PluginRoot`'s `agents_dir`, scanned via the same
  `.md`-only, `.toml`-warns discovery disk agents already use
  (`agents::discover_agents`).
- **Namespacing:** every plugin agent is surfaced as `<plugin>:<name>` in
  `agents.list` (tier `"plugin"`) and is the ONLY name it resolves under via
  `agents::resolve_agent`. The namespaced key structurally cannot collide
  with an unnamespaced project or embedded/bundled agent name, so plugin
  agents are additive-only by construction — they never override, and are
  never suppressed by, a project/embedded entry of the same local name (both
  simply coexist in the catalog under different keys).
- **Base-agent filter:** a plugin agent whose LOCAL name matches one of the
  five `BASE-*` composition-template names is excluded from the listing,
  exactly like the embedded/disk tiers (issue #3542's filter, reused
  unchanged via `agents::protocol::is_base_agent`).
- **Frontmatter projection:** parsed via the same
  `agents::md_loader::project_to_agent_config` mapping disk/embedded agents
  use (role, description, model, `max_tokens`, `tools`). Fields tcode's
  `AgentConfig` has no slot for — `effort`, `maxTurns`, `memory`,
  `isolation`, `disallowedTools` (trusty-mpm's own agent-frontmatter
  superset, which a plugin agent may carry over) — are DROPPED, with one
  aggregated `WARN` per agent naming every dropped field. This never fails
  the load.
- **`extends:` (leaf-only):** a plugin agent declaring `extends:` is loaded
  as a direct/leaf document — its own frontmatter and body only, no
  cross-catalog compose — with one `WARN` naming the ignored parent.
  Cross-catalog inheritance is an explicit non-goal of Phase 1.
- **Implementing module:** `plugins::agents::{discover_plugin_agents,
  load_plugin_agent, find_plugin_agent_config}`.

### 2.3 Skill ingestion and namespacing

- **Input:** each `PluginRoot`'s `skills_dir`, scanned for immediate
  `<name>/SKILL.md` subdirectories — cheap frontmatter only
  (`name`/`description`), matching tcode's progressive-disclosure discovery
  contract (`skills::discover_skill_metadata`'s shape). A skill's full body
  is loaded lazily, only on invoke.
- **Namespacing:** every plugin skill is surfaced as `<plugin>:<name>` in
  `skills.list` (tier `"plugin"`) and in `use_skill`'s resolvable catalog.
- **Precedence independence (locked decision):** plugin skills are an
  INDEPENDENT additive tier with respect to the project-vs-bundled
  whole-catalog-replacement threshold PR #3465 established for
  `skills.list` (a project's own non-empty `.claude/skills/` entirely
  replaces the bundled catalog in the response). The plugin tier is added
  identically regardless of which side of that threshold the response
  landed on — a project with exactly one custom skill AND an installed
  plugin sees its custom skill, no bundled entries, AND every namespaced
  plugin skill, all at once.
- **Implementing module:** `plugins::skills::{discover_plugin_skills,
  resolve_plugin_skill_body}`.

### 2.4 Resolution seam (no new parameters)

Every integration point recovers the project root it needs from a directory
it already holds — `<root>/.claude/agents` (agents catalog state,
`agents::resolve_agent`'s `dir` argument) or `<root>/.claude/skills`
(skills catalog state, `skills::FsSkillResolver`'s `skills_dir`) — via
`plugins::project_root_two_levels_up`, rather than threading a new
`project_root` parameter through existing public signatures/constructors.
Projectless (no bound project) daemons see no plugin tier anywhere: there is
no `.claude/plugins/` to scan without a project root.

**Dispatch is wired end-to-end**, not merely resolvable at the
`agents::resolve_agent` layer: `tools::delegate::DelegateToAgentTool::execute`
(the PM's `delegate_to_agent` tool), the CLI's `tcode run-task` argument
validation, and `runner::agent_config_exists` (the pre-flight existence
gate both of those call before ever reaching the runner) all accept the
`<plugin>:<name>` shape — see §2.5. A namespaced plugin agent is therefore
genuinely delegatable by the PM, not just listed in `agents.list`.

### 2.5 Security: plugin content is hostile input {#SPEC-TCPLUGIN-01~draft}

Everything under `.claude/plugins/` — `plugin.json`, directory names, and
frontmatter content — is THIRD-PARTY input, unlike a project's own
`.claude/agents|skills/` (first-party, authored by the project owner). Two
path-join sites and the dispatch-validation gates were hardened against this
trust boundary (code-critic review on PR #3547):

- **`plugin.json` `agents`/`skills` overrides (CRITICAL).**
  `plugins::load_plugin_root` previously joined a manifest's `agents`/
  `skills` string directly onto `plugin_dir` — `PathBuf::join` REPLACES the
  base entirely for an absolute component (`plugin_dir.join("/etc")` ==
  `/etc`), and a relative `../../..` override escapes upward even when not
  absolute. `plugins::safe_plugin_subdir` now rejects an override that is
  absolute or contains a `..` component outright, and additionally requires
  the joined candidate to canonicalize to a path contained within
  `plugin_dir`'s own canonicalization (also catching a symlink planted
  inside `plugin_dir` that resolves outside it). Any rejection logs one
  `WARN` and falls back to the un-overridden `agents`/`skills` convention —
  it never returns the untrusted path.
- **Namespaced dispatch names (CRITICAL/HIGH).** A caller-supplied
  `<plugin>:<local>` name — reachable from an LLM via the `use_skill` and
  `delegate_to_agent` tool arguments — is joined onto a plugin's
  `agents_dir`/`skills_dir` to build a filesystem path
  (`plugins::agents::find_plugin_agent_config`,
  `plugins::skills::resolve_plugin_skill_body`). `plugins::is_valid_namespaced_name`
  is the ONE shared guard: it requires EXACTLY two `:`-separated segments,
  each matching `agents::protocol::validate_agent_name`'s existing safe
  charset (`[a-z0-9-]+`, 1-64 chars — reused, not re-derived). Because that
  charset contains no `/` and no `.` at all, a traversal payload like
  `plugin:../../../../etc/passwd` or `plugin:/etc/passwd` is syntactically
  impossible to construct — the guard runs BEFORE any path is built, in the
  function that actually builds it (not merely in an upstream caller).
  `plugins::skills::resolve_plugin_skill_body` additionally requires the
  namespaced name to be one `discover_plugin_skills` actually found on disk
  this scan — closing the residual gap where a plugin's own `SKILL.md`
  frontmatter could declare a spoofed `name:` that passed the charset check
  but was never a real, scanned skill.
- **Dispatch-path validators updated to ACCEPT the namespaced shape.**
  Before this fix, `tools::delegate::DelegateToAgentTool::execute`, the
  CLI's `validate_agent_name` (`main.rs`), and `runner::agent_config_exists`
  each rejected ANY name containing `:` outright — so even though
  `agents::resolve_agent` could resolve a namespaced name, nothing upstream
  ever let one reach it: a plugin agent was listed but never actually
  dispatchable. All three now check `plugins::is_valid_namespaced_name`
  first (falling through to their original plain-name charset check when it
  doesn't match), which both ENABLES dispatch and IS the traversal guard for
  each of those three call sites.

## 3. Non-Goals (Explicitly Out of Scope, Phase 1)

- Marketplace or git-fetched plugin sources.
- Plugin `commands/`, `hooks/`, or MCP server declarations — detected and
  logged only, never executed or registered.
- Cross-catalog `extends:` composition for plugin agents (a plugin agent
  extending a project/embedded/another-plugin agent).
- A GUI plugin install/management flow — Phase 1 is config/CLI-driven via
  the `.claude/plugins/` directory convention only.
- Overriding an unnamespaced project or embedded/bundled agent/skill name —
  namespacing makes this structurally impossible, not merely policy.

## 4. Conformance Matrix

| Requirement | Implementing module | Status |
|---|---|---|
| Discover plugins from `.claude/plugins/<plugin>/`, honoring `plugin.json` overrides | `plugins::discover_plugin_roots` | Implemented |
| Namespace + list plugin agents in `agents.list` (`plugin` tier, additive) | `plugins::agents::discover_plugin_agents`, `agents::protocol::agents_list` | Implemented |
| Resolve `<plugin>:<name>` for dispatch | `plugins::agents::find_plugin_agent_config`, `agents::resolve_agent` | Implemented |
| `<plugin>:<name>` is genuinely delegatable end-to-end (PM tool, CLI, pre-flight gate) | `tools::delegate::DelegateToAgentTool`, `main.rs::validate_agent_name`, `runner::agent_config_exists` | Implemented |
| Drop unsupported agent frontmatter fields with a warning | `plugins::agents::load_plugin_agent` | Implemented |
| Treat `extends:` as leaf-only with a warning | `plugins::agents::load_plugin_agent` | Implemented |
| Namespace + list plugin skills in `skills.list` (`plugin` tier, independent of PR #3465 threshold) | `plugins::skills::discover_plugin_skills`, `skills::protocol::skills_list` | Implemented |
| Resolve `<plugin>:<name>` skill bodies for `use_skill` | `plugins::skills::resolve_plugin_skill_body`, `skills::FsSkillResolver` | Implemented |
| Ignore commands/hooks/MCP with a debug log | `plugins::load_plugin_root` | Implemented |
| Reject `plugin.json` `agents`/`skills` overrides that escape `plugin_dir` (§2.5) | `plugins::safe_plugin_subdir` | Implemented |
| Reject a traversal/unsafe-charset namespaced dispatch name before any path is built (§2.5) | `plugins::is_valid_namespaced_name` | Implemented |
