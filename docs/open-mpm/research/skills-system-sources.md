# Skills System Sources Research

**Date**: 2026-04-23
**Objective**: Document available skill sources for the open-mpm skills system and map them to open-mpm's integration needs.

---

## 1. skillset-mcp Format (mcp-skillset project)

The canonical skill repository is at `/Users/masa/Projects/claude-mpm-skills` (not `skillset-mcp`). The MCP server implementation lives at `/Users/masa/Projects/mcp-skillset`. These are two separate projects:

- `mcp-skillset` — Python MCP server that indexes and serves skills over the MCP protocol
- `claude-mpm-skills` — The actual skill content repository (289 SKILL.md files)

### File Format

Each skill is a directory containing:
- `SKILL.md` — Main content file with YAML frontmatter + markdown body (required)
- `metadata.json` — Redundant structured index (used by manifest generation scripts)
- `references/` — Optional directory of supplementary markdown files (e.g. `anti-patterns.md`, `workflow.md`)

### Frontmatter Fields (YAML between `---` delimiters)

```yaml
name: axum                          # required: short slug name
description: "..."                   # required: one-line description (min 10 chars)
user-invocable: false               # whether user can invoke directly
disable-model-invocation: true      # prevents LLM from calling skill as a tool
version: 1.0.0                      # optional semver
category: toolchain                 # see categories below
author: Claude MPM Team             # optional
license: MIT                        # optional SPDX identifier
compatibility: claude-code          # optional compatibility string (max 500 chars)
allowed_tools: []                   # optional space-delimited tool list
tags:                               # list of searchable tags
  - rust
  - axum
  - tokio
requires_tools: []                  # tool dependencies
progressive_disclosure:             # structured entry point metadata
  entry_point:
    summary: "..."                  # concise one-line capability summary
    when_to_use: "..."              # trigger conditions
    quick_start: "..."             # numbered steps
  token_estimate:
    entry: 140                     # tokens for entry_point section
    full: 5500                     # tokens for full skill
  references:                      # list of reference files in references/
    - anti-patterns.md
    - workflow.md
context_limit: 700                 # max context tokens when injected
```

### Directory Structure

```
claude-mpm-skills/
├── manifest.json               # Generated index (161 skills catalogued, 289 SKILL.md files total)
├── toolchains/
│   ├── ai/
│   │   ├── frameworks/         # dspy, langchain, langgraph
│   │   ├── ops/                # local-llm-ops
│   │   ├── protocols/          # mcp
│   │   ├── sdks/               # anthropic
│   │   ├── services/           # openrouter
│   │   └── techniques/         # session-compression, vector-search-workflows
│   ├── databases/              # mongodb
│   ├── elixir/                 # phoenix, ecto, liveview
│   ├── golang/                 # cli, concurrency, data, grpc, observability, testing, web
│   ├── java/                   # spring-boot
│   ├── javascript/             # express, hono (7 sub-skills), nextjs, react, svelte, sveltekit, vite
│   ├── nextjs/                 # (also here)
│   ├── php/                    # core, laravel, testing
│   ├── platforms/              # deployment (netlify, vercel)
│   ├── python/
│   │   ├── async/              # asyncio, celery
│   │   ├── data/               # sqlalchemy
│   │   ├── frameworks/         # django, fastapi-local-dev (6 refs), flask
│   │   ├── testing/            # pytest
│   │   ├── tooling/            # mypy, pyright
│   │   └── validation/         # pydantic
│   ├── rust/
│   │   ├── cli/                # clap
│   │   ├── desktop-applications/
│   │   └── frameworks/         # axum, tauri
│   ├── typescript/
│   │   ├── api/                # trpc
│   │   ├── build/              # turborepo
│   │   ├── core/               # (6 reference files)
│   │   ├── data/               # drizzle, drizzle-migrations, kysely, prisma
│   │   ├── frameworks/         # fastify, nodejs-backend
│   │   ├── state/              # tanstack-query, zustand
│   │   ├── testing/            # jest, vitest
│   │   └── validation/         # zod
│   ├── ui/                     # daisyui, headlessui, shadcn, tailwind
│   ├── universal/              # dependency/audit, docker, emergency/release, github-actions, homebrew, security/api-review
│   └── visualbasic/            # core, winforms, adonet, vb6-interop
└── universal/
    ├── architecture/           # software-patterns (5 refs)
    ├── collaboration/          # brainstorming, dispatching-parallel-agents, git-workflow, git-worktrees, requesting-code-review, stacked-prs, writing-plans
    ├── data/                   # database-migration, json-data-handling, reporting-pipelines, sec-edgar-pipeline, xlsx
    ├── debugging/              # root-cause-tracing, systematic-debugging, verification-before-completion
    ├── infrastructure/         # env-manager, kubernetes, terraform
    ├── main/                   # artifacts-builder, internal-comms, mcp-builder, skill-creator
    ├── observability/          # opentelemetry
    ├── orchestration/          # mpm-orchestration-demo
    ├── security/               # security-scanning, threat-modeling
    ├── testing/                # condition-based-waiting, test-driven-development, test-quality-inspector, testing-anti-patterns, webapp-testing
    ├── verification/           # bug-fix, pre-merge, screenshot
    └── web/                    # api-design-patterns, api-documentation, web-performance-optimization
```

