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

- Native commands come BEFORE bash/grep/curl/find: `tm memory recall "<q>"`,
  then `trusty-search search "<q>" --json`. MCP adapters are for non-mpm
  callers only.
- Never check a trusty-* daemon's health with `curl`, `lsof` or `ps` (P3):
  use `trusty-search health`.
- MCP search tools: **omit `index_id`** — `.mcp.json` pins this session's
  index (#5213). To pass one, call `trusty-search list` first; never guess.
- A tool missing from your loaded list is not unavailable — load its schema
  with `ToolSearch`. Jira/Atlassian: prefer the `twg` CLI over MCP connectors
  or `WebFetch`. Per-tool tables: `Skill(skill="tm-tool-usage-guide")`.
- The Google Workspace and Slack connectors are OPT-IN: prefer the native
  server where registered over the hosted one (ADR-0014, soft). Never diagnose
  their absence or hunt for a lookalike package. Prefixes, credentials,
  registration: `Skill(skill="tm-cli-operations")`.
