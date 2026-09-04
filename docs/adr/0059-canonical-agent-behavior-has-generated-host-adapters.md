# 0059. Canonical agent behavior has generated host adapters

- **Status:** Accepted
- **Date:** 2026-09-03
- **Scope:** Workspace-wide — authored instructions, agent definitions, and
  skills consumed by `trusty-code`, `trusty-agents`, and `trusty-mpm`, plus the
  host adapter generation that deploys them
- **Reversibility Cost:** High — canonical identity, provenance, and adapter
  generation determine how every harness receives agent behavior
- **Decision Drivers:** the owner ruling that agent and skill sources are
  identical across products; prevention of silent drift between hand-maintained
  copies; compatibility with the Agent Skills `SKILL.md` package and Codex agent
  conventions; independent per-product deployment roots
- **Supersedes / Superseded by:** Supersedes
  [ADR-0015](0015-three-product-agent-composition-model.md); superseded by
  nothing

## Context

ADR-0015 proposed a common Markdown-plus-YAML shape with shared `extends`
semantics, but still described separate product catalogs and product-specific
composition purposes. The workspace now carries copied agents, skills, and
instructions in several host directories. Equivalent behavior can drift while
every copy stays syntactically valid, and no consumer can prove which authored
source produced a deployed artifact.

Host formats are not identical. Codex Agent Skills, Claude-compatible agents,
and Trusty Code's native catalog need different directory layouts or metadata.
Treating those adapters as separate authored sources preserves the drift
problem. Treating one host's deployment directory as canonical couples the other
products to that host.

## Decision

1. **One canonical authored source represents each instruction, agent, and
   skill.** Products do not maintain behaviorally equivalent hand-edited copies.
2. **Canonical identity and content are host-neutral.** The source records a
   stable identifier, version and provenance, behavioral instructions,
   capabilities, and composition references. Host-only metadata is isolated in
   declared adapter data and cannot silently change the behavioral source.
3. **Host layouts are deterministic generated adapters.** Trusty Code, Codex,
   Claude-compatible consumers, Trusty Agents, and MPM may require different
   paths or manifests; those artifacts are generated or validated from the same
   canonical source.
4. **Trusty Code deploys its resolved native catalog below `.trusty-code/`.**
   `.codex/` and `.claude/` outputs are compatibility exports. Their existence
   does not change source ownership.
5. **The common baseline is the interoperable standards set already in use:**
   Markdown instructions, YAML frontmatter where structured metadata is
   required, the Agent Skills `SKILL.md` package shape for skills, and declared
   MCP references rather than host-private embedded configuration.
6. **Generation is reproducible and provenance-bearing.** Given the same
   canonical inputs and adapter version, output bytes and semantic behavior are
   stable. Generated artifacts identify their source and are checked for drift.
7. **Staleness is repaired at the canonical source.** If a deployed artifact
   disagrees with an Accepted spec, the source is corrected and every adapter is
   regenerated. A one-host patch is not a valid fix.
8. **Unsupported host semantics fail explicitly.** An adapter may not drop a
   capability, instruction, tool restriction, or delegation rule without a
   declared, testable compatibility disposition.

## Consequences

- Behavioral review happens once; host packaging becomes mechanical and
  testable.
- Existing hand-maintained `.agents/`, `.claude/`, and `.codex/` copies need a
  provenance audit and either adoption as canonical content or replacement by
  generated output.
- A shared parser and resolver stays useful, but shared parsing alone no longer
  satisfies the architecture.
- MPM can adopt the canonical catalog on its own schedule. Code and Agents can
  implement adapters without writing MPM-owned runtime code.
- CI needs drift checks that compare generated output against committed or
  deployed adapters and name the canonical source to fix.
- The work is not started. `trusty-code` still embeds its own assets under
  `src/assets/agents/` and `src/assets/skills/` alongside host copies;
  [#5425](https://github.com/bobmatnyc/trusty-tools/issues/5425) is the
  implementation.

## Related Decisions

Vetted against the ADR corpus on 2026-09-03:

- **ADR-0015 (Unified agent composition, Proposed):** Supersedes — retains its
  common Markdown/YAML shape and deterministic composition goals, and replaces
  separately authored product catalogs with one canonical behavior source plus
  generated adapters.
- **ADR-0024 (In-process leaf sub-agents):** Consistent — agent topology is a
  behavioral rule carried by the canonical source; this record does not change
  the Agents L0/L1 runtime boundary.
- **ADR-0025 (Collapse deployment hierarchies, Proposed):** Extends — preserves
  deployment-time precedence and adds the requirement that every deployed
  artifact points back to canonical content.
- **ADR-0026 (Credential grants stop at delegation):** Consistent — adapters
  must preserve credential and delegation restrictions.
- **ADR-0042 (Static persistent MCP configuration):** Consistent — canonical
  agents reference MCP identities; product adapters resolve persistent
  declarations without workspace injection.
- **ADR-0058 (Independent Trusty Code harness):** Extends — defines how shared
  behavior enters the product-owned `.trusty-code/` boundary.

No Accepted ADR requires independently authored host copies. ADR-0015 was only
Proposed, and this record preserves its compatible format and composition
semantics while resolving the source-identity gap.
