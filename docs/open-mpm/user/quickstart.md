# Quickstart

Get open-mpm running in five minutes.

## 1. Install

### From source (recommended for now)

```bash
git clone https://github.com/bobmatnyc/open-mpm
cd open-mpm
cargo build --release
# Binary: ./target/release/open-mpm
```

### Via cargo (when published)

```bash
cargo install open-mpm
```

## 2. Configure credentials

Create `.env.local` in your working directory (never committed):

```bash
cat > .env.local <<'EOF'
OPENROUTER_API_KEY=sk-or-v1-...
# Optional: direct Anthropic API
# ANTHROPIC_API_KEY=sk-ant-api03-...
# Optional: enables web_search tool
# BRAVE_API_KEY=BSA...
EOF
```

You only need **one** LLM credential to get started. `OPENROUTER_API_KEY`
covers every bundled agent. See [configuration.md](./configuration.md) for
the full credential matrix.

## 3. Verify

```bash
open-mpm --version
# open-mpm v0.1.37 (abc1234) build #1
```

## 4. Run modes

### Interactive CTRL REPL (default)

```bash
open-mpm
# or explicitly:
open-mpm --ctrl
```

You get a prompt:

```
CTRL> Write a Python script that formats CSV as a markdown table
```

CTRL routes the request to a per-project PM, which delegates to the right
sub-agent (e.g. `python-engineer`). Type `/help` for slash commands.

### API server + Web UI

```bash
open-mpm --api --port 7654
# [open-mpm] API:    http://localhost:7654/api
# [open-mpm] Web UI: http://localhost:7654/
# [open-mpm] Docs index: 24 documents indexed from ./docs
```

The web UI is embedded in the binary — no separate frontend deploy.
Add `--api-token <TOK>` to require bearer-token auth on `/api/*` routes.

### Workflow mode

Run a declarative pipeline (`research → plan → code → qa → observe`):

```bash
open-mpm --workflow prescriptive \
  --task "Implement FizzBuzz with pytest tests" \
  --out-dir ./out/fizzbuzz
```

Generated files land under `--out-dir` and pytest runs automatically in the
`qa` phase.

### Direct mode (bypass PM)

```bash
open-mpm --direct python-engineer \
  --task "Bubble sort with type hints" \
  --out-dir ./out/sort
```

Useful for scripting or iterating on a single agent.

## 5. Search project docs from CTRL

CTRL ships with a `search_docs` tool backed by an in-memory TF-IDF index.
Just ask in natural language:

```
CTRL> how do I write a custom skill?
```

CTRL will call `search_docs("write custom skill")` and answer using the
matching files under `docs/`. The same index powers
`GET /api/docs/search?q=...` on the API server.

## Next steps

- [CLI reference](./cli-reference.md) — every flag, every mode
- [Configuration](./configuration.md) — `.open-mpm/`, agents, skills, workflows
- [Agents and skills](./agents-and-skills.md) — what's bundled, how to extend
