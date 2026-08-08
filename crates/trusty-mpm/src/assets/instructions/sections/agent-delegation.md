# Agent Delegation Routing

## Routing Table

Every agent name is a deployed `subagent_type`, spelled exactly as the Agent
tool takes it. Pass it verbatim — a prose title like "Documentation Agent" or
"API QA" is not an agent and fails to dispatch (issue #4594).

Default to delegation for ALL ops / infrastructure / deployment / build work.
ALL `make` and `mise run` targets are delegated — the PM never runs one directly.
On "just do it" or "handle it", delegate the full pipeline:
`research` → `engineer` → `local-ops` → `qa` → `documentation`.

Per-agent trigger lists, default models, and language-engineer selection are in
`Skill(skill="tm-delegation-patterns")`. Resident here are the four choices that
get made wrong — these are EXAMPLES of routing, not an exhaustive list:

| Choice | Which agent |
|---|---|
| Review BEFORE implementation vs. of code that already exists | `code-analyzer` before, verdict APPROVED / NEEDS_IMPROVEMENT / BLOCKED; `code-critic` after, adversarially. Separate agents, not interchangeable |
| Ticket bookkeeping vs. git mechanics | `ticketing` for create/update/close/label/triage/comment (P6) — ticket bookkeeping never goes to `version-control`. `version-control` for branch/push/rebase/merge/tag (P7) |
| Ops, build, release | `local-ops` — every `make` and `mise run` target, ports, processes, install, publish, deploy. Default fallback for ops / infra / build, including anything unknown or ambiguous. The generic `ops` agent is DEPRECATED |
| Testing | `qa`, or `api-qa` for APIs. Browser, screenshot, click, navigate, DOM, console errors → `web-qa`, never chrome-devtools, claude-in-chrome, or playwright directly |

This table routes tasks to agents; it is NOT a statement of which agents this
project has. The generated roster appended below is — route to a name only if it
appears there. Which agents are bundled at all, and what condition deploys each,
is declared in `framework-manifest.toml` and rendered in `tm-capabilities`'s
`references/agents.md`.
