---
title: Skill System Gap Analysis
date: 2026-04-24
author: research-agent
---

# Skill System Gap Analysis

Five-dimension audit of what is implemented, partially wired, or completely absent in
the open-mpm skill system as of 2026-04-24.

---

## Dimension 1 — Approved skill sources config

**Status: Partially implemented (fixed paths, no user-configurable list)**

The source priority order is hard-coded in two places:

1. `src/skills/registry.rs::skill_search_paths(config_dir)` — returns a fixed,
   ordered `Vec<PathBuf>`:
   ```
   .open-mpm/skills          (project-local, highest priority)
   .claude/skills            (project-level claude-mpm compat)
   ~/.open-mpm/skills        (user-level)
   ~/.claude/skills          (user-level claude-mpm compat)
   <config_dir>/skills       (bundled, lowest priority)
   ```
2. `src/skills/mod.rs::SkillRegistry::load_with_global_cache` uses two hardcoded
   global paths: `~/.open-mpm/skills/files/` and `~/Projects/skillset-mcp`.

There is no config knob that lets an operator add a git URL, an HTTP remote, or an
arbitrary additional directory. The `GlobalSkillsCache` in `src/skills/global_cache.rs`
provides a content-addressed on-disk cache (`~/.open-mpm/skills/cache/<sha>`) and a
JSON index (`~/.open-mpm/skills/index.json`), but it is **not wired into the startup
path** — `main.rs` calls `SkillRegistry::load_with_global_cache` (the simpler local
+global scan), not `GlobalSkillsCache::refresh`.

**Gap:** No operator-facing config (TOML or env var) for additional remote or
per-project skill source URLs. The paths are hard-coded in Rust source.

---

## Dimension 2 — Graph/metadata index

**Status: In-memory inverted tag index only; no graph store; rebuilt every run**

`src/skills/registry.rs` (`SkillRegistry` — note: there are two structs named
`SkillRegistry`; this is the one in the `registry` submodule) implements:

- An `IndexMap<String, SkillMeta>` (ordered by discovery) keyed on skill name.
- A `HashMap<String, Vec<String>>` inverted index: tag → [skill_names].
- Both are built in `SkillRegistry::load(search_paths)` at startup and discarded on
  process exit. Nothing is persisted between runs.

The `SkillMeta` struct holds exactly:
```rust
pub struct SkillMeta {
    pub name: String,
    pub description: String,
    pub tags: Vec<String>,
    pub source_path: PathBuf,
}
```

No kuzu, no SQLite, no embedding store. The `src/skills/global_cache.rs` module has a
SHA-256 content cache and a JSON index of metadata, but it is not used at runtime (see
Dimension 1).

The older `src/skills/mod.rs::SkillRegistry` (the `mod`-level struct) stores a
`Vec<SkillEntry>` and uses a float relevance score (substring matching, O(n)) rather
than the O(1) inverted index. Both structs coexist; the tag-indexed one (`registry.rs`)
is the newer, more capable implementation but is only partially wired in (see
Dimension 3).

**Gap:** No graph store, no persistent index, no embedding-based semantic search.

---

## Dimension 3 — Planning includes skill finding

**Status: Instructed in system prompt; NOT wired as a required workflow step**

The plan-agent TOML (`.open-mpm/agents/plan-agent.toml`) contains an explicit
"Dependency Skill Lookup (MANDATORY)" section instructing the agent to call
`list_skills(tags=[...])` and `load_skill(...)` for every third-party library before
writing stubs. Both tools appear in `[tools].allowed`.

However, this is prompt-only guidance, not a hard workflow constraint. The workflow
engine (`src/workflow/engine.rs`) has no pre-plan "skill discovery" phase, no assertion
that skill lookup happened, and no structured result that flows into the context.

The `SkillListTool::with_tag_registry` constructor (the new tag-indexed path) is wired
but marked `#[allow(dead_code)]` in `src/tools/skill_loader.rs`. In `main.rs`,
`build_registry_for_agent` registers `SkillListTool::with_registry` (the legacy
`SkillRegistry` — float-score path), not `with_tag_registry`. The new `TagSkillRegistry`
built at startup in `main.rs` (lines 243-251) is stored in a local `_skill_registry`
variable that is never threaded into agent tool registries.

**Gap:** Tag-indexed `list_skills` is implemented but not connected to sub-agents.
There is no engine-level enforcement that planning calls `list_skills` before writing
stubs.

---

## Dimension 4 — Agents use matched skills

**Status: Three mechanisms exist; (c) is partially wired but not the primary path**

