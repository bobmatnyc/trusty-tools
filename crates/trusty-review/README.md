# trusty-review

Fast local PR-review service — LLM-backed code review with search and analysis context.

`trusty-review` fetches GitHub PR diffs, retrieves code context from
[trusty-search](../trusty-search/), queries [trusty-analyze](../trusty-analyze/) for
complexity data, then calls an LLM (AWS Bedrock by default) to produce a structured
review verdict with actionable findings.

It ships as:

- a one-shot **CLI** (`run` / `compare` subcommands)
- a long-lived **HTTP webhook server** (`serve` subcommand, port 7880)
- a **JSON-RPC 2.0 / MCP stdio service** (`serve --stdio`) for Claude Code integration

## Install

```bash
cargo install trusty-review --locked
```

This installs the `trusty-review` binary.

## Prerequisites

Two sidecar daemons must be running for full context retrieval:

- **trusty-search** on `:7878` — code-context hybrid search
- **trusty-analyze** on `:7879` — complexity and quality metrics (optional)

```bash
cargo install trusty-search --locked && trusty-search start
cargo install trusty-analyze --locked && trusty-analyze start
```

## Quick start — one-shot review

```bash
# Review a GitHub PR (Bedrock credentials required)
trusty-review run owner repo 123

# Review a local unified diff
trusty-review run --local-diff /path/to/patch.diff

# Override the reviewer model
trusty-review run owner repo 123 --reviewer-model bedrock/us.anthropic.claude-haiku-4-5

# Compare models
trusty-review compare owner repo 123
```

## HTTP server

```bash
# Start the HTTP daemon on port 7880
trusty-review serve

# Custom port / bind address
trusty-review serve --port 8080 --bind 0.0.0.0
```

Endpoints:

| Method | Path | Description |
|--------|------|-------------|
| GET | `/health` | Liveness, dependency + inference status (see MCP `review_health` for schema) |
| GET | `/status` | In-flight count + last error |
| POST | `/review` | Synchronous on-demand review |
| POST | `/pr/github/webhook` | GitHub PR webhook (HMAC-validated) |

## MCP stdio service (Claude Code integration)

```bash
# Start the MCP stdio server
trusty-review serve --stdio
```

Wire into Claude Code via `.mcp.json`:

```json
{
  "mcpServers": {
    "trusty-review": {
      "command": "trusty-review",
      "args": ["serve", "--stdio"]
    }
  }
}
```

### MCP tools

| Tool | Description |
|------|-------------|
| `review_pr` | Review a GitHub PR by owner/repo/number |
| `review_diff` | Review a raw unified diff string |
| `review_health` | Probe service liveness and configuration |

#### `review_pr`

```json
{
  "name": "review_pr",
  "arguments": {
    "owner": "bobmatnyc",
    "repo":  "trusty-tools",
    "pr":    625,
    "reviewer_model": "bedrock/us.anthropic.claude-haiku-4-5"
  }
}
```

Returns a `ReviewResult` JSON object with:
- `grade` (A+ | A | A- | B+ | B | B- | C+ | C | C- | D+ | D | D- | F) — letter grade
- `verdict` (APPROVE | APPROVE* | REQUEST_CHANGES | BLOCK | UNKNOWN)
- `findings` (array of findings with severity + confidence)
- `input_tokens` / `output_tokens` — LLM token usage
- `cost_estimate_usd` — estimated API cost

#### Grade → Verdict mapping

The verdict is derived from the grade per a fixed product decision (APPROVE floor = B-):

| Grade band           | Verdict              |
|----------------------|----------------------|
| A+, A, A-, B+, B, B- | APPROVE              |
| C+, C, C-            | APPROVE*             |
| D+, D, D-            | REQUEST_CHANGES      |
| F                    | BLOCK                |

The final verdict is `max(grade_verdict, severity_floor(findings))` — the grade
never produces a verdict weaker than what the severity floor already requires.
After verification (Phase 2), the grade is re-clamped to stay consistent with
the post-verification verdict.

When posted to GitHub, the review comment includes a footer:

```
Grade: B+ · 🤖 Reviewed by us.anthropic.claude-sonnet-4-6 · tokens ↑1234 ↓567 · est. $0.01
```

(↑ = input tokens, ↓ = output tokens). The footer appears identically in dry-run output.

#### `review_diff`

```json
{
  "name": "review_diff",
  "arguments": {
    "diff": "diff --git a/src/lib.rs ...",
    "context": "Refactoring the auth module",
    "reviewer_model": "bedrock/us.anthropic.claude-sonnet-4-6"
  }
}
```

#### `review_health`

```json
{ "name": "review_health", "arguments": {} }
```

Returns a health status object:

```json
{
  "status": "ok",
  "version": "0.3.2",
  "dry_run": true,
  "reviewer_model": "us.anthropic.claude-sonnet-4-6",
  "inference": "ok",
  "deps": {
    "trusty_search": {
      "required": true,
      "reachable": true
    },
    "trusty_analyze": {
      "required": false,
      "reachable": true
    }
  }
}
```

**Status values:**
- `ok` — all dependencies healthy and inference reachable.
- `degraded` — a required dependency (trusty-search) or inference is unreachable.
- `unknown` — cannot determine health state.

**Inference field values:**
- `ok` — AWS Bedrock and/or OpenRouter accessible.
- `unreachable` — both inference providers unreachable (network/DNS error).
- `auth_error` — inference provider reachable but auth failed (bad API key).
- `unknown` — inference probe could not determine status.

## Environment variables

| Variable | Default | Purpose |
|----------|---------|---------|
| `PR_INTELLIGENCE_DRY_RUN` | `true` | When `true`, no GitHub comments are posted |
| `TRUSTY_SEARCH_URL` | `http://127.0.0.1:7878` | trusty-search daemon URL |
| `PR_INTELLIGENCE_ANALYZER_URL` | `http://127.0.0.1:7879` | trusty-analyze daemon URL |
| `GITHUB_TOKEN` | — | GitHub personal access token for `review_pr` |
| `AWS_ACCESS_KEY_ID` / `AWS_SECRET_ACCESS_KEY` | — | AWS credentials for Bedrock |
| `OPENROUTER_API_KEY` | — | OpenRouter API key (when using OpenRouter provider) |
| `RUST_LOG` | `warn` | Tracing filter (logs to stderr) |

AWS credentials can also be supplied via `~/.aws/credentials`, IAM roles, or SSO.
The full AWS credential chain is supported.

## Reviewer model

The default reviewer model is `us.anthropic.claude-sonnet-4-6` on AWS Bedrock.

Override via:

- CLI flag: `--reviewer-model bedrock/us.anthropic.claude-haiku-4-5`
- Env var: `PR_INTELLIGENCE_REVIEWER_MODEL=bedrock/us.anthropic.claude-haiku-4-5`
- Config file: `$XDG_CONFIG_HOME/trusty-review/config.toml`

Provider prefix convention:
- `bedrock/<id>` — AWS Bedrock Converse API (no API key needed, uses AWS credential chain)
- `openrouter/<id>` — OpenRouter (requires `OPENROUTER_API_KEY`)
- Bare id — uses the configured default provider

## Cargo features

| Feature | Default | Description |
|---------|---------|-------------|
| `http-server` | yes | Axum HTTP daemon (`serve` subcommand without `--stdio`) |
| `mcp` | yes | MCP stdio JSON-RPC service (`serve --stdio`) |

## License

Elastic License 2.0 — see [LICENSE](LICENSE).
