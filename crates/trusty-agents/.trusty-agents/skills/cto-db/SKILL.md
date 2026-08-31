---
name: cto-db
description: Read-only query tools over the CTO operations database (headcount, budget, risks, work classification) for the cto-assistant persona.
tags: [cto-assistant, database, sqlite, python]
version: "0.1.0"
persona: cto-assistant
---

# CTO Operations Database

Four read-only query tools backed by a bundled Python package
(`python/`, package `cto_db_skill`) rather than hand-written Rust — see
`python/README.md` for the full "why a skill, not a coded agent" rationale
(issues #3656 / #3700 / #3732).

## Tools

- `query_headcount(filter_by?: "team" | "status" | "vendor")` — active
  headcount, optionally grouped.
- `query_budget(team?: string, category?: string)` — 2026 R&D budget
  breakdown, optionally filtered.
- `query_risks(severity?: "high" | "medium" | "low")` — risk-register proxy
  from low-confidence classifications, optionally filtered.
- `query_work_classification(pod?: string)` — most-recent-month work-type
  breakdown, optionally filtered by pod (team).

Exact input schemas live in `manifest.json`, which
`crates/trusty-agents/src/tools/python_skill.rs` reads at startup to build
one `ToolExecutor` per tool and register them (via `AgentPlugin` +
`install_plugins`) against the `cto-assistant` persona declared above.

## Data source — read this before trusting a number

This skill refuses to answer unless a database is configured (#4860). Set
`CTO_DB_PATH` to the real database (`~/Duetto/cto/data/cto.db`, a
Duetto-internal file on Bob's machine, not reachable from this environment).
Setting `CTO_DB_USE_FIXTURE=1` instead serves a bundled **fixture**
(`python/fixtures/cto_fixture.db`) of invented sample data — useful for tests
and local development, never a source of real figures. With neither set, all
four tools return a JSON error rather than a fabricated answer.

Every successful response carries `db_path` and `is_fixture`, so fixture
output stays identifiable. See `python/README.md` for details.

## Invocation contract

`crates/trusty-agents/src/tools/python_skill.rs`'s `PythonSkillToolExecutor`
spawns `manifest.python.command` (from `python/` as the working directory)
with the tool name appended as an extra argument, writes the tool call's
JSON arguments to the subprocess's stdin, and expects exactly one JSON
object on stdout (or a JSON `{"error": "..."}` object + non-zero exit,
which becomes a recoverable `ToolResult::err(...)`, never a panic).

This bridge is generic — it does not know anything about CTO DB
specifically, only how to run a skill manifest. Any future Python skill
that follows the same `manifest.json` shape gets the same wiring for free.