### Total Skills

- **289 SKILL.md files** total in the `claude-mpm-skills` repository
- **161 skills** catalogued in the current `manifest.json`
- The gap reflects skills that exist but haven't been indexed in the latest manifest run

### Valid Categories (mcp-skillset validation schema)

The `mcp-skillset` validator (`SkillValidator.VALID_CATEGORIES`) enforces these categories for the MCP server use case:
`testing`, `debugging`, `refactoring`, `documentation`, `security`, `performance`, `deployment`, `architecture`, `data-analysis`, `code-review`, `collaboration`

The `claude-mpm-skills` repo itself uses broader categories: `toolchain`, `universal`, and custom values like `ai-service`, `agent-protocol`.

### Index / Cache Mechanism

The `mcp-skillset` server uses:
- **ChromaDB** vector store at `~/.mcp-skillset/storage/` for semantic search
- **NetworkX** knowledge graph for dependency/relationship traversal
- **In-memory dict cache** for loaded skill objects
- **`manifest.json`** in the repo root as a pre-generated flat index (token counts, tags, paths)
- **Hybrid search** (70% vector + 30% graph by default, configurable via `config.yaml`)

---

## 2. Other Skill Sources Found

### ~/.claude/skills/ — Claude Code User-Level Skills

These are skills deployed to Claude Code's user-level skill directory. All are mpm-related operational skills, not domain content skills:

```
claude-mpm/           dalle-image-generation/    gemma-local-apple/
hyperdev-article-workflow/  masas-casual-comms/  mcp-vector-search-pr-mr-skill/
mpm/                  mpm-agent-update-workflow/ mpm-bug-reporting/
mpm-circuit-breaker-enforcement/  mpm-config/   mpm-delegation-patterns/
mpm-doctor/           mpm-git-file-tracking/     mpm-help/
mpm-init/             mpm-message/               mpm-postmortem/
mpm-pr-workflow/      mpm-session-management/    mpm-session-pause/
mpm-session-resume/   mpm-status/                mpm-teaching-mode/
mpm-ticket-view/      mpm-ticketing-integration/ mpm-tool-usage-guide/
mpm-verification-protocols/  nifi-workflows-team-skill/  vector-search/
vector-search-pr-mr-skill/   writing-style-bob-matsuoka/
```

None of these are directly relevant to open-mpm's domain skill injection use case. They are Claude Code harness orchestration skills, not sub-agent content skills.

### /Users/masa/Projects/claude-mpm-agents/skills/

Contains a mix of custom research and domain skills:
- `caveman-prompt-compression` — prompt compression technique
- `java-algorithm-patterns`, `java-async-concurrent`
- `python-algorithm-cookbook`, `python-async-patterns`, `python-di-soa-patterns`
- `research-google-workspace`, `research-mcp-skillset`, `research-ticketing-protocol` (agent protocol skills)

The Python skills here (`python-async-patterns`, `python-algorithm-cookbook`, `python-di-soa-patterns`) are dense reference cards — simpler format than `claude-mpm-skills`.

### /Users/masa/Projects/rustbot/skills/ and /Users/masa/Projects/trusty-izzie/skills/

Both contain categorical subdirectories with JSON-format tool skills (browser-automation, web-search, etc.) rather than markdown skill cards. The `rustbot/skills/` directory also contains higher-level skills: `caveman-prompt-compression`, `java-algorithm-patterns`, `python-async-patterns`, etc.

### /Users/masa/Projects/mcp-ticketer/skills/

Not explored in depth — likely contains ticketing workflow skills.

### .mcp.json Skill References

The `open-mpm/.mcp.json` references `mcp-vector-search` and `kuzu-memory` MCP servers — no skill repositories referenced directly. The `~/.mcp.json` is empty (`{}`). No MCP config directly references `mcp-skillset` for the open-mpm project.

