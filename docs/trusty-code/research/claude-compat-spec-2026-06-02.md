# Claude Code Compatibility Specification for tcode

**Status**: Normative — study complete 2026-06-02  
**Epic**: [#587](https://github.com/bobmatnyc/trusty-tools/issues/587) — trusty-code (tcode) extraction + Claude-Code compatibility workstream  
**Crate**: `crates/trusty-code/` (binary `tcode`; formerly `trusty-harness` / `tharn`)  
**Sources**: code.claude.com/docs (settings, memory, sub-agents, skills, mcp, hooks); platform.claude.com Agent SDK — verified 2026-06-02

---

## Table of Contents

1. [Overview and Scope](#1-overview-and-scope)
2. [Configuration Surface Inventory](#2-configuration-surface-inventory)
3. [Orchestration and Agent Model](#3-orchestration-and-agent-model)
4. [Compatibility Matrix](#4-compatibility-matrix)
5. [Constraints — Decided](#5-constraints--decided)
6. [Implementation Notes for the Rust Harness](#6-implementation-notes-for-the-rust-harness)
7. [Critical Gotchas](#7-critical-gotchas)
8. [Compatibility Sub-Issue Breakdown C1–C10](#8-compatibility-sub-issue-breakdown-c1c10)
9. [Sources](#9-sources)

---

## 1. Overview and Scope

`trusty-code` (binary `tcode`) is a **Claude-Code-compatible, single-instance,
per-project execution harness** with the MPM orchestration brain replacing
Claude Code's free-form main agent.

### What "single-instance, per-project" means

- tcode runs **exactly one project** at a time — it is not a daemon managing
  multiple projects concurrently. The `open-mpm` daemon is the multi-project
  coordinator; it spawns each tcode instance as a **separate OS process** and
  communicates over a per-project **socket/NDJSON IPC** channel (reusing the
  existing `ctrl/socket` + `ipc/` NDJSON code).
- This is architecturally distinct from `trusty-mpm`, which manages *external*
  Claude Code sessions (hooks, overseer, circuit breaker — no LLM calls, no
  workflow engine). tcode runs *internal* sub-agents with its own LLM stack,
  NDJSON IPC, and full prescriptive workflow engine.

### What "Claude-Code-compatible" means

When tcode is run inside a project that contains a `.claude/` directory, it
picks up **user-level** (`~/.claude/`) AND **project-level** configuration —
agents, skills, MCP integrations, slash commands, `CLAUDE.md`, permissions,
`settings.json` — with the **same file paths, the same precedence rules, and
the same merge semantics** as Claude Code itself.

On top of that configuration compatibility, tcode executes the **MPM
orchestration brain**: deterministic Research → Plan → Code → QA workflow
phases with mandatory verification gates, circuit breakers, PM-as-main-loop
control, and delegated authority to specialist agents.

The compatibility scope for epic #587 covers **everything listed in the
configuration surface inventory below** except hooks, which are explicitly
out of scope (see [Section 5](#5-constraints--decided)).

### Name history

The crate was referred to as `trusty-harness` and `tharn` in earlier comments
on epic #587. Those names were superseded on 2026-06-02:
**`trusty-code`** / **`tcode`** is the final name. All sub-issues #639–#647
that reference "trusty-harness"/"tharn" now mean trusty-code/tcode. The design
is unchanged.

---

## 2. Configuration Surface Inventory

tcode must read and honor all of the following surfaces.

### 2.1 settings.json

**Purpose**: model selection, environment variable injection, permission rules,
tool behaviour flags, and (documented but ignored) hooks.

| Layer | File path | Typical use |
|-------|-----------|-------------|
| Managed / Enterprise | `~/.claude/managed_settings.json` (exact path may vary by deployment) | Org-level policy; cannot be overridden |
| User | `~/.claude/settings.json` | Personal defaults |
| Project | `<project-root>/.claude/settings.json` | Per-project settings; committed |
| Project-local | `<project-root>/.claude/settings.local.json` | Personal project overrides; gitignored |

**Precedence (highest to lowest)**: Managed > User > Project > Local

This is a **layered merge**: each lower layer provides defaults; higher layers
override. The merge is per-key, not wholesale replacement. For arrays (e.g.,
`permissions.allow`), see the permissions-union rule in
[Section 2.7](#27-permissions).

**Top-level keys (abridged)**:

```jsonc
{
  "model": "claude-opus-4-5",          // default model for main agent
  "env": { "KEY": "value" },           // environment variable injection
  "permissions": {
    "allow": ["Bash(git log:*)"],       // allow rules
    "deny": [],                         // deny rules
    "ask": []                           // ask-before-run rules
  },
  "hooks": { ... }                      // READ-ONLY / IGNORED in tcode
}
```

### 2.2 CLAUDE.md (Memory)

**Purpose**: persistent project instructions loaded into every agent's context.

| Location | Path | Notes |
|----------|------|-------|
| User global | `~/.claude/CLAUDE.md` | Applies to all projects |
| Project root | `<project-root>/CLAUDE.md` | Standard per-project instructions |
| Subdirectory | `<subdir>/CLAUDE.md` (any level) | Scoped to that subtree |

**Load order and precedence**:

- Files are collected by walking **from the repo root toward the current working
  directory** (parent → child).
- The file **nearest cwd wins** — child-level instructions take precedence over
  parent-level instructions. This is the **reverse** of the settings.json
  precedence direction and is Gotcha #3 (see [Section 7](#7-critical-gotchas)).

**`@import` directives**: a `CLAUDE.md` may include `@import <relative-path>`
to pull in additional files. Maximum recursion depth: **4 levels**.

**Memory truncation**: tcode truncates each `CLAUDE.md` file at approximately
**200 lines or 25 KB**, whichever is lower, measured in **lines** not tokens.
This is Gotcha #4.

### 2.3 .claude/agents/

**Purpose**: sub-agent definitions — named, delegatable agents that tcode's PM
can dispatch work to.

| Location | Path |
|----------|------|
| User-level | `~/.claude/agents/*.md` |
| Project-level | `<project-root>/.claude/agents/*.md` |

**File format**: Markdown with YAML front matter.

```yaml
---
name: security-reviewer
description: Reviews code for security vulnerabilities
model: claude-opus-4-5              # optional; overrides default
tools:                               # optional; restricts available tools
  - Read
  - Bash
---

Agent system prompt text goes here.
```

**Key front-matter fields**:

| Field | Type | Purpose |
|-------|------|---------|
| `name` | string | Machine name (used in delegation, slash commands) |
| `description` | string | Human-readable; shown in agent picker |
| `model` | string | Optional model override (Bedrock or OpenRouter ID) |
| `tools` | string list | Whitelist of tools this agent may use |

**Inheritance**: agents inherit the tool and permission set of their parent
context unless `tools` explicitly restricts it. An agent cannot *elevate*
permissions beyond the parent — see Gotcha #6 and Gotcha #7.

**tcode mapping**: each `.claude/agents/*.md` file becomes an
**MPM-delegatable agent**. The `model:` field is honored and may specify any
Bedrock or OpenRouter model (see [Section 5](#5-constraints--decided)).

### 2.4 .claude/skills/

**Purpose**: reusable prompt fragments or structured task packs that tcode's PM
and agents can invoke.

| Location | Path |
|----------|------|
| Bundled | built into tcode binary |
| User-level | `~/.claude/skills/<name>/SKILL.md` |
| Project-level | `<project-root>/.claude/skills/<name>/SKILL.md` |

**File format**: `SKILL.md` at the root of each skill directory. The file may
include a `disable-model-invocation` flag (YAML front matter or inline
directive) to suppress automatic LLM calls in pure-pipeline skills.

**tcode mapping**: `.claude/skills/` entries become **MPM skills** and are
preloaded at harness startup. Skill names are derived from the directory name.

### 2.5 .claude/commands/

**Purpose**: user-defined slash commands that can be invoked by name during a
session.

| Location | Path |
|----------|------|
| User-level | `~/.claude/commands/*.md` |
| Project-level | `<project-root>/.claude/commands/*.md` |

**File format**: Markdown. The filename (minus `.md`) becomes the command name.
Commands may accept `$ARGUMENTS` placeholder for runtime arguments.

**Namespace**: project commands take precedence over user commands of the same
name.

**tcode mapping**: slash commands are surfaced through the CLI and TUI
interfaces and mapped to MPM skill invocations.

### 2.6 .mcp.json (MCP Servers)

**Purpose**: declares MCP (Model Context Protocol) servers whose tools are
available to agents.

| Location | Path | Notes |
|----------|------|-------|
| Project | `<project-root>/.mcp.json` | Per-project MCP servers |
| User | `~/.claude.json` (legacy) / `~/.claude/mcp.json` | User-global MCP servers |

**Scope precedence**: project `.mcp.json` takes precedence over user-level MCP
config for same-named servers.

**Transport types**: `stdio` (subprocess over stdin/stdout), `http`, `sse`
(server-sent events).

**Tool naming convention**: MCP tools are available under the namespace
`mcp__<server-name>__<tool-name>`. Spaces in server or tool names are normalized
to underscores. This is Gotcha #5.

**Deferred tools**: MCP servers may advertise a large tool catalogue; tcode
should support lazy loading / deferral so that only the tool schemas needed for
the current context are fetched.

**tcode mapping**: `.mcp.json` servers are started by the **MCP bridge** within
tcode and their tools are made available to all delegated agents. See
implementation note in [Section 6.5](#65-mcp-bridge).

### 2.7 Permissions

**Purpose**: controls which tool invocations tcode allows, asks the user about,
or denies.

Permissions live inside `settings.json` under the `permissions` key at each
layer.

**Evaluation algorithm (first match wins)**:

1. Evaluate **deny** rules across all layers (deny > ask > allow).
2. Evaluate **ask** rules across all layers.
3. Evaluate **allow** rules across all layers.
4. Default: ask if none of the above match.

**Rules accumulate across scopes** (they are unioned, not replaced). A deny in
any layer blocks regardless of an allow in another layer.

**Permission rule syntax** (Claude Code `settings.json`):

```
ToolName                        # all uses of ToolName
ToolName(pattern)               # uses matching pattern
Bash(git log:*)                 # Bash calls starting with "git log"
mcp__server__tool               # specific MCP tool
```

**`bypassPermissions` / `acceptEdits` modes**: when a parent agent runs in
`bypassPermissions` mode, child agents may NOT downgrade to a more restrictive
mode. When the parent is in `acceptEdits` mode, children inherit it. These are
Gotcha #7.

**tcode mapping**: permission gating is enforced by the
**permission-gating module** before any tool execution. See
[Section 6.4](#64-permission-gating-algorithm).

### 2.8 Hooks (READ-ONLY / IGNORED)

Claude Code's `settings.json` hook system (`PreToolUse`, `PostToolUse`,
`Notification`, `Stop`, `SubagentStop`) defines shell commands that run at
lifecycle events.

**tcode does NOT execute hooks.** The `settings.json` config loader reads the
`hooks` key (to avoid parse errors on valid config files) but discards all hook
definitions without executing them.

The hook extensibility model is intentionally replaced by tcode's
**event-driven tool execution model** (see [Section 5.2](#52-event-driven-tool-model))
and interaction through tcode's API / CLI / TUI surfaces.

---

## 3. Orchestration and Agent Model

### 3.1 What tcode emulates from Claude Code

Claude Code implements an orchestration model where:

- A **main agent** receives a task and runs a free-form agentic loop.
- Sub-agents (`.claude/agents/`) can be spawned for specialised subtasks;
  they receive a task string, a tool list, a model, and a system prompt.
- Sub-agents run in **isolated worktrees** branched from the project's
  **default branch** (not the parent's HEAD). This is Gotcha #6.
- MCP tools are available to all agents that have permission to use them.
- Hooks fire at lifecycle events (pre/post-tool, stop, etc.) — **not emulated
  by tcode**.

### 3.2 How tcode maps this onto MPM

| Claude Code concept | tcode / MPM mapping |
|---------------------|---------------------|
| Main agent (free-form loop) | **PM (Project Manager) agent** — the MPM main loop |
| Sub-agent (`.claude/agents/*.md`) | MPM-delegatable agent; PM dispatches explicitly |
| Agent system prompt | Loaded from the `.md` file body |
| Agent `model:` | Per-agent Bedrock or OpenRouter model (see Section 5.3) |
| Agent `tools:` whitelist | Tool + permission inheritance enforced per agent |
| `.claude/skills/` | MPM skills; preloaded at startup |
| `.claude/commands/` | MPM skill invocations via CLI/TUI |
| `.mcp.json` | MCP bridge; tools available to delegated agents |
| Hooks | Not emulated; replaced by event-driven tool model |
| `CLAUDE.md` | Loaded into PM and delegated agent context |
| `settings.json` | Config loaded with full precedence merge |
| Permissions | Enforced by permission-gating module |

### 3.3 Where tcode's brain diverges

Claude Code uses a **free-form** main agent that decides dynamically what to do.
tcode's PM runs a **deterministic, opinionated workflow**:

1. **Research phase** — gather context, read files, search codebase (via
   trusty-search built-in).
2. **Plan phase** — produce a written plan; gate on PM approval.
3. **Code phase** — delegate implementation to specialist agents.
4. **QA / verification phase** — delegate testing/review; circuit breaker fires
   on repeated failure.

Mandatory verification gates and circuit breakers mean some work that a
free-form Claude Code session would attempt indefinitely will be halted and
escalated by tcode. This is a deliberate design choice, not a compatibility gap.

---

## 4. Compatibility Matrix

This matrix states precisely what tcode MUST honor, how it maps, and where it
diverges.

| Surface | tcode MUST honor | MPM mapping | Divergence |
|---------|-----------------|-------------|------------|
| `settings.json` precedence (Managed > User > Project > Local) | Yes — full merge with correct precedence | Loaded by config-loader module at startup | None |
| `settings.json` model selection | Yes — used as PM default model | Passed to `HarnessConfig` | Per-agent model may override via `.claude/agents` frontmatter |
| `settings.json` env injection | Yes | Injected into agent subprocess environments | None |
| `settings.json` permissions | Yes — deny/ask/allow union + first-match | Enforced by permission-gating module | None |
| `settings.json` hooks | Read but IGNORED | Parsed (no parse error), discarded | **Intentional divergence** — hooks not executed |
| `CLAUDE.md` load order (parent→child, nearest cwd wins) | Yes | Loaded into PM context + passed to delegated agents | None |
| `CLAUDE.md` `@import` (max depth 4) | Yes | Resolved at load time | None |
| `CLAUDE.md` truncation (~200 lines / 25 KB, line-based) | Yes | Same limits | None |
| `.claude/agents/*.md` discovery | Yes — user + project | Each `.md` → MPM-delegatable agent | Delegation is *explicit* (PM dispatches by name), not purely semantic matching |
| `.claude/agents` frontmatter `model:` | Yes | Bedrock or OpenRouter per agent | Per-agent provider mix/match is a tcode *extension* beyond Claude Code |
| `.claude/agents` tool inheritance | Yes — child inherits parent unless restricted | Enforced by permission-gating + tool registry | Cannot elevate beyond parent; `bypassPermissions` parent blocks child downgrade |
| `.claude/skills/` discovery + preload | Yes | MPM skills | None |
| `.claude/commands/` discovery | Yes | MPM skill invocations via CLI/TUI | None |
| `.mcp.json` server startup (stdio/http/sse) | Yes | MCP bridge | None |
| MCP tool naming (`mcp__server__tool`) | Yes — spaces → underscores | Tool registry normalises names | None |
| Repo-root resolution (walk up from cwd) | Yes | Config loader + repo-root resolver | None |
| Sub-agent worktree isolation (branch from default branch) | Yes | Worktree provisioner in ctrl/ | Explicit branching from default branch, not parent HEAD |
| Secrets / OAuth token handling | Yes — never written to disk/logs | Keychain + env-var injection | None |
| trusty-memory integration | Built-in | Native client to `:7070` daemon | Claude Code has no built-in equivalent; this is a tcode *addition* |
| trusty-search integration | Built-in | Native client to `:7878` daemon | Claude Code has no built-in equivalent; this is a tcode *addition* |

---

## 5. Constraints — Decided

These constraints are resolved as of 2026-06-02 and are not open questions.

### 5.1 No hooks

tcode does **NOT** implement the Claude Code `settings.json` hooks event system.

Rationale: hooks are considered an architectural hack — they couple the harness
to external shell scripts at fine-grained lifecycle points, making the execution
model difficult to reason about and impossible to intercept cleanly from tcode's
own tooling.

**What replaces hooks**: the **event-driven tool execution model** (see
Section 5.2). All hook-like extensibility goes through tcode's API / CLI / TUI
surfaces.

The config loader **reads** but **discards** the `hooks` key in `settings.json`
so that valid Claude Code config files do not cause parse errors.

### 5.2 Event-driven tool model

All tool usage in tcode is event-driven via an **internal event bus / stream**
over tool invocation and lifecycle events. The event types correspond roughly to
Claude Code's hook events (`PreToolUse`, `PostToolUse`, etc.) but are delivered
to in-process subscribers, not external shell scripts.

External interaction and observation happen through:

- The **tcode API** (per-project socket/NDJSON IPC; optional HTTP surface)
- The **`tcode` CLI**
- The **TUI** (feature-gated `tui` Cargo feature)

This model replaces the hook extensibility model entirely.

### 5.3 Per-agent model mix/match across Bedrock and OpenRouter

Each agent — the PM and every delegated agent — independently selects any model
via either provider:

- **AWS Bedrock**: `bedrock/us.anthropic.claude-sonnet-4-6`, any Bedrock model ID
- **OpenRouter**: any OpenRouter model slug

Provider + model selection order (highest priority first):
1. `.claude/agents` front matter `model:` field
2. MPM per-agent model config (in `HarnessConfig`)
3. `settings.json` top-level `model:` field (fallback default)

This builds on `open-mpm`'s existing `llm/` adapter layer, which already
supports both Bedrock and OpenRouter. No new provider integration is required.

### 5.4 API / CLI / TUI driven

tcode's interaction surfaces are:

- **API** — the per-project socket/NDJSON IPC inherited from open-mpm's `ctrl/`
  module; an optional HTTP surface may be added later.
- **CLI** — the `tcode` binary with subcommands:
  - `tcode serve --project <path>` — boots a per-project instance serving its
    socket
  - `tcode run-task <task>` — one-shot task execution (no socket)
  - `tcode run-workflow <spec>` — one-shot workflow execution (no socket)
- **TUI** — feature-gated `tui` Cargo feature (ratatui/crossterm, mirroring
  trusty-mpm's existing TUI feature gate).

No hook-based extensibility is provided.

### 5.5 trusty-memory and trusty-search are first-class

tcode ships **built-in clients** for both trusty-memory (`:7070`) and
trusty-search (`:7878`). Memory routing and code search are core capabilities,
not optional MCP add-ons.

An injected `MemoryBackend` trait seam (inherited from the original
`tools/native_memory/` design) is preserved for testing and alternate backends,
but the **default, day-one integration** is trusty-memory + trusty-search as
first-class components.

---

## 6. Implementation Notes for the Rust Harness

### 6.1 Config loader module layout

```
crates/trusty-code/src/
└── config/
    ├── mod.rs          # re-exports; builds HarnessConfig from all layers
    ├── settings.rs     # settings.json serde models + layered merge
    ├── memory.rs       # CLAUDE.md discovery, @import resolution, truncation
    ├── agents.rs       # .claude/agents/*.md discovery + front-matter parse
    ├── skills.rs       # .claude/skills/<n>/SKILL.md discovery + preload
    ├── commands.rs     # .claude/commands/*.md discovery
    ├── mcp.rs          # .mcp.json + user MCP serde models
    └── repo_root.rs    # walk-up cwd → .claude/ resolver
```

Each module is bounded to < 500 lines (workspace line-cap applies).

### 6.2 Precedence merge

The layered merge for `settings.json` is implemented as a series of
`Option`-preserving overlay operations:

```rust
// Pseudo-code — actual types will use serde + custom merge trait
let merged = Settings::default()
    .overlay(user_settings)
    .overlay(project_settings)
    .overlay(local_settings)
    .overlay(managed_settings);  // managed last = highest precedence
```

For permission arrays (`allow`, `deny`, `ask`), rules are **unioned** across all
layers (not replaced). The evaluation order (deny > ask > allow, first match)
is applied at runtime by the permission-gating module, not during the merge
step.

### 6.3 CLAUDE.md load order and @import resolution

1. Locate project root via repo-root resolver (walk up from cwd).
2. Collect all `CLAUDE.md` files: `~/.claude/CLAUDE.md` (user-global), then
   walk from repo root → cwd, collecting each directory's `CLAUDE.md`.
3. **Resolve `@import` directives** at each level (max depth 4). On cycle
   detection, log a warning and stop recursion.
4. Concatenate in **parent → child order**.
5. Truncate each file at ~200 lines / 25 KB (**line-based, not token-based**) —
   the child-level file (nearest cwd) is not truncated preferentially; each
   file is independently truncated before concatenation.
6. The resulting combined text is the memory context. Because child files appear
   later in the concatenated output, their instructions implicitly shadow
   parent-level instructions when the LLM gives later-in-context precedence.

### 6.4 Permission-gating algorithm

```
function gate(tool_call, rules_union):
    for rule in rules_union.deny:
        if rule.matches(tool_call):
            return DENY

    for rule in rules_union.ask:
        if rule.matches(tool_call):
            return ASK

    for rule in rules_union.allow:
        if rule.matches(tool_call):
            return ALLOW

    return ASK  // default
```

`rules_union` is the union of `permissions.deny`, `permissions.ask`,
`permissions.allow` arrays from all settings layers (Managed ∪ User ∪ Project
∪ Local). Within each list, rules from higher-precedence layers are prepended so
that a managed deny takes first-match priority over a user allow.

**`bypassPermissions` / `acceptEdits`**: when the parent context's permission
mode is `bypassPermissions`, the gate function returns ALLOW for all calls and
child agents may not downgrade this. When parent is `acceptEdits`, child agents
inherit it.

### 6.5 MCP bridge

The MCP bridge:

1. Reads `.mcp.json` (project) and user-level MCP config at startup.
2. For each server, spawns a subprocess (`stdio`) or establishes an HTTP/SSE
   connection.
3. Fetches the tool catalogue from each server (lazy / deferred: schemas are
   fetched on-demand when a tool is first needed).
4. Registers tools in the tool registry under the `mcp__<server>__<tool>`
   namespace (spaces → underscores).
5. Forwards tool calls from agents to the appropriate MCP server and returns
   results.

The bridge must handle server restarts (exponential backoff reconnect) and
graceful shutdown (drain in-flight calls before exiting).

### 6.6 Agent loader

The agent loader:

1. Discovers `*.md` files in `~/.claude/agents/` and
   `<project-root>/.claude/agents/`.
2. Parses YAML front matter (`name`, `description`, `model`, `tools`).
3. Registers each agent in the MPM agent registry as a delegatable agent.
4. Resolves `model:` to a Bedrock model ID or OpenRouter slug (see
   Section 5.3).
5. Applies tool restriction from `tools:` front matter by intersecting with the
   parent permission set (child cannot exceed parent).

---

## 7. Critical Gotchas

These are the ten highest-risk implementation pitfalls identified during the
compatibility study. Each should be addressed explicitly in the corresponding
C-series sub-issue.

### Gotcha 1 — Settings precedence (C2)

**Problem**: it is tempting to implement the merge as "last write wins" across
all files, which would make project or local settings override managed settings.

**Rule**: Managed always wins. The overlay chain must be applied in
ascending-precedence order so that managed settings are applied last (highest
priority). Never let user/project/local override managed.

### Gotcha 2 — Permissions UNION + order (C3)

**Problem**: permissions from multiple layers might be merged as a
last-writer-wins replacement, discarding rules from other layers.

**Rule**: permission rules **accumulate** (union) across all layers. A deny in
any layer takes effect regardless of an allow in another. Within the deny list,
higher-precedence-layer rules are evaluated before lower-precedence-layer rules
(managed deny fires before user deny).

### Gotcha 3 — CLAUDE.md load order is reversed vs. settings (C2)

**Problem**: since settings use Managed > User > Project > Local (lowest layer
= least specific), it is natural to assume memory files follow the same order.
They do not.

**Rule**: CLAUDE.md files are collected parent → child, and the **child
(nearest cwd) takes precedence**. A subdirectory `CLAUDE.md` overrides the
repo-root `CLAUDE.md` for work done in that subdirectory. This is the opposite
of the settings merge direction.

### Gotcha 4 — Memory truncation is LINE-based (C2)

**Problem**: implementors often assume truncation is token-based (e.g., first
N tokens fit in context window).

**Rule**: each `CLAUDE.md` is truncated at approximately **200 lines or 25 KB**,
whichever limit is hit first. The truncation is line-based, not token-based.
Implement `take_while_under_limit` in terms of line count AND byte count.

### Gotcha 5 — MCP tool-name normalization (C6)

**Problem**: MCP server names and tool names may contain spaces, hyphens, and
mixed case. Passing these through verbatim causes mismatches when agents
reference tools by the canonical `mcp__server__tool` name.

**Rule**: normalize server names and tool names when registering and when
matching: replace spaces with underscores. Hyphens are preserved as-is (Claude
Code preserves hyphens). The canonical form is `mcp__<server>__<tool>` with
spaces replaced by underscores only.

### Gotcha 6 — Subagent worktree branches from default branch, not parent HEAD (C4)

**Problem**: when provisioning a worktree for a delegated agent, it is natural
to branch from the parent agent's current HEAD (which may be on a feature
branch).

**Rule**: Claude Code branches subagent worktrees from the repository's
**default branch** (e.g., `main`), not from the parent agent's HEAD. tcode must
replicate this behavior for compatibility. The parent may be on a feature branch
while the sub-agent starts from a clean `main`.

### Gotcha 7 — Permission-mode inheritance exceptions (C3, C4)

**Problem**: an agent might try to run in `--dangerously-skip-permissions` mode
even when its parent is in `bypassPermissions` mode, assuming child modes are
independent.

**Rule**: if the parent agent runs in `bypassPermissions` mode, children
**may not downgrade** to a more restrictive mode. If parent is in `acceptEdits`
mode, children inherit it and may not override it. These modes propagate
downward through the delegation chain.

### Gotcha 8 — Secrets and OAuth token handling (C6)

**Problem**: MCP servers and agents may need OAuth tokens or API keys. Logging
these to stderr or writing them to temp files creates credential exposure.

**Rule**: tcode never writes tokens or secrets to disk or logs. Credentials are
held in memory (or the OS keychain where supported) and injected into agent
subprocess environments as environment variables. This applies to MCP server
auth tokens, LLM API keys, and any OAuth flows initiated by MCP servers.

### Gotcha 9 — Repo-root resolution (C2)

**Problem**: assuming `cwd` equals the repository root causes incorrect config
loading when `tcode` is invoked from a subdirectory.

**Rule**: walk **up** from cwd (or from the `--project <path>` argument if
provided) to find the repository root, identified by the presence of `.claude/`
or `.git/` (with `.claude/` taking priority). Do not assume cwd == repo root.
All config paths are anchored to the resolved repo root, not to cwd.

---

## 8. Compatibility Sub-Issue Breakdown C1–C10

These are a parallel axis from the 8 extraction phases (#639–#647). Each C-issue
covers a self-contained compatibility concern.

| Sub-issue | Title | Depends on | Key deliverables |
|-----------|-------|------------|-----------------|
| **C1** [#649] | Config schema + serde models | — (pure data types) | `Settings`, `AgentDef`, `SkillDef`, `CommandDef`, `McpConfig`, `ClaudeMd` Rust structs; serde/deserialize impls; unit tests against sample config files |
| **C2** [#650] | Config loader + precedence merge | C1 | `ConfigLoader`: discovers all files, applies Managed > User > Project > Local merge for settings; parent→child load + `@import` for `CLAUDE.md`; line-based truncation; repo-root resolver (Gotchas 1, 3, 4, 9) |
| **C3** [#651] | Permission model | C1, C2 | `PermissionGate`: union rules across scopes; deny > ask > allow first-match; `bypassPermissions`/`acceptEdits` inheritance (Gotchas 2, 7) |
| **C4** [#652] | `.claude/agents` → MPM-delegatable agents | C1, C2, C3 | `AgentLoader`: discovers and parses agent `.md` files; registers in MPM agent registry; per-agent Bedrock/OpenRouter model resolution; tool-restriction enforcement; worktree branching from default branch (Gotchas 6, 7) |
| **C5** [#653] | `.claude/skills` + `.claude/commands` loaders | C1, C2 | `SkillLoader` + `CommandLoader`: discovery, preloading, `disable-model-invocation` flag support; skill/command registration in MPM |
| **C6** [#654] | MCP bridge | C1, C2, C3 | `McpBridge`: server startup (stdio/http/sse); deferred tool-schema fetching; `mcp__server__tool` namespace normalization; reconnect backoff; secret/token handling (Gotchas 5, 8) |
| **C7** [#655] | Event-driven tool execution model + interaction APIs | C3, C4, C6 | Internal event bus over tool invocation/lifecycle; external observation API on per-project socket; CLI tool-event inspection; replaces the hook executor entirely |
| **C8** [#656] | Built-in trusty-memory + trusty-search integration | C4 | Native clients for trusty-memory (`:7070`) and trusty-search (`:7878`); integrated into PM research phase and memory-routing logic; `MemoryBackend` trait seam preserved for testing |
| **C9** [#657] | MPM orchestration reconciliation | C4, C5, C7, C8 | PM main loop wired to compat config: Research/Plan/Code/QA phases using loaded agents + skills + tools; mandatory verification gates; circuit breakers; explicit PM→agent delegation honoring `.claude/agents` definitions |
| **C10** [#658] | Git worktree isolation support | C4, C9 | tcode provisions a dedicated git worktree branched from the default branch for each isolated delegated agent; auto-cleans on completion; honors `.claude/agents` `isolation: worktree` directive; enables safe parallel delegation mirroring MPM PM-level worktree discipline |

---

## 9. Sources

All sources verified 2026-06-02:

- **Claude Code settings documentation**:
  `https://code.claude.com/docs/en/settings.md`
- **Claude Code memory documentation**:
  `https://code.claude.com/docs/en/memory.md`
- **Claude Code sub-agents documentation**:
  `https://code.claude.com/docs/en/sub-agents.md`
- **Claude Code skills documentation**:
  `https://code.claude.com/docs/en/skills.md`
- **Claude Code MCP documentation**:
  `https://code.claude.com/docs/en/mcp.md`
- **Claude Code hooks documentation**:
  `https://code.claude.com/docs/en/hooks.md`
- **Anthropic Agent SDK** (platform.claude.com):
  `https://platform.claude.com`
- **Epic #587** (primary design record):
  `https://github.com/bobmatnyc/trusty-tools/issues/587`

---

*Document generated from the compatibility study posted to epic #587,
comment "Claude Code compatibility spec for tcode (study complete — 2026-06-02)".*
