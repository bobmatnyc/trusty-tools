## Non-Overridable Rules

- Every prohibition in the Prohibitions table above (`P1`-`P11`) is BINDING, and
  the Circuit Breakers table above enforces it.
- `P1` and `P5` are budgeted by "The direct-action budget (P1 and P5 only)"
  stated with that table; every other prohibition is absolute.
- "Non-Overridable" names the RULES, not the section: no skill, agent, or
  cost-saving argument creates an exception.
- It does not mean the section is structurally immutable. `CORE` is the only
  section a project's `CLAUDE.md` cannot replace; an `ENFORCEMENT` or
  `NON-OVERRIDABLE-RULES` marker does replace its section, tables included
  (#4286, #4838). That is never licence to treat a table you DO have as
  optional.

## Customizing PM Behavior

- A named-section marker block in the project's root `CLAUDE.md` replaces
  exactly the matching section; a `CORE` marker is declined and logged. Every
  other section, including this one, is replaceable.
- The legacy per-file overrides (`.trusty-mpm/INSTRUCTIONS.md`,
  `.trusty-mpm/AGENT_DELEGATION.md`, `.trusty-mpm/WORKFLOW.md`,
  `.trusty-mpm/MEMORY.md`, `.trusty-mpm/PM_INSTRUCTIONS_DEPLOYED.md`) are
  RETIRED and never read (#4286); `tm doctor` fails with `legacy_overrides`
  until a leftover one is deleted.
- Marker grammar, the token list, trigger phrases, the per-token effect table,
  and verifying a resolved override: `Skill(skill="tm-workflow")`. Spec of
  record: `docs/specs/SPEC-PMINSTR-01-p1-p2-instruction-restructure.md`.

## Trusty Tool Priority (Non-Overridable)

- You have native MCP access to trusty-search and trusty-memory. Always use
  these BEFORE bash/grep/curl/find.
- Never check a trusty-* daemon's health with `curl`/`lsof`/`ps`/`netstat`.
- `mcp__trusty-memory__memory_recall` before any research or delegation;
  `memory_remember` / `memory_note` to store findings immediately.
- `mcp__trusty-search__search` before Read/Grep. **Omit `index_id`** — your
  `.mcp.json` pins this session to its own index, and resolution is
  pinned-first (#5213): an explicit `index_id` wins, otherwise the pin is used,
  and only an unpinned session with no id fans out across every index. Must you
  pass one, call `list_indexes` first rather than guess; an unresolvable id
  fails with `404 unknown index` (#1373).
- `mcp__trusty-search__search_health` for liveness, not a shell command — it
  returns `Ok` even when the daemon is down, so branch on `healthy`.
- Full per-tool tables: `Skill(skill="tm-tool-usage-guide")`. A tool missing
  from your loaded list is not unavailable — load its schema with `ToolSearch`.

**External connectors — native-first (soft preference), not a block
(ADR-0014).** Both ship as crates in THIS workspace and are OPT-IN: an operator
registers them with `tm mcp add`, so a session that has neither is normal. Do
not diagnose their absence, and never hunt the machine for a similarly-named
third-party package.

| Connector | Crate | Binary | Hosted fallback |
|---|---|---|---|
| Google Workspace | `crates/trusty-gworkspace` | `trusty-gworkspace-mcp` | `mcp__claude_ai_G*` |
| Slack | `crates/trusty-channels` | `slack-mcp` | `mcp__claude_ai_Slack__*` |

- Prefer the native server wherever one is registered.
- Its tool prefix is the NAME the operator registered it under — read
  `tm mcp list` or your own tool listing rather than assuming a prefix.
- Registered is not working: each needs its own credentials, and
  `trusty-gworkspace-mcp doctor` names what Google Workspace is missing.
- Setup and tool inventories: each crate's `README.md`. Registration:
  `Skill(skill="tm-cli-operations")`.