---

## 3. Mapping to open-mpm Categories

### Current open-mpm Skills (`config/skills/`)

```
fixture-quality.md           frameworks/fastapi.md
frameworks/pytest.md         git-operations.md
languages/python.md          languages/rust.md
python-packaging.md          python-testing.md
workflow/docker.md           workflow/tdd.md
workflow/wave-planning.md
```

Format: flat YAML frontmatter (name, description, tags) + markdown body. No `progressive_disclosure` structure. No `metadata.json` sidecars.

### Skill Inventory: claude-mpm-skills vs. open-mpm Needs

| Skill (claude-mpm-skills path) | Category | Usable as-is? | Notes |
|---|---|---|---|
| `toolchains/rust/frameworks/axum/SKILL.md` | rust | Yes | Tokio + tower + middleware patterns directly relevant to open-mpm's HTTP/IPC layer |
| `toolchains/rust/cli/clap/SKILL.md` | rust | Yes | Relevant if open-mpm adds CLI interface |
| `toolchains/rust/desktop-applications/SKILL.md` | rust | Partial | Too GUI-focused; tokio patterns usable |
| `toolchains/ai/services/openrouter/SKILL.md` | ai-service | Yes | Directly relevant — open-mpm calls OpenRouter. Covers streaming, function calling, model routing |
| `toolchains/ai/sdks/anthropic/SKILL.md` | ai-sdk | Yes | Relevant for `use_anthropic_direct = true` path |
| `toolchains/ai/protocols/mcp/SKILL.md` | ai-protocol | Partial | MCP TypeScript/Python-focused; architectural patterns transferable |
| `toolchains/ai/techniques/session-compression/SKILL.md` | ai-technique | Yes | Directly relevant for managing long agent conversations |
| `toolchains/python/frameworks/fastapi-local-dev/SKILL.md` | python | Yes (for python sub-agents) | For python-engineer sub-agent's skill context |
| `toolchains/python/testing/pytest/SKILL.md` | python | Yes (for python sub-agents) | For python-engineer sub-agent |
| `toolchains/python/validation/pydantic/SKILL.md` | python | Yes (for python sub-agents) | For python-engineer sub-agent |
| `toolchains/python/frameworks/django/SKILL.md` | python | Yes (for python sub-agents) | Broader coverage for python sub-agent |
| `toolchains/python/frameworks/flask/SKILL.md` | python | Yes (for python sub-agents) | Lighter alternative to FastAPI for sub-agents |
| `toolchains/python/async/asyncio/SKILL.md` | python | Yes (for python sub-agents) | Async patterns for python sub-agent |
| `toolchains/python/tooling/mypy/SKILL.md` | python | Yes (for python sub-agents) | Type safety skill for python sub-agent |
| `universal/testing/test-driven-development/SKILL.md` | testing | Yes | Drop-in replacement/upgrade for current `workflow/tdd.md` — much richer content |
| `universal/debugging/systematic-debugging/SKILL.md` | debugging | Yes | High-value universal debugging methodology |
| `universal/debugging/verification-before-completion/SKILL.md` | debugging | Yes | Pre-flight check pattern useful for all agents |
| `universal/architecture/software-patterns/SKILL.md` | architecture | Yes | For PM orchestrator context |
| `universal/collaboration/dispatching-parallel-agents/SKILL.md` | orchestration | Yes | Directly relevant to wave-planning / agent delegation |
| `universal/infrastructure/env-manager/SKILL.md` | infrastructure | Yes | .env.local handling patterns |
| `toolchains/universal/infrastructure/docker/SKILL.md` | workflow | Yes | Upgrade to current `workflow/docker.md` |
| `toolchains/universal/infrastructure/github-actions/SKILL.md` | workflow | Yes | CI/CD skill for open-mpm |
| `universal/security/security-scanning/SKILL.md` | security | Partial | General security patterns; cargo-specific tooling not covered |
| `toolchains/ai/frameworks/langgraph/SKILL.md` | ai | No | LangGraph is Python-specific; not relevant to Rust harness |
| `toolchains/javascript/` (all) | javascript | No | Not relevant to open-mpm's Rust+Python stack |
| `toolchains/typescript/` (all) | typescript | No | Not relevant |
| `toolchains/visualbasic/` (all) | visualbasic | No | Not relevant |

---

## 4. Recommended Integration Strategy

### Option A: Direct File Copy (Minimal Friction)

