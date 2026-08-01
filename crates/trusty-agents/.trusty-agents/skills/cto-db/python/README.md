# cto-db-skill

Python skill package backing cto-assistant's four `[tools].allow` entries:
`query_headcount`, `query_budget`, `query_risks`, `query_work_classification`.

## IMPORTANT: fixture data, not the real Duetto database

**The database this skill queries by default (`fixtures/cto_fixture.db`) is
invented sample data, checked in for structural completeness and
testability only.** It is not a copy, sample, or export of any real Duetto
system. Names, teams, and numbers are all placeholders (e.g. "Ada Fixture",
"GTM Sample Team").

The real source this skill is meant to eventually query is
`~/Duetto/cto/data/cto.db` — a SQLite file that lives on Bob's machine. This
sandbox cannot reach or verify that file, so this skill was built and tested
entirely against the fixture. **Do not treat any number this skill returns
as real Duetto headcount/budget/risk data until `CTO_DB_PATH` is pointed at
the genuine file and someone has verified the schema actually matches.**

To use the real database once it's reachable:

```bash
export CTO_DB_PATH=~/Duetto/cto/data/cto.db
```

With `CTO_DB_PATH` unset, `db.resolve_db_path()` falls back to the bundled
fixture. See `db.py`'s module docstring for the exact resolution order.

## Why this schema

The four tables/view this skill queries (`person`, `rd_budget_2026`,
`user_work_distribution`, `v_needs_review`) match the schema already
documented — and unit-tested against an in-memory fixture with the same
shape — in `crates/trusty-cto-db/src/lib.rs`. That Rust crate (plus
`crates/tc-services::cto_db` and the since-deleted `crates/cto-assistant`)
implemented the same four tools natively in Rust, fully tested, but was
never wired into any built binary: the only `install_plugins(...)` call site
that registered it was removed by PR #3310 ("sever cto-assistant edge"), and
nothing called it after that (issues #3656, #3700, #3732). This Python skill
is a parallel,
from-scratch reimplementation — chosen over resurrecting that Rust wiring
per the owner's explicit architectural directive that CTO DB business logic
belongs in a skill, not hardcoded as a hand-written Rust `ToolExecutor`
inside/adjacent to the agent (see #3656's DOC-41 §2.0 "declarative-only"
objection). `crates/cto-assistant` — the agent-specific adapter layer — was
dissolved in #3732 once this skill reached parity. The host-agnostic
`crates/trusty-cto-db` and `crates/tc-services::cto_db` are left untouched;
assessing them is separate scope.

## Package layout

- `src/cto_db_skill/db.py` — the four query functions + DB path resolution.
- `src/cto_db_skill/cli.py` — subprocess entrypoint invoked by
  `crates/trusty-agents/src/tools/python_skill.rs`: reads a tool name from
  argv, JSON args from stdin, prints a JSON result (or `{"error": ...}`) to
  stdout, exits 0/1.
- `fixtures/build_fixture.py` — regenerates the committed
  `fixtures/cto_fixture.db`. Run it after editing the sample rows.
- `tests/test_db.py` — unit tests against an in-memory seeded connection.
- `tests/test_cli.py` — end-to-end tests that actually spawn the CLI as a
  subprocess (the real invocation contract), against the fixture DB.

## Running the tests

```bash
cd crates/trusty-agents/.trusty-agents/skills/cto-db/python
uv venv
uv pip install -e ".[dev]"
uv run pytest
```

## Manual smoke test

```bash
echo '{"filter_by": "team"}' | uv run python -m cto_db_skill.cli query_headcount
```