The following injection mechanisms are present:

**(a) All skills always loaded** — Not the case. Skills are never blindly concatenated
into every prompt.

**(b) Manually specified per-agent in TOML** — `PhaseDef.skills: Option<Vec<String>>`
exists in `src/workflow/config.rs`. The workflow engine reads `phase.skills` and passes
it to `SkillsLoader::build_skills_prefix` as `explicit_skills`. When set to `["auto"]`
it triggers language/framework detection; when set to named skills it loads those
specific files. However, no phase in `prescriptive.json` or `prescriptive-gpt.json`
sets this field, so it is dead config in practice.

**(c) Dynamically matched by tags at task time** — Two mechanisms:
  - `SkillRegistry::auto_inject` (legacy, mod-level): keyword/substring score, returns
    top-N skill bodies prepended to the rendered phase task. Wired via
    `WorkflowEngine::with_skill_registry` → called at line ~439 of `engine.rs`.
  - `SkillsLoader::build_skills_prefix`: detects language from `Cargo.toml` /
    `requirements.txt` / `package.json`, detects frameworks from task text keywords
    (fastapi, pytest, docker…). Wired via `WorkflowEngine::with_skills_loader` → called
    at line ~417 of `engine.rs`. The skills-loader path runs FIRST and "supersedes"
    the legacy path when both are present.

In `main.rs` (line ~835) the workflow engine is constructed with both
`with_skill_registry(Some(skill_registry))` and `with_skills_loader(Some(skills_loader))`.
So for workflow runs, (c)-dynamic is active via two overlapping channels.

For sub-agent runs (the `--agent` path), `build_registry_for_agent` wires
`SkillLoaderTool` and `SkillListTool` into the tool registry so agents can call
`load_skill` / `list_skills` themselves, but auto-injection does not apply.

**Summary of injection mode:** Dynamic match at task time via `auto_inject` + keyword
detection. No TOML-specified skills active for any shipped phase.

---

## Dimension 5 — Effectiveness rating / feedback loop

**Status: Not implemented**

There is no post-task skill effectiveness scoring anywhere in the codebase:

- `src/workflow/engine.rs` records phase outputs and timing (`PerfCollector`) but
  captures nothing about which skills were injected or whether they correlated with
  pass/fail.
- `observe-agent.toml` is a plain Markdown report synthesizer — it has no `list_skills`
  / `load_skill` tools and produces no structured skill-usage metadata.
- `qa-agent.toml` has a prose section "Skill Lookup on Test Failure" telling the QA
  agent to call `list_skills(tags=[...])` when a test fails and to document root cause
  "so a skill can be created from your findings." This is the only feedback-to-skills
  mechanism, and it is entirely manual (no automatic capture, no score stored).
- No `skill_usage_log`, no hit-rate metric, no `skill_effectiveness` field in any perf
  record, no automated skill update from QA findings.

**Gap:** The feedback loop is completely missing as automated infrastructure. The QA
agent's prose instruction to note missing skills is the extent of it.

---

## Summary Table

| Dimension | Status | Key gap |
|-----------|--------|---------|
| 1. Approved skill sources config | Partial | Hard-coded paths; no operator config for remote/extra sources |
| 2. Graph/metadata index | Partial | In-memory inverted tag index only; no persistence; `GlobalSkillsCache` exists but is not used at runtime |
| 3. Planning includes skill finding | Partial | Instructed in plan-agent prompt; not engine-enforced; new `TagSkillRegistry` built at startup but not threaded into sub-agent tool registries |
| 4. Agents use matched skills | Partial — (c) active | Auto-inject via `auto_inject` + `SkillsLoader` keyword detection is wired; no phase in prescriptive.json uses explicit `skills:` field; new tag-registry `list_skills` path is dead code |
| 5. Effectiveness rating | Missing | No automated scoring, no skill-usage log; only manual prose instruction in qa-agent system prompt |

---

## Notable Implementation Quirks

- There are two distinct `SkillRegistry` structs: `src/skills/mod.rs` (float-score,
  used for `auto_inject` and tool wiring) and `src/skills/registry.rs` (tag-inverted
  O(1), newer, only partially connected). The naming collision creates confusion.
- `SkillListTool::with_tag_registry` is marked `#[allow(dead_code)]` with a comment
  "Wired into `build_registry_for_agent` in a follow-up PR" — that follow-up PR has
  not landed.
- The `GlobalSkillsCache` (SHA-256 content cache + JSON index) in `global_cache.rs`
  is fully implemented but neither called at startup nor surfaced through any tool.
