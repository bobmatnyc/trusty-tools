# Getting Started with open-mpm

## Prerequisites

- **Rust stable 1.80+** — the crate uses edition 2024. Install via
  [rustup](https://rustup.rs/).
- **Git** — required if you use `auto_push` or `worktree_protection` workflows.
- **One LLM credential** (choose one, or combine):

| Credential | When required |
|---|---|
| `OPENROUTER_API_KEY` | Default routing for all agents. Get one at openrouter.ai. |
| `ANTHROPIC_API_KEY` | Direct Anthropic API (`use_anthropic_direct = true` agents). |
| `CLAUDE_CODE_OAUTH_TOKEN` | `runner = "claude-code"` agents only. Generate via `claude setup-token`. |

- **BRAVE_API_KEY** (optional) — enables the `web_search` tool for
  research-agent and qa-agent. Without it, web search returns a graceful error
  message that the LLM can recover from.
- **gh CLI** (optional) — required only if `ticket_management.enabled = true` in
  a workflow JSON.

---

## Setup

```bash
# Clone the repository
git clone <repo-url> open-mpm
cd open-mpm

# Create the API key file (never committed)
cat > .env.local <<'EOF'
OPENROUTER_API_KEY=sk-or-v1-...
# Uncomment as needed:
# ANTHROPIC_API_KEY=sk-ant-api03-...
# CLAUDE_CODE_OAUTH_TOKEN=sk-ant-oat01-...
# BRAVE_API_KEY=BSA...
EOF

# Compile (first build downloads crate dependencies)
cargo build

# Verify the build
cargo run -- --version
# Prints: open-mpm v0.1.0 (abc1234) build #1
```

---

## Quick Verification

Run the test suite to confirm nothing is broken:

```bash
cargo test
```

Most tests are unit tests that run without an API key. The only integration
tests that require a live API key are skipped automatically when
`OPENROUTER_API_KEY` is unset.

---

## Run Modes

### CTRL REPL (default / --ctrl)

The interactive mode. Start it with no flags or with `--ctrl`:

```bash
cargo run
# or explicitly:
cargo run -- --ctrl
```

CTRL presents a prompt `>` and accepts natural-language requests. It routes
each request to a PM actor tied to the current project directory, which in turn
delegates to the appropriate sub-agent. Type `exit` or `quit` to leave.

Example session:

```
> Write a Python script that formats CSV data as a Markdown table.

[PM]: Delegating to python-engineer…
[python-engineer]: Here is the script:

## File: csv_to_md.py
```python
import csv, sys
...
```

Output written to: .open-mpm/out/<run_id>/
```

### Direct Mode (--direct)

Bypasses the PM LLM entirely and sends a task straight to a named sub-agent.
Useful for scripting or iterating on a specific agent without burning PM tokens.

```bash
# From a task file
cargo run -- --direct python-engineer --task-file ./task.md

# Inline task string
cargo run -- --direct research-agent --task "What are the best Rust async HTTP clients?"

# Save generated files to a directory
cargo run -- --direct code-agent --task-file ./task.md --out-dir ./out/run1
```

If the agent output contains `## File: <path>` sections, they are extracted and
written under `--out-dir` automatically.

### Workflow Mode (--workflow)

Runs a declarative multi-phase pipeline defined in a JSON file.

```bash
cargo run -- --workflow prescriptive \
  --task-file ./my-task.md \
  --out-dir ./out/$(date +%Y%m%d-%H%M%S)
```

This runs the built-in `config/workflows/prescriptive.json` pipeline:

```
research -> plan -> code -> qa -> observe -> (docs, skipped by default)
```

Each phase output is available as a template variable in subsequent phases
(`{{research}}`, `{{plan}}`, etc.). After the `code` phase, generated files are
extracted to `--out-dir`. The `qa` phase runs pytest against those files.

### PM Mode (--pm)

Single-shot PM orchestrator. Reads one line from stdin, delegates via the LLM,
prints the result:

```bash
echo "Write a bubble sort in Python" | cargo run -- --pm
```

---

## First Workflow Walkthrough

This walkthrough runs the full `prescriptive` pipeline on a small task.

**1. Create a task file:**

```bash
cat > /tmp/fizzbuzz-task.md <<'EOF'
Implement FizzBuzz in Python.

Requirements:
- Function `fizzbuzz(n: int) -> list[str]` that returns FizzBuzz values 1..n
- "Fizz" for multiples of 3, "Buzz" for multiples of 5, "FizzBuzz" for both
- A `__main__` block that prints fizzbuzz(20)
- Full pytest test coverage
EOF
```

**2. Create an output directory:**

```bash
mkdir -p out/fizzbuzz
```

**3. Run the workflow:**

```bash
cargo run -- --workflow prescriptive \
  --task-file /tmp/fizzbuzz-task.md \
  --out-dir ./out/fizzbuzz
```

Watch the structured log output on stderr (each phase emits an `info` line when
it starts and completes). The final observe-agent report is printed to stdout.

**4. Inspect the output:**

```bash
ls out/fizzbuzz/
# fizzbuzz.py   test_fizzbuzz.py   (plus any stubs/ from plan phase)

python3 -m pytest out/fizzbuzz/ -v
```

**5. Check performance telemetry:**

```bash
ls docs/performance/runs/
# 20260423-011958-build52.json  (one file per workflow run)

cat docs/performance/runs/*.json | python3 -m json.tool | head -40
```

---

## CTRL Mode in Depth

CTRL (the default mode) is the preferred day-to-day interface. It manages
multiple PM actors so you can switch between projects without restarting.

### Starting CTRL

```bash
cargo run -- --ctrl
```

### Basic interaction

Type any natural-language request at the `>` prompt. CTRL routes it to the PM
actor for the current working directory.

### Multi-project usage

Each project directory gets its own PM actor. When CTRL starts, it registers
the current directory in `~/.open-mpm/projects.json`. Switch projects by
running CTRL from different directories, or by using the CTRL project selector
(if configured in your `ctrl.toml`).

### Session management

```bash
# Clear agent conversation history before a run
cargo run -- --clear-sessions

# Force project re-initialization (re-seeds the memory graph)
cargo run -- --reinit
```

---

## Code Index

open-mpm maintains a local code index of your project for agent-assisted search.
The index uses tree-sitter parsing and local sentence-transformer embeddings —
no API call is made during indexing.

```bash
# Build (or refresh) the index
cargo run -- --reindex
# Indexed 247 chunks.

# Keep the index live as you edit
cargo run -- --watch

# Search the index from the CLI
cargo run -- code search "async error handling"
# Returns: ranked list of matching code chunks with file/line refs

# Search the memory (turn history) index
cargo run -- memory search "fizzbuzz"
```

---

## Environment Variables Reference

| Variable | Required | Description |
|---|---|---|
| `OPENROUTER_API_KEY` | For most agents | OpenRouter API key |
| `ANTHROPIC_API_KEY` | When `use_anthropic_direct = true` | Direct Anthropic API key |
| `CLAUDE_CODE_OAUTH_TOKEN` | When `runner = "claude-code"` | OAuth token from `claude setup-token` |
| `BRAVE_API_KEY` | Optional | Brave Search API key for web_search tool |
| `RUST_LOG` | Optional | Log level: trace, debug, info, warn, error (default: info) |
| `OPEN_MPM_CONFIG_DIR` | Optional | Override for config/agents/ directory path |
| `OPEN_MPM_OUT_DIR` | Optional | Default output root when --out-dir is omitted |
| `OPEN_MPM_RUN_ID` | Auto-set | Shared run ID inherited by sub-agents |
| `OPEN_MPM_MAX_TURNS` | Optional | Per-invocation max-turns override for sub-agents |
| `OPEN_MPM_MODEL_<AGENT>` | Optional | Per-agent model override, e.g. `OPEN_MPM_MODEL_CODE_AGENT` |
| `OPEN_MPM_DEFAULT_MODEL` | Optional | Fallback model when agent TOML has no model set |

---

## Using Direct Anthropic API

Set `use_anthropic_direct = true` in an agent TOML to route calls directly to
`api.anthropic.com` instead of through OpenRouter. This gives lower latency and
access to the latest-features first.

```toml
[llm]
temperature = 0.2
max_tokens = 8192
use_anthropic_direct = true
```

Requires `ANTHROPIC_API_KEY` in `.env.local`. Do not use with
`CLAUDE_CODE_OAUTH_TOKEN` — the REST API rejects OAuth tokens.

---

## Using the claude CLI Runner

Set `runner = "claude-code"` in the `[agent]` section to have the binary spawn
the locally-installed `claude` CLI instead of calling the REST API:

```toml
[agent]
name = "claude-code-engineer"
runner = "claude-code"
...
```

This uses `CLAUDE_CODE_OAUTH_TOKEN`. Before running a workflow that uses
claude-code agents, the binary validates CLI authentication and fails fast if it
fails. Claude Max subscribers can use this mode without a separate API key.

---

## Make Targets

A `Makefile` provides convenience wrappers around common `cargo` commands:

```bash
make build      # cargo build
make test       # cargo test
make clippy     # cargo clippy --all-targets -- -D warnings
make fmt        # cargo fmt
make lint       # clippy + fmt
make ctrl       # run CTRL REPL
make release    # cargo build --release
make clean      # cargo clean
make version    # print semver from Cargo.toml
```

Run a specific prescriptive workflow:

```bash
make run-task TASK_FILE=./my-task.md
```
