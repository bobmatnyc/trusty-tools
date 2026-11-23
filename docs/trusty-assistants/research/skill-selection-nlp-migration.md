---
title: Skill Selection Pipeline — Current State and NLP Migration Plan
date: 2026-04-27
status: research
---

# Skill Selection Pipeline: Current State and NLP Migration Plan

## 1. Current Pipeline — Precise Description

There are **two independent skill-selection subsystems** operating in parallel.
They share the same disk layout but are architecturally separate.

---

### Path A: `SkillsLoader` + `build_skills_prefix_tracked`

**Trigger**: Called on every phase that has a `skills_loader` attached
(`engine.rs` lines ~782–815).

**Inputs at selection time**:
| Input | Source | Available? |
|---|---|---|
| `explicit` | `phase.skills` from workflow JSON (e.g. `["auto"]` or `["rust", "tdd"]`) | Yes |
| `project_dir` | `code_dir ?? out_dir ?? cwd` (the task output directory, not harness root) | Yes |
| `task` (rendered) | The rendered phase template string after `{{task}}` substitution | Yes — full task text |

**Algorithm when `explicit == ["auto"]`** (`build_skills_prefix_tracked`, `skills/mod.rs` line 747):

1. **`detect_languages(project_dir)`** — checks for presence of sentinel files:
   - `Cargo.toml` → `"rust"`
   - `requirements.txt` | `pyproject.toml` | `setup.py` → `"python"`
   - `package.json` → `"typescript"`

2. **`detect_frameworks(task)`** — lowercased substring scan of rendered task text:
   - `"fastapi"` → `"fastapi"`
   - `"pytest"` | `" test "` | `"testing"` → `"pytest"`
   - `"sqlalchemy"` → `"sqlalchemy"`
   - `"tokio"` | `"axum"` → `"tokio"`
   - `"docker"` | `"container"` → `"docker"`

3. **`detect_workflow_skills(task)`** — lowercased substring scan:
   - `"tdd"` | `"test first"` | `"red green"` | `"red-green"` → `"tdd"`
   - `"wave"` | `"decompose"` → `"wave-planning"`

4. Union of all three → `skill_names: Vec<String>`

5. **`resolve_skill_path(name)`** — searches in order:
   - `.open-mpm/skills/languages/<name>.md`
   - `.open-mpm/skills/frameworks/<name>.md`
   - `.open-mpm/skills/workflow/<name>.md`
   - `.open-mpm/skills/<name>.md` (flat fallback)
   - `~/.open-mpm/skills/files/<name>.md` (global)
   - `~/Projects/skillset-mcp/<name>.md` (global)

6. Loads each resolved file, strips frontmatter, assembles a `## Relevant Skills` prompt block prepended to the rendered task.

**Output**: A markdown prefix injected before the agent's task prompt.

---

### Path B: `SkillRegistry` (`tag_skill_registry`) + `discover_skills_for_task`

**Trigger**: Called once per workflow run, before the phase loop (engine.rs line 632).
Only injects into the **plan phase** (line 757); other phases use Path A.

**Inputs at selection time**:
- `task: &str` — the cleaned task text (persona tags stripped, `{{task}}` is the original user request)

**Algorithm** (`engine.rs` line 186, `task_signals.rs`):

1. `TaskSignals::extract(task)` — single-pass lowercased substring scan producing:
   - `languages`: python, rust, typescript, javascript, go, java, ruby
   - `frameworks`: fastapi, flask, django, pytest
   - `tags`: testing, docker, git-operations, wave-planning, documentation
   - `role`: docs > research > planner > qa > ops > engineer (priority-ordered)

2. Union of `signals.tags + signals.languages + signals.frameworks` → tag set

3. `SkillRegistry.find_by_tags(&tag_refs)` — O(1) inverted-index lookup (tag → Vec<name>),
   ranked by tag-overlap count + effectiveness score (use_count, recency from perf data)

4. Top-N results returned as `Vec<DiscoveredSkill>` with name, summary, tags

5. Skills are **listed as a summary** in the plan prompt (not full bodies), so the planner knows what's available before writing assignments

**Output**: Skill summaries in the plan-phase prompt; full bodies NOT injected here.

---

### Path C: `SkillRegistry.auto_inject` (legacy)

**Trigger**: Falls through to this when NO `skills_loader` is attached (engine.rs line 821).

**Algorithm**: `SkillRegistry.search(query, top_n)` — scores each skill entry by
word-level substring match across `name` (0.4), `description` (0.2), `tags` (0.4),
capped at 1.0. Returns top-N with score > 0.

---

## 2. Available Skills

### Project-local (`.open-mpm/skills/`)

**Top-level:**
- `fixture-quality.md`
- `git-operations.md`
- `python-compat.md`
- `python-packaging.md`
- `python-testing.md`

**`languages/`:**
- `go-idiomatic.md`
- `java-idiomatic.md`
- `python-idiomatic.md`
- `python.md`
- `react-idiomatic.md`
- `rust-idiomatic.md`
- `rust.md`
- `typescript-idiomatic.md`

