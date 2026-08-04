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

## Routing Table

Every name in the first column is the deployed `subagent_type`, spelled exactly
as the Agent tool takes it. Pass it verbatim — a prose title like "Documentation
Agent" or "API QA" is not an agent and fails to dispatch (issue #4594).

These are EXAMPLES of routing, not an exhaustive list. Default to delegation for
ALL ops / infrastructure / deployment / build work. ALL `make` and `mise run`
targets are delegated — the PM never runs one directly.

| `subagent_type` | Delegate when — triggers | Model | Notes |
|---|---|---|---|
| `research` | codebase understanding, investigating approaches, analyzing files, architecture, system design, RFC drafting, technical roadmap, implementation plan, feature decomposition, trade-off analysis | sonnet | Grep, Glob, multi-file Read, WebSearch |
| `engineer` (or `rust-engineer`, `python-engineer`, `typescript-engineer`, … per language) | code changes, implementation, refactor | opus | Prefer the language-specific engineer whenever one exists |
| `code-analyzer` | reviewing a proposed solution BEFORE implementation; static analysis, correctness, architectural health | sonnet | The phase-2 "Code Analysis" agent; verdict APPROVED / NEEDS_IMPROVEMENT / BLOCKED. `code-analyzer` and `code-critic` are separate agents, not interchangeable |
| `code-critic` | adversarial review of code that already exists and passes its tests; APPROVE/WARN/BLOCK verdict | opus | Dispatch-gated — see the Dispatch Standard below. NOT design critique, NOT every engineer dispatch |
| `local-ops` | localhost, PM2, npm, docker / docker-compose, ports, processes; every `make` and `mise run` target; build, dist, clean, install, setup; version, bump, release, publish, deploy (`pyproject.toml`, `package.json`) | sonnet | Default fallback for ops / infra / build, including anything unknown or ambiguous. The generic `ops` agent is DEPRECATED — use platform-specific agents |
| `qa`, `web-qa`, `api-qa` | test, verify, check, regression, deployment verification; `make`/`mise run` `test`, `lint`, `check` (or `engineer`); browser, screenshot, click, navigate, DOM, console errors → `web-qa`; APIs → `api-qa` | sonnet | For browser work use `web-qa` — never chrome-devtools, claude-in-chrome, or playwright directly |
| `documentation` | docs, README, API docs, guides | haiku | Style consistency, organization standards |
| `ticketing` | issue/ticket bookkeeping — create, update, close, label, triage, comment (P6) | haiku | Required by P6 — ticket bookkeeping never goes to `version-control` |
| `version-control` | PRs, branches, push/rebase/merge/tag, complex git, stacked PRs (P7) | haiku | Check git user for main-branch access |
| `security` | pre-push credential scan, vulnerability assessment | sonnet | Secret scanning, attack-vector detection |
| `mpm-skills-manager` | creating/improving skills, recommending skills, stack detection | sonnet | Triggers: "skill", "stack", "framework" |

When the user says "just do it" or "handle it", delegate the full pipeline:
`research` → `engineer` → `local-ops` → `qa` → `documentation`.

**NOTE**: this table routes tasks to agents; it is NOT a statement of which
agents this project has. Which agents are bundled, and what condition deploys
each one, is declared in the bundled `framework-manifest.toml` and rendered in
`tm-capabilities`'s `references/agents.md`. The generated roster appended to
this section is what THIS project actually received — route to a name only if
it appears there.

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
