---
title: "Codebase Pre-Feature Analysis: Skills, Prompt Construction, Global Config, MCP, Kuzu"
date: 2026-04-30
author: research-agent
---

# Codebase Pre-Feature Analysis

Five areas surveyed before feature implementation.

---

## Area 1: Skills Loading

### How skills are loaded

There are **two parallel skill loading systems** that coexist:

**System A: `SkillsLoader`** (`src/skills/mod.rs` — the workflow-engine path)

- `struct SkillsLoader { skills_root: PathBuf, cache: Mutex<HashMap<PathBuf, String>> }`
- Constructed with a root like `.open-mpm/skills/`
- Resolves skill names by searching subdirs in order: `languages/<name>.md`, `frameworks/<name>.md`, `workflow/<name>.md`, flat `<name>.md`
- Also searches global paths: `~/.open-mpm/skills/files/` and `~/Projects/skillset-mcp`
- Key method: `build_skills_prefix_tracked(explicit: &[String], project_dir: &Path, task: &str) -> (String, Vec<String>)`
  - If `explicit` contains `"auto"`: keyword detection (Cargo.toml → rust, package.json → typescript, etc.) or LLM-based selection (`OPEN_MPM_SKILL_LLM=1`)
  - If explicit names provided: resolves those names directly
- Used by `workflow/engine.rs` for per-phase skill injection into task text (prepended to task, not system prompt)

**System B: `SkillRegistry`** (`src/skills/registry.rs` — the sub-agent startup path)

