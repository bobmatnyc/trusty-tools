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

## Installation

### Install from prebuilt binary

Download the latest prebuilt binary for your platform from the
[GitHub Releases](https://github.com/bobmatnyc/trusty-tools/releases) page.
Binaries follow the tag convention `trusty-review-v<version>`:

| Platform | Archive name |
|----------|-------------|
| macOS arm64 | `trusty-review-aarch64-apple-darwin.tar.gz` |
| Linux x86\_64 (glibc) | `trusty-review-x86_64-unknown-linux-gnu.tar.gz` |

Extract and place the `trusty-review` binary on your `PATH`.

### Install with cargo

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-review --locked
```

This builds and installs the `trusty-review` binary from the latest `main` branch.

### With Homebrew (recommended)

```bash
brew tap bobmatnyc/trusty
brew install trusty-review
```

Or install directly without tapping:

```bash
brew install bobmatnyc/trusty/trusty-review
```

Homebrew provides:
- Automatic updates via `brew upgrade trusty-review`
- Standard macOS / Linux PATH integration
- Easy dependency management

### Prerequisites

> **Required:** A GitHub token (`GITHUB_TOKEN`) or GitHub App credentials for PR
> fetching and (optionally) posting review comments. Set
> `PR_INTELLIGENCE_DRY_RUN=false` to enable comment posting (default: dry-run).
>
> **LLM credentials:** AWS Bedrock credentials (env vars, `~/.aws/credentials`,
> IAM role, or SSO) for the default `bedrock/` provider, or `OPENROUTER_API_KEY`
> for OpenRouter models.
>
> **Contributor profiling** (`trusty-review profile`): requires a pre-populated
> `tga` SQLite database. Set `TRUSTY_TGA_DB` or pass `--db <path>`. Compiled in
> by default; omit with `--no-default-features --features http-server,mcp` for a
> slimmer build without `tga`/`rusqlite` compilation.
>
> **Sidecar services** (optional, degrade gracefully when absent):
> - **trusty-search** on `:7878` — code-context hybrid search for richer reviews
> - **trusty-analyze** on `:7879` — complexity and quality metrics
>
> ```bash
> cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-search --locked
> cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze --locked
> trusty-search start
> trusty-analyze serve
> ```

## Quick start — one-shot review

```bash
# Review a GitHub PR (Bedrock credentials required)
trusty-review run owner repo 123

# Review a local unified diff file
trusty-review run --local-diff /path/to/patch.diff

# Review a diff piped in on stdin
git diff origin/main...HEAD | trusty-review run --local-diff -

# Review an arbitrary git ref range directly (no manual `git diff` step) —
# runs `git diff -M <base>...<head>` in the current directory. `--head`
# defaults to HEAD (the last commit, not the working tree).
trusty-review run --base origin/main
trusty-review run --base origin/main --head my-feature-branch

# Point a review at a checkout the daemon has never indexed — no manual
# `trusty-search index <dir>` needed first.
trusty-review run --base origin/main --source-root ~/code/some-other-checkout

# Override the reviewer model
trusty-review run owner repo 123 --reviewer-model bedrock/us.anthropic.claude-haiku-4-5

# Compare models
trusty-review compare owner repo 123
trusty-review compare --base origin/main --models bedrock/us.anthropic.claude-haiku-4-5,bedrock/us.anthropic.claude-sonnet-4-6
```

> `--local-diff` (file or `-` for stdin) and `--base`/`--head` (git ref range)
> are always dry-run — like every non-GitHub source, they can never post a
> live PR comment (issue #2993). `--base` and `--local-diff` are mutually
> exclusive.

### Explicit source context: `--source-root` (issue #2994)

Code context normally flows through a trusty-search index keyed by
`TRUSTY_SEARCH_INDEX` (explicit) or auto-derived from the current directory's
git root against the daemon's index registry. That means reviewing an
arbitrary checkout — a fresh worktree, a repo the daemon has never seen —
required first running `trusty-search index <dir>` out of band.

`--source-root <dir>` (on both `run` and `compare`) resolves this explicitly:

- If `<dir>` already matches a registered trusty-search index (same
  longest-root-path matching as the CWD/env auto-derive, just against an
  explicit directory), that index is used — no behaviour change from today's
  auto-derive, just an ergonomic override for a one-off review of a directory
  other than the CWD.
- If `<dir>` does **not** match a registered index, the review proceeds in
  **diff-only mode**: no code-context retrieval, with a clear notice printed
  to stderr and prepended as a banner in the review body. Reviews never
  silently query the wrong project's index. (An ephemeral/ad-hoc index was
  considered but deferred — it interacts with the ephemeral-index-leak
  investigation (issue #2914) and reliable cleanup isn't trivially safe from
  this crate alone; diff-only-with-notice is the safe default. Ephemeral
  indexing remains a documented follow-up.)
- An explicit `TRUSTY_SEARCH_INDEX` always wins over `--source-root` — it
  remains the fully-explicit override; `--source-root` is the ergonomic
  one-off path. Omitting `--source-root` entirely is a no-op: existing
  `TRUSTY_SEARCH_INDEX`/CWD-derive behaviour is unchanged.

`--context-path <glob>` (scoping which source paths are eligible as retrieved
context) was proposed alongside `--source-root` but is **not** implemented in
this pass — it is deferred to a follow-up issue.

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
Grade: B+ · 🤖 Reviewed by Trusty-Review (`us.anthropic.claude-sonnet-4-6`) · tokens ↑1234 ↓567 · est. $0.01
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

## Context sources & the conformance gate

`trusty-review` enriches a review with external **context sources** (JIRA,
Confluence, GitHub Issues, and the intent/method-**conformance** back gate). Each
source resolves independently from layered config — an env var beats a TOML key
beats the built-in default (the `ContextSourcesConfig` struct):

| Source | Enable env var | Mode env var | TOML table | Default |
|--------|----------------|--------------|------------|---------|
| JIRA | `TRUSTY_REVIEW_CONTEXT_JIRA_ENABLED` | `…_JIRA_MODE` | `[context.sources.jira]` | auto (on creds) |
| Confluence | `TRUSTY_REVIEW_CONTEXT_CONFLUENCE_ENABLED` | `…_CONFLUENCE_MODE` | `[context.sources.confluence]` | auto (on creds) |
| GitHub Issues | `TRUSTY_REVIEW_CONTEXT_GITHUB_ISSUES_ENABLED` | `…_GITHUB_ISSUES_MODE` | `[context.sources.github_issues]` | auto (on creds) |
| **Conformance** | `TRUSTY_REVIEW_CONTEXT_CONFORMANCE_ENABLED` | `…_CONFORMANCE_MODE` | `[context.sources.conformance]` | **DISABLED** |

Enable values are lenient (`true`/`1`/`yes`/`on`); mode is `live` (only mode
supported today) or `semantic` (not yet implemented for these sources).

### The `conformance` back gate (DOC-15)

The conformance source is the **BACK gate** of the intent/method-conformance
capability ([`docs/specs/intent-conformance.md`](../../docs/specs/intent-conformance.md)).
During review it resolves "what method did the ticket/spec prescribe?" via the
shared intent-source resolver (ISR) and surfaces it so the reviewer LLM can flag a
diff that **explicitly contradicts** that method (matrix M5). A gap or an
unresolved intent surfaces nothing — the gate is conservative and fail-open, so it
never manufactures a false-positive finding.

It is **default-DISABLED** (unlike the other sources, it does not auto-enable on
mere credential presence) because it issues a GitHub ticket fetch and is opt-in.
Turn it on explicitly:

```bash
# Env (one-shot)
TRUSTY_REVIEW_CONTEXT_CONFORMANCE_ENABLED=true trusty-review serve

# …or in $XDG_CONFIG_HOME/trusty-review/config.toml
[context.sources.conformance]
enabled = true
mode = "live"
```

The gate is backed by the `intent_source` module in **trusty-common**, gated
behind that crate's **`intent-source`** Cargo feature (which `trusty-review`
already enables). See the [Cargo features](#cargo-features) note below.

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

## Report generation

`trusty-review report` generates a CAST-style technical due-diligence report
from repository inspection — a structured markdown + JSON pair with executive
summary, per-application scorecards, findings by severity, risk registers, and
graph-ready datasets. Full design detail:
[docs/trusty-review/spec/report-generation.md](../../docs/trusty-review/spec/report-generation.md).

```bash
trusty-review report --manifest dd/acme.toml \
  --template report-technical-dd-cast --out /tmp/dd-reports
```

### Manifest

A single TOML file drives the run: a `[report]` section (title, optional
`template`/`analyst`/`corpus`) plus one or more `[[repositories]]` entries.
Each repository declares **exactly one** of `path` (local checkout) or
`remote` (`owner/repo`, with optional `username` for attribution), plus an
optional `ref` and a pre-produced trusty-analyze `metrics` JSON file:

```toml
[report]
title = "Acme Technical DD"

[[repositories]]
name    = "Acme Web"
path    = "/path/to/local/checkout"   # OR remote = "owner/repo"
ref     = "main"
metrics = "acme-metrics.json"
```

Local `path` entries get deterministic git enrichment (branch, short SHA,
origin, dirty flag); missing deterministic data renders as an explicit honesty
marker (e.g. `not stated in source data`) rather than being invented — a
convention enforced throughout the deterministic fill (M1).

### `--synthesize` (M2, opt-in LLM synthesis)

Off by default — without it, output is the byte-for-byte deterministic M1
fill. `--synthesize` layers LLM-written prose onto the executive summary, Top
Risks table, and RED/AMBER finding narratives only; **GREEN findings are
never synthesized** (filtered out before the prompt is built, so it's a
structural guarantee, not a prompt instruction). It reuses the crate's
existing reviewer LLM provider (no new client/dependency). Any provider,
parse, or guardrail failure **fails closed** to the deterministic output with
a visible `synthesis: unavailable (<reason>)` note — never a partial-trusted
result. A deterministic numeric guardrail additionally rejects any
synthesized field that cites a figure not present in the underlying report
data.

### `--corpus` / `--corpus-add` / `--benchmark` (M3, benchmark corpus)

Opt-in, fully deterministic cross-repo percentile/quartile placement against
a local corpus of accumulated per-repository metrics snapshots (the
trusty-review analogue of CAST's Appmarq peer benchmark). Snapshots are
privacy-redacted — the source path/URL is stored as a basename only, never
the full path or remote URL. `--corpus <dir>` resolves the corpus location
(falling back to the manifest's `[report].corpus` key, then a per-user XDG
data directory); `--corpus-add` appends a snapshot per analyzed repository
after a successful run; `--benchmark` computes and fills percentile/quartile
placement tables. A corpus with fewer than 5 peers is never ranked — the
report discloses `benchmark: corpus too small (n=<peers>)` instead of a
fabricated placement (the small-n honesty gate).

### `--instructions <md>` (analyst brief)

`--instructions <file>` (precedence over manifest `[report].instructions`)
hands the generator a free-form markdown brief — focus areas, deal concerns,
questions. It is always recorded verbatim as an `## Analyst Instructions`
section; under `--synthesize` it is additionally injected as focus
directives that steer emphasis in the executive summary and RED/AMBER prose.
Instructions steer emphasis only — they never authorize invention and never
relax the numeric guardrail or the no-green rule. A missing file is a hard
error; an empty file warns and proceeds as absent.

### Inference-first output: repo scanning, provenance, section instructions

Without `--synthesize`, and even without an external trusty-analyze metrics
file, a local-path repository still gets a substantive report: `src/report/scan.rs`
computes a **measured** baseline directly from the checkout — tracked file
list (`git ls-files`, or a filtered walk for non-git paths), total LoC + a
per-language breakdown, file counts, and top declared dependencies from
`package.json`/`Cargo.toml`/`pyproject.toml`/`go.mod`. An external metrics
JSON is treated as enrichment layered on top of the scan — where both provide
a figure, the declared metrics win; where only the scan has it, the measured
figure fills the field.

Every substantive value in the rendered report carries a trailing **provenance
marker** — `⁽ᵐ⁾` measured (from the repo), `⁽ᵈ⁾` declared (manifest/analyst/metrics
input), or `⁽ⁱ⁾` inferred (LLM judgement grounded in repo evidence); a
genuinely-unknowable field is dropped rather than shown with a marker. A
one-line legend renders once near the top of the report.

Templates may override the instruction given to each of the three
LLM-synthesized sections (`executive_summary`, `top_risks`,
`finding_elaboration`) with a `<!-- instruct:<section_id> ... -->` comment
anywhere in the template file; the override replaces the built-in generic
instruction for that section only and never reaches rendered output (it is
stripped like any other non-`dataset:` comment). The analyst `--instructions`
brief still layers on top as an additive emphasis overlay.

Full detail on the scan heuristics, provenance model, and instruction
layering: [docs/trusty-review/spec/report-generation.md](../../docs/trusty-review/spec/report-generation.md).

### `--analyze` (deterministic trusty-analyze integration)

`--analyze` populates the metrics-driven sections (the complexity-distribution
chart + RED/AMBER findings) from a live trusty-analyze daemon (`:7879`)
without an LLM or a hand-authored metrics JSON. It is fully deterministic and
fail-open: any probe, fetch, or parse failure degrades to the built-in scan
rather than erroring — a missing analyze index is never fatal. Full detail:
[docs/trusty-review/spec/report-generation.md](../../docs/trusty-review/spec/report-generation.md).

### Mermaid charts (`--no-mermaid`)

Every populated Graph-Ready Data Appendix table (tagged with a
`<!-- dataset: <slug> | chart: <type> | x: … | y: … -->` marker) gets a
Mermaid chart rendered directly beneath it — `bar`/`stacked-bar` as
`xychart-beta`, `radar` as `radar-beta` (Mermaid ≥ 11.6), `heatmap` has no
Mermaid equivalent and falls back to a note. This is on by default, purely
deterministic (no LLM, no network) — a rendering pass over already-filled
table rows. Disable it with `--no-mermaid` or the manifest `[report] mermaid
= false` key (the flag always wins); with it disabled the report is
byte-identical to the pre-Mermaid output.

### Templates

Two vendor-neutral, placeholder-only templates ship in
`crates/trusty-review/templates/`: `report-technical-dd` (generic) and
`report-technical-dd-cast` (CAST-specific health factors, ISO-5055 domains,
Appmarq-style benchmarks). Override either by dropping a same-named file in
the XDG template override directory (`~/.trusty-review/templates/`, checked
before the bundled `include_str!()` default — same pattern as the reviewer's
`VoiceLoader`).

### No-green-analysis convention

Across the whole report-generation feature, GREEN (healthy) findings are
rendered as a one-line topic list only — no elaboration, root-cause prose, or
recommendation ever attaches to a GREEN item, whether from deterministic fill
or LLM synthesis. This keeps every report focused on what's actually
actionable.

## Cargo features

| Feature | Default | Description |
|---------|---------|-------------|
| `http-server` | yes | Axum HTTP daemon (`serve` subcommand without `--stdio`) |
| `mcp` | yes | MCP stdio JSON-RPC service (`serve --stdio`) |
| `profile` | yes | Longitudinal contributor-profiling pipeline (`profile` subcommand); pulls in `tga` + `rusqlite` |

> The conformance back gate (above) is backed by **trusty-common**'s
> **`intent-source`** feature, which `trusty-review` enables unconditionally
> (it is part of the dependency declaration, not a `trusty-review` feature). That
> feature gates the shared intent-source resolver (ISR) so the other trusty-common
> consumers that do not need it pay nothing.

Slim build (no contributor profiling, no `tga`/`rusqlite` compilation):

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-review \
  --locked --no-default-features --features http-server,mcp
```

## License

MIT — see [LICENSE](LICENSE).