Copy selected `SKILL.md` files directly into `config/skills/` with adaptation:

1. Strip the `progressive_disclosure` frontmatter block — open-mpm's skill injector reads the full file, not the MCP-specific structured metadata
2. Retain `name`, `description`, `tags`, and optionally `version`/`author`/`license`
3. Organize into open-mpm's existing category structure: `languages/`, `frameworks/`, `workflow/`

**Effort**: Low. Suitable for 5-10 targeted skills.

### Option B: Adopt claude-mpm-skills Format (Recommended for Scale)

Align open-mpm's skill format with the `claude-mpm-skills` standard:

1. Add `progressive_disclosure.entry_point` to all existing skills — provides token-efficient injection (entry ~100 tokens vs full ~5000)
2. Add `context_limit` field to control injection size per agent turn
3. Create `references/` subdirectories for large skills that benefit from on-demand loading
4. Generate a `manifest.json` for fast lookup without parsing every file

**Benefits**:
- Skills become compatible with `mcp-skillset` MCP server out of the box
- Enables token-aware injection: use entry_point summary in short turns, full content for deep work
- Aligns with the ecosystem's de facto standard

**Effort**: Medium. Requires updating 11 existing skills + adding tooling.

### Option C: Reference via mcp-skillset MCP Server

Add `mcp-skillset` as an MCP server in `open-mpm/.mcp.json` and fetch skills on demand from the `claude-mpm-skills` repository via the MCP protocol:

```json
{
  "mcpServers": {
    "mcp-skillset": {
      "type": "stdio",
      "command": "uv",
      "args": ["run", "--directory", "/Users/masa/Projects/mcp-skillset", "mcp-skillset", "mcp"]
    }
  }
}
```

**Benefits**: Access to all 289 skills with semantic search; no file maintenance.
**Drawbacks**: Adds Python dependency + MCP server process; overkill for current POC phase.

### Immediate Recommendations (Phase 1)

Priority skills to copy into `config/skills/` now (Option A, targeted):

1. `toolchains/ai/services/openrouter/SKILL.md` → `config/skills/services/openrouter.md`
   - Directly covers open-mpm's primary LLM integration point
2. `toolchains/rust/frameworks/axum/SKILL.md` → `config/skills/languages/rust-axum.md`
   - Even if open-mpm doesn't use axum today, tokio + tower patterns are directly applicable
3. `universal/testing/test-driven-development/SKILL.md` → replace `config/skills/workflow/tdd.md`
   - Significantly richer than current stub; covers Rust + Python test patterns
4. `universal/collaboration/dispatching-parallel-agents/SKILL.md` → `config/skills/workflow/agent-delegation.md`
   - Core to open-mpm's PM → sub-agent delegation model
5. `toolchains/ai/techniques/session-compression/SKILL.md` → `config/skills/workflow/session-compression.md`
   - Important for managing long-running agent sessions without token blowup

### Format Adaptation Required

The `claude-mpm-skills` SKILL.md format is designed for Claude Code skill injection (agent-level system prompt enrichment), not for open-mpm's sub-agent prompt building. The key adaptation:

- **Claude Code skills**: Injected into Claude Code's own context at startup; `progressive_disclosure` metadata controls what Claude Code loads
- **open-mpm skills**: Injected into sub-agent system prompts via TOML config + file reads; no MCP skill-loading mechanism yet

The simplest integration: strip `progressive_disclosure`, `user-invocable`, `disable-model-invocation` frontmatter fields and inject the markdown body content directly into agent system prompts. The content quality in `claude-mpm-skills` skills is high and will work as plain markdown reference material regardless of the metadata framing.

---

## Summary

| Metric | Value |
|---|---|
| Primary skill source | `claude-mpm-skills` (`/Users/masa/Projects/`) |
| Total skills available | 289 SKILL.md files |
| Skills directly usable for open-mpm (Rust/AI) | ~8 (axum, clap, openrouter, anthropic-sdk, session-compression, MCP protocol, asyncio, ai techniques) |
| Skills for python sub-agent injection | ~8 (fastapi, pytest, pydantic, django, flask, asyncio, mypy, sqlalchemy) |
| Skills usable as universal workflow guidance | ~10 (TDD, systematic-debugging, verification, dispatching-agents, docker, github-actions, env-manager, software-patterns, security-scanning) |
| Format compatibility with open-mpm | Partial — frontmatter needs stripping; content directly usable |
| Recommended next step | Copy 5 priority skills (Option A), plan format alignment (Option B) for Phase 2 |
