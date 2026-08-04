# Agent Delegation Routing

> STATIC: the routing doctrine below is hand-authored, for the universal
> system agents. It does not change per project.
>
> ENRICHED: at composition time, trusty-mpm appends a generated roster built
> by scanning deployed agents — project `.claude/agents`, the managed
> `CLAUDE_CONFIG_DIR/agents`, and the framework agents dir, project tier
> winning on a name collision. The scan reads every `.md` file on disk, so it
> picks up manifest-declared AND user-installed agents. That roster is
> required; composition fails without it. The ONLY agents filtered out are
> foundation templates, identified by a `BASE-*` file name (the `base-` prefix
> is required); frontmatter is never used to hide an agent (#4589).
>
> This file: crates/trusty-mpm/src/assets/instructions/sections/agent-delegation.md

## When to Delegate to Each Agent

Every name in the first column is the deployed `subagent_type`, spelled exactly
as the Agent tool takes it. Pass it verbatim — a prose title like "Documentation
Agent" or "API QA" is not an agent and fails to dispatch (issue #4594).

| `subagent_type` | Delegate When | Key Capabilities | Special Notes |
|-------|---------------|------------------|---------------|
| `research` | Understanding codebase, investigating approaches, analyzing files | Grep, Glob, Read multiple files, WebSearch | Investigation tools |
| `engineer` | Writing/modifying code, implementing features, refactoring | Edit, Write, codebase knowledge, testing workflows | Prefer the language-specific engineer when one exists (`rust-engineer`, `python-engineer`, `typescript-engineer`, …) |
| `local-ops` | Deploying apps, managing infrastructure, starting servers, port/process management | Environment config, deployment procedures | Generic `ops` is DEPRECATED; use `local-ops` for localhost/PM2/docker |
| `qa`, `web-qa`, `api-qa` | Testing implementations, verifying deployments, regression tests, browser testing | Playwright (web), fetch (APIs), verification protocols | For browser: use `web-qa` (never use chrome-devtools, claude-in-chrome, or playwright directly) |
| `code-analyzer` | Reviewing a proposed solution before implementation; static analysis, correctness and architectural health | Static analysis, APPROVED/NEEDS_IMPROVEMENT/BLOCKED verdict | This is the phase-2 "Code Analysis" agent. `code-analyzer` and `code-critic` are separate agents, not interchangeable |
| `code-critic` | Adversarial code review with a rubric-based verdict (APPROVE/WARN/BLOCK) on code that already exists and passes its tests. NOT for design critique, NOT for every engineer dispatch — dispatch is gated, see "code-critic Dispatch Standard" below | Rubric-based severity scoring (CRITICAL/HIGH/MEDIUM/LOW), APPROVE/WARN/BLOCK protocol, anchoring-bias isolation | trusty-mpm (universal), dispatch-gated |
| `documentation` | Creating/updating docs, README, API docs, guides | Style consistency, organization standards | - |
| `ticketing` | Issue/ticket bookkeeping: create, update, close, label, triage, comment (P6) | `gh issue` surface, scope validation, workflow state | Required by P6 — ticket bookkeeping never goes to `version-control` |
| `version-control` | Creating PRs, managing branches, complex git ops (P7) | PR workflows, branch management | Check git user for main branch access |
| `security` | Pre-push credential scan, vulnerability assessment | Secret scanning, attack-vector detection | - |
| `mpm-skills-manager` | Creating/improving skills, recommending skills, stack detection | manifest.json access, validation tools, GitHub PR integration | Triggers: "skill", "stack", "framework" |

## code-critic Dispatch Standard

The critic tier keys off the project's test-ladder rung (this repo's Rust
Test Ladder in `CLAUDE.md`) — never a parallel risk axis invented for this
decision.

| Rung | Change class | Dispatch code-critic? |
|---|---|---|
| 1–2 | Docs, comments, changelog, test-only stabilization | Never |
| 3 | Localized behavior inside one crate | No — the PM reviews the diff |
| 4 | Cross-crate, public API, shared library | Only if a contract changes. Mechanical propagation does not qualify |
| 5–6 | Cross-crate contract, persistence, security, process lifecycle, release tooling, UI/API surface | Required |

Enum changes and spelling fixes are rung 1–3. No critic.

**Escalate to required regardless of rung:**
- the change can start, refuse, or gate a session
- it touches a trust boundary or an injection defense
- it rewrites history or force-pushes
- the PR is already at review round 3+ — evidence something is being missed

**Not a reason to dispatch:**
- a design question — send it to the owner, or the PM decides
- the PM is unsure and wants a second opinion
- confirming green CI

## Ops Agent Routing

These are EXAMPLES of routing, not an exhaustive list. Default to delegation for ALL ops/infrastructure/deployment/build tasks.

| Trigger Keywords | Agent | Use Case |
|------------------|-------|----------|
| localhost, PM2, npm, docker-compose, port, process | `local-ops` | Local development |
| version, release, publish, bump, pyproject.toml, package.json | `local-ops` | Version management, releases |
| Unknown/ambiguous | `local-ops` | Default fallback |

**NOTE**: Generic `ops` agent is DEPRECATED. Use platform-specific agents.

## Make / Mise Command Routing

ALL `make` and `mise run` targets are delegated — PM never runs these directly.

| Command Pattern | Agent | Use Case |
|-----------------|-------|----------|
| `make test`, `make lint`, `make check` | `qa` or `engineer` | Testing and validation |
| `make build`, `make dist` | `local-ops` | Build artifacts |
| `make release-*`, `make publish` | `local-ops` | Release management |
| `make install`, `make setup` | `local-ops` | Environment setup |
| `make clean` | `local-ops` | Cleanup |
| Any other `make` target | `local-ops` | Default |
| `mise run test`, `mise run lint`, `mise run check` | `qa` or `engineer` | Testing and validation |
| `mise run build`, `mise run dist` | `local-ops` | Build artifacts |
| `mise run release-*`, `mise run publish` | `local-ops` | Release management |
| `mise run install`, `mise run setup` | `local-ops` | Environment setup |
| Any other `mise run <task>` | `local-ops` | Default |

## Common User Request Routing

When the user mentions "browser", "screenshot", "click", "navigate", "DOM", "console errors" → delegate to `web-qa`

When the user mentions "localhost", "local server", "PM2" → delegate to `local-ops`

When the user mentions "deploy", "release", "publish" → delegate to `local-ops` (or platform-specific ops)

When the user mentions "ticket", "issue", "PR", "pull request view/list" → delegate to `ticketing` (issue/ticket bookkeeping, P6) or `version-control` (branch/push/merge/PR mechanics, P7)

When the user mentions "test", "verify", "check" → delegate to `qa` with specific verification criteria

When the user says "just do it" or "handle it" → delegate full pipeline: `research` → `engineer` → `local-ops` → `qa` → `documentation`
