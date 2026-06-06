# ADR: Engineer Agent + Skills over Language-Specific Agents

**Status**: Accepted  
**Date**: 2026-04-26  
**Applies to**: `.open-mpm/agents/`, `.open-mpm/skills/languages/`

---

## Decision

open-mpm ships **one generic `engineer` agent** that handles all non-Python engineering tasks. Language context is supplied at runtime by injecting skill files from `.open-mpm/skills/languages/`. Adding support for a new language means adding a skill file, not a new agent file.

`python-engineer` is the single named language specialist and is the explicit exception to this rule.

---

## Context

Early prototyping considered one agent per language (rust-engineer, golang-engineer, typescript-engineer, …). That was rejected for the following reasons:

**PM complexity.** The PM must decide which agent to call. Every new specialist is another branching case the PM's LLM has to reason about. The error rate grows with the number of choices.

**Combinatorial explosion.** A real project might need a Rust engineer who also knows gRPC, or a TypeScript engineer who knows NestJS and Prisma. Multiplying specialists across language × framework dimensions is unworkable.

**Skill composition already solves this.** The `SkillRegistry` can inject multiple skill files into a single prompt. A Go + gRPC task gets `go-idiomatic.md` plus any gRPC skill. Composition is free; a new agent is not.

**Maintenance surface.** Each agent file is another TOML to keep in sync with routing tests, inspection scripts, and the PM's agent summary. One `engineer` + N skill files has a much lower maintenance cost.

**Python is the exception.** Python's packaging conventions (virtual environments, pyproject.toml layout, dependency pinning) are complex enough and divergent enough from other languages that a specialist system prompt justifies a dedicated agent. This exception was made deliberately and does not generalize.

---

## How the Routing Works

```
PM receives task
    │
    ▼
TaskSignals::extract(task)
    │  Scans lowercased task text for language / framework / role keywords.
    │  Returns: languages=["rust"], frameworks=[], role="engineer", tags=[]
    │
    ▼
AgentRegistry::best_match(role, languages, frameworks, tags)
    │  Scoring: role match = 10 pts, language match = 5 pts,
    │           framework match = 3 pts, tag match = 1 pt.
    │
    │  Disqualifier: if task has language signals AND a candidate declares
    │  a non-empty languages list with no overlap → candidate is skipped.
    │  The generic `engineer` declares languages=[] so it is never skipped.
    │
    │  Python boost: if language=="python" and candidate=="python-engineer",
    │  +5 pts so the specialist wins over the generic engineer.
    │
    ▼
`engineer` wins for non-Python tasks
    │
    ▼
SkillRegistry injects matching language skill(s)
    │  skills=["auto"] → registry finds skills whose tags match task signals.
    │  go task → go-idiomatic.md injected into engineer's system prompt.
    │  typescript task → typescript-idiomatic.md injected.
    │
    ▼
engineer executes with full language context in its prompt
```

Source references:
- `src/inspection/task_signals.rs` — `TaskSignals::extract`
- `src/agents/registry.rs` — `AgentRegistry::best_match` (see inline comments for the disqualifier and python boost logic)
- `src/skills/registry.rs` — `SkillRegistry`, tag-indexed skill lookup
- `src/agents/prompt_builder.rs` — skill injection into system prompt

---

## The Correct Pattern: Adding Language Support

1. Create `.open-mpm/skills/languages/<language>-idiomatic.md`.
2. Add YAML frontmatter tags that match what `TaskSignals::extract` will emit for that language. For example:
   ```yaml
   ---
   tags: [go, golang, idiomatic]
   description: Idiomatic Go patterns for open-mpm engineer agent
   ---
   ```
3. Write the skill content: language idioms, toolchain conventions, import style, common pitfalls.
4. Run `cargo test` and verify `registry_best_match_uses_engineer_for_non_python` still passes.
5. Optionally run `./tests/harness/run_inspection.sh` for a full routing smoke test.

No changes to `src/` are required for a new language. No new agent TOML.

---

## The Anti-Pattern (Reverted)

During a routing bug fix, six agent files were created:

```
.open-mpm/agents/rust-engineer.toml
.open-mpm/agents/golang-engineer.toml
.open-mpm/agents/typescript-engineer.toml
.open-mpm/agents/javascript-engineer.toml
.open-mpm/agents/java-engineer.toml
.open-mpm/agents/ruby-engineer.toml
```

These were reverted. The routing bug was in the scoring logic in `AgentRegistry::best_match`, not in the absence of specialist agents. The fix belonged in `src/agents/registry.rs`, not in new TOML files.

Creating language-specialist agents is the wrong fix for routing problems. When a task routes to the wrong agent, check:
1. `TaskSignals::extract` — is the language being detected?
2. `AgentRegistry::best_match` scoring — is the disqualifier or boost logic wrong?
3. The skill injection path — is the right skill being found and injected?

---

## Skill Composition Examples

**Go + gRPC task**: `TaskSignals::extract` emits `languages=["go"], frameworks=["grpc"]`. `best_match` selects `engineer`. `SkillRegistry` injects `go-idiomatic.md` plus any skill tagged `grpc`. The engineer's prompt contains both language idioms and gRPC conventions.

**TypeScript + NestJS task**: Signals `languages=["typescript"], frameworks=["nestjs"]`. Engineer selected. `typescript-idiomatic.md` + NestJS skill injected. One agent, composed context.

**Rust task**: Signals `languages=["rust"]`. Engineer selected. `rust-idiomatic.md` injected (or `rust.md` if that matches better by tag). No rust-engineer agent needed.

**Python + FastAPI task**: Signals `languages=["python"], frameworks=["fastapi"]`. `python-engineer` wins via the +5 python boost. `python-packaging.md`, `python-idiomatic.md`, and any FastAPI skill injected on top of python-engineer's specialist system prompt.

---

## Future Specialists

Adding a second language specialist (beyond `python-engineer`) requires:
1. A written justification in this document explaining why skill injection is insufficient.
2. A corresponding update to the routing tests in `src/agents/registry.rs`.
3. Explicit approval — do not add specialists opportunistically.

Candidates that would justify a specialist: a language with fundamentally different toolchain conventions that cannot be captured in a skill file, or a domain (security, data engineering) where the system prompt itself must differ substantially from the generic engineer's operating principles.