**`frameworks/`:**
- `fastapi.md`
- `pytest.md`

**`workflow/`:**
- `delegation.md`
- `docker.md`
- `tdd.md`
- `wave-planning.md`

**`personas/`** (discovered but not listed — likely persona-injection skills)

**Total project-local: ~19 skill files.**

Global skills from `~/.open-mpm/skills/files/` and `~/Projects/skillset-mcp` supplement these (up to 50 per source, see `MAX_SKILLS_PER_SOURCE`).

---

## 3. Skill File Format

Each skill is a Markdown file with an optional YAML frontmatter block:

```markdown
---
name: rust
description: Ownership, lifetimes, error handling, async tokio, iterators, Arc/Mutex
tags: [rust, ownership, lifetimes, async, tokio, error-handling, iterators]
---

# Rust Language Skill

## Ownership and the Borrow Checker Mental Model
...body content injected verbatim into agent prompt...
```

**Parsed fields:**
| Field | Type | Required | Fallback |
|---|---|---|---|
| `name` | String | No | file stem (e.g. `"rust"` from `rust.md`) |
| `description` | String | No | empty string |
| `tags` | `[tag1, tag2, ...]` | No | empty list |

Files without frontmatter are indexed with the filename as name and empty tags/description.

---

## 4. Key Problems with the Current Approach

### A. Two separate keyword systems that diverge
`SkillsLoader.detect_frameworks` and `TaskSignals::extract` are maintained separately.
A new framework added to one is not automatically in the other.

### B. Exact substring matching misses paraphrase
- Task: "build an HTTP service in Rust" → no `"fastapi"` hit, no `"tokio"` hit
  (even though tokio is used internally; "tokio" must literally appear in the text)
- Task: "implement a REST API with actix-web" → zero framework matches

### C. File-system detection is too coarse and wrong-directory-prone
Before fix #233, `Cargo.toml` in the harness root caused "rust" to be injected into every task.
The fix scopes detection to `code_dir` (agent output dir) — but that directory is usually
empty at skill-selection time, so FS detection often fires only on re-runs.

### D. No semantic understanding of intent
"Write comprehensive unit tests for the payment module" → only hits `"testing"` tag via
the word `"test"`, not TDD or pytest or fixture-quality skills which are clearly relevant.

### E. Skills discovered vs. skills injected are disconnected
Path B (`discover_skills_for_task`) surfaces skill names to the planner. Path A
(`build_skills_prefix_tracked`) independently re-runs keyword matching to inject
bodies. A skill the planner knew about may not get its body injected into subsequent
phases (different keyword coverage).

---

## 5. Recommended NLP (LLM-Based) Skill Selection

### 5.1 Proposed Architecture

Replace the keyword matching inside `build_skills_prefix_tracked` (and optionally
`TaskSignals::extract`) with a single cheap LLM call that outputs a ranked list of
skill names.

```
Input:
  - task: &str                  (rendered phase task text)
  - available_skills: Vec<(name, description, tags)>  (from registry)
  - max_skills: usize           (e.g. 3)

LLM call → Output: Vec<String>  (ordered list of skill names, most relevant first)

Fallback: current keyword matching if LLM call fails or times out
```

### 5.2 Where to Insert

**Location**: `SkillsLoader::build_skills_prefix_tracked` in `src/skills/mod.rs` around line 747.

Replace the block:
```rust
let skill_names: Vec<String> = if explicit.iter().any(|s| s == "auto") {
    let mut names = Self::detect_languages(project_dir);
    names.extend(Self::detect_frameworks(task));
    names.extend(Self::detect_workflow_skills(task));
    names
} else {
    explicit.to_vec()
};
```

With:
```rust
let skill_names: Vec<String> = if explicit.iter().any(|s| s == "auto") {
    // Try LLM-based selection; fall back to keyword matching on error.
    match llm_select_skills(&self.skills_index, task, max_skills, &self.llm_client).await {
        Ok(names) => names,
        Err(e) => {
            tracing::warn!(error = %e, "LLM skill selection failed; using keyword fallback");
            let mut names = Self::detect_languages(project_dir);
            names.extend(Self::detect_frameworks(task));
            names.extend(Self::detect_workflow_skills(task));
            names
        }
    }
} else {
    explicit.to_vec()
};
```

The `llm_select_skills` function needs access to:
- The `SkillRegistry` index (already loaded at engine startup)
- An `async-openai` client (already exists in the engine, needs threading into `SkillsLoader`)
- OR a closure/trait object injected at construction time

### 5.3 Prompt Design

```
System:
  You are a skill selector. Given a task description and a list of available skills,
  return the names of the most relevant skills to inject into an AI coding agent's prompt.
  Respond with ONLY a JSON array of skill names, e.g. ["rust", "tdd", "pytest"].
  Select 0 to {max_skills} skills. Prefer precision over recall — only select skills
  that are clearly useful for this specific task. Return [] if no skill is relevant.

User:
  TASK:
  {task_text}

  AVAILABLE SKILLS:
  {skill_index}

  Select the most relevant skills (max {max_skills}).
```

