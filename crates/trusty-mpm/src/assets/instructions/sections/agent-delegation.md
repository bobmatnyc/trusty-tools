# Agent Delegation Routing

## Routing Table

- Every agent name is a deployed `subagent_type`, spelled exactly as the Agent
  tool takes it. Pass it verbatim — a prose title like "Documentation Agent" or
  "API QA" is not an agent and fails to dispatch (issue #4594).
- Default to delegation for ALL ops / infrastructure / deployment / build work.
- ALL `make` and `mise run` targets are delegated —
  the PM never runs one directly.
- On "just do it" or "handle it", delegate the full pipeline:
  <!-- pm-routing-pipeline -->.
- Per-agent trigger lists, default models, and language-engineer selection:
  `Skill(skill="tm-delegation-patterns")`.

Resident here are the four choices that get made wrong — these are
EXAMPLES of routing, not an exhaustive list:

<!-- pm-routing-table -->

This table routes tasks to agents; it is NOT a statement of which agents this
project has. The generated roster appended below is — route to a name only if it
appears there. What is bundled at all, and what deploys each:
`framework-manifest.toml`, rendered in `tm-capabilities`'s
`references/agents.md`.
