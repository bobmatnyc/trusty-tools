# Research & Decision Documents

This folder contains investigation documents, decision documents, audits, and
architecture analyses that inform the engineering trajectory of trusty-code.

## Purpose

Research documents capture:

- **Investigation artifacts** — deep-dive analysis, validation runs, audits
- **Decision rationale** — why one approach was chosen over another
- **Trade-off analysis** — correctness vs. compatibility, performance vs.
  maintainability
- **Compatibility specs** — normative reference for Claude-Code-compatible
  surfaces (config, agents, skills, MCP, permissions)

These are distinct from [`regression-testing/`](../regression-testing/) (which
tracks numeric integration-test snapshots per release) and
[`sessions/`](../sessions/) (which capture narrative engineering-session
trajectory).

## Naming Convention

- Investigation / audit docs: `<topic>-<YYYY-MM-DD>.md`
  - Example: `config-loader-audit-2026-07-01.md`
- Decision documents: `<topic>-decision-<YYYY-MM-DD>.md`
  - Example: `permission-model-decision-2026-07-15.md`
- Compatibility / normative specs: `<topic>-spec-<YYYY-MM-DD>.md`
  - Example: `claude-compat-spec-2026-06-02.md`

## Documents

### Compatibility Specifications

- [`claude-compat-spec-2026-06-02.md`](claude-compat-spec-2026-06-02.md) —
  Durable Claude Code compatibility specification for tcode. Configuration-surface
  inventory (settings, memory, agents, skills, MCP, permissions, hooks),
  precedence rules, MPM orchestration mapping, intentional divergences, 9
  critical implementation gotchas, and the C1–C9 compatibility sub-issue
  breakdown. Source: study completed 2026-06-02; refs epic #587.

---

**Related**: [Regression Testing Index](../regression-testing/README.md) | [Session Summaries Index](../sessions/README.md)