Where `{skill_index}` is the output of `SkillRegistry::format_index()`, which already
produces a bulleted list: `**name** — description [tags: ...]`.

For a registry of 19 local skills + 50 global = ~70 skills, the index fits comfortably
in a single prompt (roughly 2–4KB).

### 5.4 Model Selection

Use **`anthropic/claude-haiku-3`** (or OpenRouter equivalent: `anthropic/claude-3-haiku`).

Rationale:
- Skill selection is a structured extraction task — no reasoning required
- Response is a single small JSON array (< 100 tokens output)
- Haiku is ~20x cheaper and ~5x faster than Sonnet
- Latency target: < 1s added overhead per phase
- Could also use `openai/gpt-4o-mini` for even lower cost if OpenRouter pricing favors it

The model name should be configurable (e.g. in `.open-mpm/agents/pm.toml` under a
`[skill_selection]` section or as an env var `SKILL_SELECTOR_MODEL`).

### 5.5 Response Parsing

```rust
// Parse the JSON array from the LLM response, tolerating markdown fences.
fn parse_skill_names(content: &str) -> Vec<String> {
    // Strip ```json ... ``` fences if present.
    let cleaned = content
        .trim()
        .trim_start_matches("```json")
        .trim_start_matches("```")
        .trim_end_matches("```")
        .trim();
    serde_json::from_str::<Vec<String>>(cleaned)
        .unwrap_or_default()
        .into_iter()
        // Validate: only return names that actually exist in the registry.
        .filter(|name| registry.skills.iter().any(|s| &s.name == name))
        .take(max_skills)
        .collect()
}
```

The validation step (filter by known names) prevents hallucinated skill names from
silently producing empty `resolve_skill_path` lookups.

### 5.6 Caching Strategy

Cache by `(task_fingerprint, skill_index_fingerprint)` to avoid re-calling for
identical inputs across phases:

```rust
type SkillCache = Arc<Mutex<HashMap<(u64, u64), Vec<String>>>>;
```

- `task_fingerprint`: FxHash of the first 512 chars of the task text (stable enough
  within a workflow run; full task is expensive to hash on every call)
- `skill_index_fingerprint`: computed once at registry load time (hash of all skill names)

Most workflows run the same task through 4–6 phases; caching avoids 5 redundant calls.

### 5.7 Fallback Guarantees

The LLM call must be wrapped with:
1. A hard timeout (`tokio::time::timeout(Duration::from_secs(5), ...)`)
2. A try-catch on the `?` that returns the keyword-matching result instead of propagating
3. Logging at `warn` level so the operator can see when fallback is active

The keyword matching code in `detect_languages`, `detect_frameworks`, and
`detect_workflow_skills` should be **kept** as the fallback, not deleted.

### 5.8 Thread of Task Text

The raw user task text is available throughout:
- `engine.rs` line 615: `ctx.task = cleaned_task` (persona tags stripped, otherwise verbatim)
- The rendered phase template `rendered` at line 802 contains `{{task}}` expanded
- Both are passed to `build_skills_prefix_tracked` — the rendered template is the richer
  signal (it may contain phase-specific instructions that sharpen context)

For skill selection, **use `rendered`** (the already-rendered phase task) rather than
`ctx.task`, because by the time `build_skills_prefix_tracked` is called, `rendered`
already has the phase context substituted. This gives the LLM the most specific signal.

---

## 6. Implementation Sequencing

1. **Add `llm_client: Option<Arc<OpenAIClient>>` to `SkillsLoader`** — thread the same
   client instance the engine already holds. Gate on `Some(_)` so tests without a client
   fall through to keyword matching automatically.

2. **Implement `llm_select_skills`** as an async free function in `src/skills/mod.rs`.
   Keep it small and unit-testable with a mock client.

3. **Wire the cache** — `SkillsLoader` already has a `Mutex<HashMap>` for file content;
   add a parallel `Mutex<HashMap<(u64,u64), Vec<String>>>` for LLM results.

4. **Feature-flag via config** — add `[skill_selection] use_llm = false` to
   `.open-mpm/configuration.yaml` so operators can opt in. Default `false` until
   validated in production runs.

5. **Metrics** — emit a tracing event `skill_selection_method = "llm" | "keyword"` so
   the perf collector can track fallback rate.

---

## 7. What NOT to Change

- `TaskSignals::extract` feeds agent **routing** (which agent runs the task), not skill
  injection. It should stay keyword-based for determinism and zero latency on the hot path.
- `SkillRegistry.search` (Path C / legacy auto_inject) can remain keyword-based; it is
  already superseded by Path A when a `SkillsLoader` is attached.
- The skill file format (frontmatter + markdown body) does not need to change.
- The `discover_skills_for_task` path (Path B, plan-phase summary) can be migrated
  separately or left as-is; it only provides summaries, not injected bodies.
