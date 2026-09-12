Added

- Every dispatchable roster agent now declares a `tools:` allowlist in Claude
  Code's vocabulary, so a dispatched subagent carries only the tools and MCP
  servers its role uses instead of the implicit all-tools default. Measured on
  2026-09-12: a subagent turn carried 151 deferred MCP tool names plus their
  schemas, and an allowlist that omits `Skill` also sheds the whole ~131-entry
  skills listing — +1.7K tokens for a two-tool agent against +51.8K for
  general-purpose. `mcp__claude-in-chrome` reaches `web-qa` only,
  `mcp__trusty-mpm` only `local-ops` and the two `mpm-*` managers,
  `mcp__trusty-memory` only `memory-manager`/`research`/`code-analyzer`,
  `mcp__trusty-review` only `code-critic` and `version-control`, and
  `mcp__trusty-search` the engineers, QA, analysis, research, security and
  documentation families. `Skill` survives only where the family loads a skill
  it does not already preload: `rust-engineer`, `version-control`, `local-ops`
  and `mpm-skills-manager`. An otherwise read-only role still carries the write
  tool its own body mandates: `research` keeps `Write, Edit` for the capture
  step that saves to `docs/research/`, and `code-analyzer` keeps `Write` for the
  script its large-volume path generates under `scripts/code-review/`. For the
  same reason `BASE-AGENT.md` — inherited by every agent, most of which no
  longer carry `Skill` — now points at the `tm-prose-style` skill in prose
  instead of modelling a `Skill(...)` call the agent cannot make. Per
  `docs/specs/agent-context-minimization.md` §C, pinned by
  `every_roster_agent_deploys_with_its_declared_tools` (#7683).
- `tcode_tools:` — a second agent-frontmatter allowlist, parsed, override-merged
  and emitted exactly like `tools:`, carrying `trusty-code`'s own tool
  vocabulary. One roster serves two runtimes whose tool names do not overlap,
  and `ToolRegistry::gated` matches by exact name, so a Claude-vocabulary list
  read as a trusty-code allowlist gates that agent down to zero callable tools
  (#7683). `AgentMetadata` carries `#[non_exhaustive]` from this release on, so
  the next field it gains costs no consumer a compile error and no crate a
  breaking bump — build one from `AgentMetadata::default()` and assign the
  fields you need (#7683).
