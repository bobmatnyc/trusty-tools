# Agent Delegation Routing

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
| `code-critic` | adversarial review of code that already exists and passes its tests; APPROVE/WARN/BLOCK verdict | opus | Dispatch-gated by test-ladder rung — see `Skill(skill="tm-delegation-patterns")`. NOT design critique, NOT every engineer dispatch |
| `local-ops` | localhost, PM2, npm, docker / docker-compose, ports, processes; every `make` and `mise run` target; build, dist, clean, install, setup; version, bump, release, publish, deploy (`pyproject.toml`, `package.json`) | sonnet | Default fallback for ops / infra / build, including anything unknown or ambiguous. The generic `ops` agent is DEPRECATED |
| `qa`, `web-qa`, `api-qa` | test, verify, check, regression, deployment verification; `make`/`mise run` `test`, `lint`, `check` (or `engineer`); browser, screenshot, click, navigate, DOM, console errors → `web-qa`; APIs → `api-qa` | sonnet | For browser work use `web-qa` — never chrome-devtools, claude-in-chrome, or playwright directly |
| `documentation` | docs, README, API docs, guides | haiku | Style consistency, organization standards |
| `ticketing` | issue/ticket bookkeeping — create, update, close, label, triage, comment (P6) | haiku | Required by P6 — ticket bookkeeping never goes to `version-control` |
| `version-control` | PRs, branches, push/rebase/merge/tag, complex git, stacked PRs (P7) | haiku | Check git user for main-branch access |
| `security` | pre-push credential scan, vulnerability assessment | sonnet | Secret scanning, attack-vector detection |
| `mpm-skills-manager` | creating/improving skills, recommending skills, stack detection | sonnet | Triggers: "skill", "stack", "framework" |

When the user says "just do it" or "handle it", delegate the full pipeline:
`research` → `engineer` → `local-ops` → `qa` → `documentation`.

This table routes tasks to agents; it is NOT a statement of which agents this
project has. The generated roster appended below is — route to a name only if it
appears there. Which agents are bundled at all, and what condition deploys each,
is declared in `framework-manifest.toml` and rendered in `tm-capabilities`'s
`references/agents.md`.
