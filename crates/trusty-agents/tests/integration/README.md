# Integration Tests

These tests verify the full trusty-agents (`tagent`) installation experience, separate from unit tests.

## Durable chat attachments API regression

`chat_attachments.py` exercises real `tagent`, `trusty-memory`, and `trusty-search`
binaries with synthetic data (#7370). Python 3.10 or later is sufficient; the
image fixture uses the standard library. Run from the workspace root after
building the binaries. Pass their exact paths and SHA256 hashes:

```bash
python3 crates/trusty-agents/tests/integration/chat_attachments.py --run \
  --assistant-root-parent "$HOME/Documents" \
  --tagent target/debug/tagent --tagent-sha256 <tagent-sha256> \
  --memory target/debug/trusty-memory --memory-sha256 <memory-sha256> \
  --search target/debug/trusty-search --search-sha256 <search-sha256>
```

The assistant parent must exist. Each run creates unique synthetic directories,
uses random loopback ports and an ephemeral API token, and stops its owned
processes. It preserves logs and a JSON proof in the printed `PROOF` directory.
It does not modify installed assistants or daemon configuration.

The assertions cover raw image/table preparation, provider image bytes and table
values, memory-owned history and asset bytes after restart, follow-up hydration,
token and origin rejection, foreign assistant/session denial, unsupported image
providers, and rejection before inference when an older memory daemon cannot
persist attachments. A failed history probe on a plain-text follow-up is recorded
separately because that legacy path can proceed without historical context.

The default provider observer is local and uses no credentials. For an explicitly
authorized live vision check, append `--real-vision --credential-store <path>`.
This permits one synthetic image request to OpenRouter's `openai/gpt-4o-mini`.
The shared resolver reads the existing store through a temporary symlink in the
synthetic home. The harness removes the symlink and never logs credential values.
This optional check costs provider usage and requires an existing configured key.

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

The task files live in `.trusty-agents/tasks/` in the project root (`level-1.txt` …
`level-5.txt`). The integration test uses **Level 2** (markdown table
formatter) by default as a lightweight smoke test of the full pipeline.

To stage a different level, pass `LEVEL=N` to `install.sh`:

```bash
LEVEL=3 ./tests/integration/install.sh
```

## Bake-off API keys

The bake-off requires `OPENROUTER_API_KEY` or `ANTHROPIC_API_KEY` to
be set. These are **real LLM calls** — the test is not mocked.

`install.sh` automatically forwards `.env.local` from the project root into
the staged test dir if present.

## Relationship to `cargo test`

`cargo test` runs unit tests and API integration targets. The bake-off scripts
require API keys and live network access. The attachment script launches three
explicitly selected daemon binaries. These scripts are intentionally **not**
invoked by `cargo test`.

If you add LLM-dependent tests under `src/` or `tests/`, mark them with
`#[ignore]` and document that they belong in this integration suite.
