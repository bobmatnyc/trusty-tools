# Integration Tests

These tests verify the full open-mpm installation experience, separate from unit tests.

## Quick Start

```bash
# Build and set up a test installation
./tests/integration/install.sh

# Then follow the printed instructions to run the bake-off
```

## What This Tests

1. **Binary build** — `cargo build --release` produces a working binary
2. **Agent discovery** — `.claude/agents/python-engineer.md` is discovered and loaded
3. **Skill injection** — bundled skills are found and available
4. **Harness protocol** — agents receive harness instructions automatically
5. **Bake-off execution** — the full workflow runs and produces output

## Layout

```
tests/integration/
├── README.md           — this file
├── install.sh          — build + stage a clean test dir under /tmp
├── run_bakeoff.sh      — run a bake-off level and verify output
└── fixtures/
    ├── CLAUDE.md                        — project description for the test project
    └── agents/python-engineer.md        — .md-format agent for .claude/agents/
```

## Individual Bake-off Tasks

The task files live in `.open-mpm/tasks/` in the project root (`level-1.txt` …
`level-5.txt`). The integration test uses **Level 2** (markdown table
formatter) by default as a lightweight smoke test of the full pipeline.

To stage a different level, pass `LEVEL=N` to `install.sh`:

```bash
LEVEL=3 ./tests/integration/install.sh
```

## Note on API Keys

The integration test requires `OPENROUTER_API_KEY` or `ANTHROPIC_API_KEY` to
be set. These are **real LLM calls** — the test is not mocked.

`install.sh` automatically forwards `.env.local` from the project root into
the staged test dir if present.

## Relationship to `cargo test`

`cargo test` runs pure unit tests (mocked LLM responses) and is safe to run
anywhere. The integration scripts here require API keys and live network
access, so they are intentionally **not** invoked by `cargo test`.

If you add LLM-dependent tests under `src/` or `tests/`, mark them with
`#[ignore]` and document that they belong in this integration suite.
