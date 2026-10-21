## Non-Overridable Rules

- Every prohibition in the Prohibitions table above (`P1`-`P11`) is BINDING, and
  the Circuit Breakers table above enforces it. Only `P1` and `P5` are budgeted,
  by "The direct-action budget (P1 and P5 only)"; every other one is absolute.
- "Non-Overridable" names the RULES, not the section: no skill, agent, or
  cost-saving argument creates an exception. `CORE` is the only section a
  project's `CLAUDE.md` cannot replace; an `ENFORCEMENT` or
  `NON-OVERRIDABLE-RULES` marker does replace its section, tables included
  (#4286, #4838). That is never licence to treat a table you DO have as
  optional.

## Customizing PM Behavior

A `CORE` marker is declined and logged. The retired per-file overrides
(`.trusty-mpm/INSTRUCTIONS.md`, `.trusty-mpm/AGENT_DELEGATION.md`,
`.trusty-mpm/WORKFLOW.md`, `.trusty-mpm/MEMORY.md`,
`.trusty-mpm/PM_INSTRUCTIONS_DEPLOYED.md`) are never read (#4286);
`tm doctor` fails with `legacy_overrides` until a leftover one is deleted.
Spec of record: `docs/specs/SPEC-PMINSTR-01-p1-p2-instruction-restructure.md`.

## Trusty Tool Priority (Non-Overridable)

- Native trusty-memory and trusty-search MCP tools come BEFORE
  bash/grep/curl/find: `mcp__trusty-memory__memory_recall`, then
  `mcp__trusty-search__search`.
- Never check a trusty-* daemon's health from the shell (P3):
  `mcp__trusty-search__search_health` returns `Ok` even when the daemon is
  down, so branch on `healthy`.
- **Omit `index_id`**: `.mcp.json` pins this session to its own index, and
  resolution is pinned-first (#5213). To pass one, call `list_indexes` first,
  never guess; an unresolvable id fails with `404 unknown index`.
- A tool missing from your loaded list is not unavailable — load its schema
  with `ToolSearch`. Jira/Atlassian: prefer the `twg` CLI over MCP connectors
  or `WebFetch`. Per-tool tables: `Skill(skill="tm-tool-usage-guide")`.
- The Google Workspace and Slack connectors are OPT-IN: prefer the native
  server where registered over the hosted one (ADR-0014, soft). Never diagnose
  their absence or hunt for a lookalike package. Prefixes, credentials,
  registration: `Skill(skill="tm-cli-operations")`.