- `struct SkillRegistry { skills: IndexMap<String, SkillMeta>, tag_index: HashMap<String, Vec<String>> }`
- `SkillMeta { name, description, tags, source_path, effectiveness_score: f32, use_count: u32, last_used: Option<String> }`
- Loaded via `SkillRegistry::load(search_paths: &[PathBuf])` — synchronous, recursive directory walk
- Search path order (from `skill_search_paths(config_dir)`): `.open-mpm/skills` → `.claude/skills` → `~/.open-mpm/skills` → `~/.claude/skills` → `<config_dir>/skills`
- `OPEN_MPM_SKILLS_PROJECT_LOCAL_ONLY=1` restricts to only `.open-mpm/skills` + bundled dir (used by CTRL to avoid 700-file claude-mpm scan)
- Also loaded from `skill-sources.toml` via `SkillSourceRegistry` (operator-configurable additional paths)
- Cap: 50 skills per source (`MAX_SKILLS_PER_SOURCE`); directories with ≥200 `.md` files and no `.toml` manifest are skipped as "external" (prevents claude-mpm's 700-file `~/.claude/skills/` from hanging startup)
- Effectiveness scores are persisted to `~/.open-mpm/skills/index.json` and merged on reload

**System C: `SkillRegistry::auto_inject`** (`src/skills/mod.rs`) — used by sub-agents for tool-based skill access

### Which agents get skills and how injection happens

**Sub-agents (spawned via `--agent <name>`)** — path in `src/main.rs` around line 2318:

```rust
// In the sub-agent handler (main.rs ~2318):
if let Some(skills) = &cfg.system_prompt.skills && !skills.is_empty() {
    let resolver = FsSkillResolver::from_defaults();
    for s in skills {
        if let Some(text) = resolver.resolve(s) {
            let layer = format!("# Skill: {s}\n\n{text}");
            builder = builder.add_skill(layer);  // added to SystemPromptBuilder
        }
    }
}
```

Skills declared in the agent TOML under `[system_prompt] skills = ["rust", "tdd"]` are injected as **separate layers in the system prompt**, appended after project CLAUDE.md layers. This happens at sub-agent spawn time.

**Workflow phases** — `workflow/engine.rs` around line 801:

```rust
// Skills-first injection into task/user message text, not system prompt:
let (skills_prefix, used_names) = loader
    .build_skills_prefix_tracked(&explicit_skills, &project_dir, &rendered)
    .await;
if !skills_prefix.is_empty() {
    rendered = format!("{skills_prefix}\n\n---\n\n{rendered}");
}
```

Skills are **prepended to the task text** (user message), not to the system prompt, in the workflow engine path.

**PM orchestrator** — `src/main.rs` line 2301: only `SystemPromptBuilder::new(...).walk_project_instructions(&cwd)` — no automatic skill injection at the PM layer.

**ctrl agent** — CTRL has no explicit skill injection logic in `src/ctrl/mod.rs`.

### Skill registry or ad-hoc?

Structured registry (`SkillRegistry`) with frontmatter parsing, tag indexing, effectiveness scoring, and persistence. The legacy `SkillsLoader` co-exists for the workflow-engine path. There is no single "unified" registry; the two systems share frontmatter parsing logic but operate separately.

### TOML field for agent-declared skills

```toml
[system_prompt]
content = "..."
skills = ["rust", "tdd"]   # Optional Vec<String>
```

Defined in `AgentConfig.system_prompt: SystemPrompt` where:
```rust
pub struct SystemPrompt {
    pub content: String,
    pub skills: Option<Vec<String>>,
}
```

---

## Area 2: Agent Prompt Construction

### `SystemPromptBuilder` — `src/agents/prompt_builder.rs`

```rust
pub struct SystemPromptBuilder {
    base: String,
    harness_layers: Vec<String>,
    project_layers: Vec<(PathBuf, String)>,
    user_memory_layer: Option<String>,
    skill_layers: Vec<String>,
    subagent_layers: Vec<String>,
    goal_block: Option<crate::context::GoalBlock>,
}
```

**Build order (canonical, from `build()`):**

```
goal_block → harness_layers → base (TOML content) → project_layers → user_memory_layer → skill_layers → subagent_layers
```

Separator between layers is `"\n\n---\n\n"` (`LAYER_SEPARATOR`). Project layers use a labeled separator: `"\n\n--- [from: /path/to/CLAUDE.md] ---\n\n"`.

### Sub-agent prompt construction (`src/main.rs` ~2290–2340)

```rust
let mut builder =
    SystemPromptBuilder::new(cfg.system_prompt.content.clone())
        .walk_project_instructions(&cwd);

// Harness layers (compiled-in constants from agents::harness_protocol):
builder = builder.add_harness_layer(BASE_PROTOCOL);
if matches!(cfg.agent.runner, RunnerKind::ClaudeCode) && !cfg.llm.use_finish_task {
    builder = builder.add_harness_layer(CLAUDE_CODE_PROTOCOL);
}
if cfg.llm.use_finish_task {
    builder = builder.add_harness_layer(FINISH_TASK_PROTOCOL);
}

// Skills from [system_prompt] skills = [...]:
for s in skills {
    if let Some(text) = resolver.resolve(s) {
        builder = builder.add_skill(format!("# Skill: {s}\n\n{text}"));
    }
}

let system_prompt_content = builder.build();
```

`walk_project_instructions` walks from `$HOME/.claude/CLAUDE.md` → filesystem root CLAUDE.md/AGENTS.md → ... → CWD CLAUDE.md/AGENTS.md. Root comes before leaf (so root-level instructions appear first in the built prompt).

### PM orchestrator prompt

From `src/main.rs` line 2301: identical pattern — `SystemPromptBuilder::new(cfg.system_prompt.content.clone()).walk_project_instructions(&cwd)` — with harness layers added but no automatic skill injection unless `skills` is declared in `pm.toml`.

### ctrl prompt

ctrl uses `AgentConfig::ctrl_default()` which calls `from_toml_str(CTRL_DEFAULT_TOML, ...)`. The `CTRL_DEFAULT_TOML` constant (defined in `src/agents/mod.rs` ~line 548) is a multi-paragraph system prompt hardcoded in the binary. No special builder is used for ctrl — the REPL calls the LLM directly with the built prompt from `AgentConfig`.

### `harness_protocol` constants (`src/agents/harness_protocol.rs`)

Three compiled-in constants:
- `BASE_PROTOCOL` — output directory rules, `## Summary` requirement
- `CLAUDE_CODE_PROTOCOL` — `write_file` tool usage rules (injected only for `runner = "claude-code"` without `use_finish_task`)
- `FINISH_TASK_PROTOCOL` — `finish_task` tool rules (injected only when `use_finish_task = true`)

---

## Area 3: Global Config

### Is there a global config file?

**No `~/.open-mpm/config.toml` or equivalent global config file exists in the codebase.** There is no struct named `GlobalConfig` or similar.

What does exist at `~/.open-mpm/`:

| Path | Purpose |
|---|---|
| `~/.open-mpm/projects.json` | `ProjectRegistry` — tracks all projects seen by open-mpm |
| `~/.open-mpm/skills/index.json` | Persisted skill effectiveness scores |
| `~/.open-mpm/skills/files/` | Global skill search path (discovered by `skill_search_paths`) |
| `~/.open-mpm/memory/` | `UserMemoryStore` backed by redb + usearch |
| `~/.open-mpm/agents/` | Optional global agent TOML overrides |

### How `ProjectRegistry` is loaded (`src/registry/mod.rs`)

```rust
pub struct ProjectRegistry {
    registry_path: PathBuf,  // ~/.open-mpm/projects.json
}

impl ProjectRegistry {
    pub fn new() -> Result<Self> {
        let home = dirs::home_dir().ok_or_else(|| ...)?;
        Ok(Self { registry_path: home.join(".open-mpm").join("projects.json") })
    }
    pub async fn load(&self) -> Result<HashMap<String, ProjectEntry>> { ... }
}
```

### Agent config dir resolution (`src/agents/mod.rs`)

```rust
fn agent_config_path(name: &str) -> PathBuf {
    match std::env::var("OPEN_MPM_CONFIG_DIR") {
        Ok(s) if !s.is_empty() => PathBuf::from(s).join(format!("{name}.toml")),
        _ => PathBuf::from(".open-mpm/agents").join(format!("{name}.toml")),
    }
}
```

`OPEN_MPM_CONFIG_DIR` env var is the only mechanism for overriding where agents are found. Fallback is CWD-relative `.open-mpm/agents/`.

### `skill-sources.toml` — operator-configurable skill sources (`src/skills/sources.rs`)

Loaded by `SkillSourceRegistry::load(&project_root)` from `.open-mpm/skill-sources.toml`. This is the closest thing to a "per-project config" beyond the agent TOMLs. No global equivalent.

---

## Area 4: Current MCP Configuration

### `.mcp.json` at `/Users/masa/Projects/open-mpm/.mcp.json`

```json
{
  "mcpServers": {
    "kuzu-memory": {
      "type": "stdio",
      "command": "kuzu-memory",
      "args": ["mcp"],
      "env": {
        "KUZU_MEMORY_PROJECT_ROOT": "/Users/masa/Projects/open-mpm",
        "KUZU_MEMORY_DB": "/Users/masa/Projects/open-mpm/.kuzu-memory/memories.db"
      }
    },
    "mcp-vector-search": {
      "type": "stdio",
      "command": "uv",
      "args": ["run", "--directory", "/Users/masa/Projects/open-mpm", "mcp-vector-search", "mcp"],
      "env": {
        "PROJECT_ROOT": "/Users/masa/Projects/open-mpm",
        "MCP_PROJECT_ROOT": "/Users/masa/Projects/open-mpm"
      }
    }
  }
}
```

Two MCP servers configured:
1. `kuzu-memory` — runs `kuzu-memory mcp` stdio server; stores memories in `/Users/masa/Projects/open-mpm/.kuzu-memory/memories.db`
2. `mcp-vector-search` — runs via `uv run mcp-vector-search mcp`; semantic code search

### MCP-related structs/code in the Rust source

**No MCP client code exists in the Rust source.** MCP is used exclusively from the Claude Code side (the tool session that runs Claude Code as a subprocess). The `.mcp.json` file configures Claude Code's MCP server list, not the open-mpm Rust process.

The Rust code references MCP only in:
- `src/init/mod.rs` — seeds MCP connection documentation into the embedded memory store at first run (reads `.mcp.json` and seeds it as context, but does not call MCP tools)
- `src/tools/skill_loader.rs` — `load_skill` tool that agents can call (no MCP involvement)

---

## Area 5: Kuzu-Memory

### Kuzu integration in the Rust codebase

**No active kuzu integration exists in the Rust source.** All references are historical/tombstone comments:

- `src/tools/memory.rs` line 15: "Replaces the legacy `KuzuRecallTool` which shelled out to a Python `kuzu` interpreter — KùzuDB was archived by Apple in Oct 2025 and is unmaintained."
- `src/memory/user_store.rs` line 14: "This module previously also slurped `*.md` / `*.txt` snippets from `~/.kuzu-memory/user/` produced by the now-archived KùzuDB Python shim. That path was removed."
- `src/init/mod.rs` line 13: Similar tombstone — `.kuzu-memory/` slurping was removed.

The replacement is the embedded `redb + usearch + fastembed` stack (`src/memory/redb_usearch.rs`, `src/memory/store.rs`), which provides semantic memory without external Python dependencies.

### `~/.kuzu-memory/` structure

```
~/.kuzu-memory/
  config.yaml          # kuzu-memory MCP server config (not used by Rust)
```

The `config.yaml` is the configuration for the `kuzu-memory` MCP server (the Python/external process). Key fields relevant to cross-project memory:

- `memory.enable_multi_user: true`
- `user.mode: project` (project-scoped, not user-scoped)
- `user.user_db_path: /Users/masa/.kuzu-memory/user.db` (cross-project user memory)
- `storage.max_size_mb: 50.0`
- `recall.max_memories: 10`
- `recall.strategies: [keyword, entity, temporal]`
- `git_sync.enabled: true` — auto-syncs git commit history into memory

The actual memories database is at `/Users/masa/Projects/open-mpm/.kuzu-memory/memories.db` (per-project, per `.mcp.json`).

---

## Summary Table

| Area | Key Finding |
|---|---|
| Skills loading | Two parallel systems: `SkillsLoader` (workflow engine, prepends to task text) and `SkillRegistry` (sub-agent startup, feeds into `SystemPromptBuilder.add_skill()`). TOML field: `[system_prompt] skills = [...]`. |
| Prompt construction | `SystemPromptBuilder` in `src/agents/prompt_builder.rs`. Order: goal → harness → base TOML → project CLAUDE.md layers → user memory → skill layers → subagent layers. |
| ctrl vs PM | ctrl uses hardcoded `CTRL_DEFAULT_TOML` constant (no builder at init). PM and sub-agents both use `SystemPromptBuilder`. |
| Global config | No `~/.open-mpm/config.toml`. The closest global state is `~/.open-mpm/projects.json` (project registry) and `~/.open-mpm/skills/index.json` (skill effectiveness). Agent config dir controlled by `OPEN_MPM_CONFIG_DIR` env var. |
| MCP | `.mcp.json` configures `kuzu-memory` and `mcp-vector-search` MCP servers for Claude Code sessions. No MCP client code in the Rust binary itself. |
| Kuzu | Legacy Python kuzu shim removed. Rust codebase uses embedded `redb + usearch` for memory. `~/.kuzu-memory/` is the external `kuzu-memory` MCP server's storage (configured via `~/.kuzu-memory/config.yaml`). |
