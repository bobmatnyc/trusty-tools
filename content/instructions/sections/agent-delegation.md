# Agent Delegation Routing

## Routing Table

- Default to delegation for ALL ops / infrastructure / deployment / build work.
  ALL `make` and `mise run` targets are delegated — the PM never runs one directly.
- On "just do it" or "handle it", delegate the full pipeline:
  `research` → `engineer` → `local-ops` → `qa` → `documentation`.
- Per-agent triggers and language-engineer selection:
  `Skill(skill="tm-delegation-patterns")`.

The four choices that get made wrong —
EXAMPLES of routing, not an exhaustive list:

| Choice | Which agent |
|---|---|
| Review BEFORE implementation vs. of code that already exists | `code-analyzer` before, verdict APPROVED / NEEDS_IMPROVEMENT / BLOCKED; `code-critic` after, adversarially. Separate agents, not interchangeable |
| Issue work vs. PR/git work | By artifact (#5202): the Issue is `ticketing`'s, whole (P6); the Pull Request, title and body included, and all git work are `version-control`'s (P7). Never split one PR edit across both |
| Ops, build, release | `local-ops` — every `make` and `mise run` target, ports, processes, install, publish, deploy. Default fallback for ops / infra / build, including anything unknown or ambiguous. The generic `ops` agent is DEPRECATED |
| Testing | `qa`, or `api-qa` for APIs. Browser, screenshot, click, navigate, DOM, console errors → `web-qa`, never chrome-devtools, claude-in-chrome, or playwright directly |

The table does not say which agents exist: route only to a name the listing or
roster below carries.
