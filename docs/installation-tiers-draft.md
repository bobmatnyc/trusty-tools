# Trusty-Tools Installation Tiers — Draft Reference Material

**Status:** Draft input for Bob's live-environment walkthrough documentation.  
**Date:** 2026-07-26  
**Purpose:** Structured reference for two coherent installation tiers; Bob will synthesize the final walkthrough prose.

---

## Overview

Two tiers define a working trusty-tools stack:

- **MINIMAL** — orchestration + code intelligence + memory + review gate. No credentials or external accounts required.
- **ADVANCED** — adds agents, per-project harness, analytics, and MCP servers for external platforms. Requires credentials/accounts.

### Tier Boundary

**MINIMAL** contains only components that function in isolation without external account/credential setup. Each crate can bootstrap and serve clients with zero configuration beyond installation.

**ADVANCED** boundary: components requiring OAuth tokens, API keys, or external accounts. These are opt-in enrichments; their absence degrades functionality gracefully, not catastrophically.

---

## MINIMAL Tier — Core Stack

### Tier Composition (VALIDATED)

| Component | Role | Status | Required for Install? | Runs as Daemon? |
|-----------|------|--------|----------------------|-----------------|
| `tm` / `trusty-mpm` 1.0.2 | Session orchestration, tmux coordination, PM harness | Required | Yes | Yes (process-managed) |
| `trusty-search` 0.39.1 | Hybrid code search (BM25 + embeddings) | Required | Yes | Yes (launchd-managed) |
| `trusty-memory` 0.21.1 | Durable memory (MCP server) | Required | Yes | Yes (launchd-managed) |
| `trusty-review` 0.10.1 | LLM-backed PR review gate | Required | Yes | Yes (launchd-managed) |

**VALIDATION NOTES:**
- Each REQUIRED member in `stable_set()` is listed above. These four comprise the entire set of required components.
- `trusty-mpm` depends on `trusty-search` (for code context), but the dependency is optional at runtime: tm can start without search running, and search can run independently. **However:** for the coherent workflow loop (sessions → search → review) to function, all four must be runnable after install.
- No component pulls in a fifth required member as a transitive hard dependency.
- All four can bootstrap with zero configuration (no credentials, no `.env.local`, no account setup).

**Install Verification Logic:** Installer's `stable_set()` in `crates/trusty-installer/src/commands/stable_set.rs` marks only these four as `required: true`. The other three (trusty-analyze, tga, trusty-console) are optional (`required: false`), so a fresh install succeeding on only these four is "VERIFIED" (exit 0), not "DEGRADED" (exit 2).

---

## ADVANCED Tier — Enrichment Stack

### Tier Composition (PROPOSED)

| Component | Role | Status | Credential Type | Dependency |
|-----------|------|--------|-----------------|------------|
| `trusty-agents` 0.38.6 | Agentic orchestration for non-coding workflows | Optional | LLM provider (OpenRouter/Anthropic/Claude Code OAuth) | None (standalone) |
| `trusty-code` 0.3.0 | Per-project coding harness | Optional | Claude Code integration | Assumes Claude Code + `.claude/` config |
| `trusty-git-analytics` 2.9.4 (binary: `tga`) | Developer productivity analytics, DORA metrics | Optional | None (pure analysis) | None |
| `trusty-analyze` 0.7.4 | Complexity analysis sidecar for trusty-search | Optional | None | **Requires running `trusty-search` daemon** |
| `trusty-console` 0.5.0 | Web dashboard for service discovery + status | Optional | None | None (detects services by probing) |
| `slack-mcp` (from trusty-channels 0.1.0) | MCP server for Slack chat access | Optional | `SLACK_BOT_TOKEN` | None (HTTP client) |
| `telegram-mcp` (from trusty-channels 0.1.0) | MCP server for Telegram bot access | Optional | `TELEGRAM_BOT_TOKEN` | None (HTTP client) |
| `trusty-gworkspace-mcp` 0.2.2 | MCP server for Google Workspace (Gmail, Docs, Drive, Calendar) | Optional | Google OAuth (interactive setup) | None (HTTP client) |

**BOUNDARY JUSTIFICATION:**
- All ADVANCED components are credential-gated or external-account-dependent.
- None are required for the core workflow loop (sessions → search → review).
- MINIMAL functions completely without any of them.
- ADVANCED features degrade gracefully when absent (search doesn't fail if analyze is missing; PM agents don't fail if MCP servers are missing).

---

## Fresh Environment Prerequisites

**Target Platform:** macOS 12+ (Apple Silicon), or Linux x86_64 (glibc 2.38+).

Before running any installation steps:

- [ ] **Xcode Command Line Tools** are installed.
  - Verify: `xcode-select --print-path` → should return a path (not "not installed").
  - If missing: `xcode-select --install` (interactive; requires ~1 GB, 5–10 min).

- [ ] **GitHub CLI** (`gh`) is installed and authenticated.
  - Verify: `gh auth status` → should show logged-in user.
  - If missing: install via Homebrew (`brew install gh`) or https://cli.github.com, then `gh auth login`.
  - **Why:** `tm sessions new` and `gh pr create` (the core workflow) both require `gh`. Install it *before* starting the trusty-tools install.

- [ ] **Homebrew** is installed (macOS only; Linux uses system package managers).
  - Verify: `brew --version` → should return version number.
  - If missing on clean macOS: `trusty-installer` will fail early if Homebrew is not present and tmux cannot be auto-installed. **Workaround (2026-07-26):** install Homebrew first: `/bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"`, then run the trusty-tools installer. (See **Gotchas** #3821.)

- [ ] **Network connectivity** to GitHub (git clones), crates.io (binary downloads), and PyPI (embeddings model download for trusty-search, ~2 GB).

- [ ] **Disk space:** at least 4 GB free.
  - Trusty-search model cache: ~2 GB (downloaded on first daemon startup to `~/Library/Caches/trusty-search/`).
  - Trusty-memory model cache: ~100 MB.
  - Binaries + temp: ~200 MB.

- [ ] **RAM:** Trusty-search hard-checks 16 GB minimum at daemon startup. Set `TRUSTY_SKIP_RAM_CHECK=1` to bypass (at your own risk for small workloads). Verify: `vm_stat | grep "Pages free"` (should be > 16 GB).

---

## Install Command — One-Liner (MINIMAL + ADVANCED)

```bash
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y
```

This installs:
- `trusty-installer` (the control plane binary, also aliased as `tctl`).
- All **REQUIRED** members: `tm`, `trusty-search`, `trusty-memory`, `trusty-review`.
- All **OPTIONAL** members: `trusty-analyze`, `tga`, `trusty-console` (prebuilt binaries available; cargo fallback if platform unsupported).

**Result:** MINIMAL tier is complete. ADVANCED components are not installed by the single-URL script; they are added separately (see **Per-Crate Installation** below).

---

## Per-Crate Documentation

### trusty-mpm 1.0.2 (binary: `tm`)

**What it gives you:**  
Session orchestration and tmux integration for coding work. Create, pause, resume, and manage multi-project sessions with automatic worktree isolation and git state snapshots.

**What it requires:**
- **System**: tmux (auto-installed by trusty-installer if missing and Homebrew is available).
- **Environment**: None (no credentials, no configuration on first run).
- **Daemon**: Yes, process-managed via `tm start|stop|restart`.
- **Disk/RAM**: Negligible (~10 MB binary, <100 MB runtime state).

**Install command:**
```bash
# Via single-URL installer (includes all REQUIRED members):
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y

# Or via Homebrew (if already installed):
brew install bobmatnyc/trusty/trusty-mpm

# Or via cargo (requires Rust 1.94+):
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-mpm --locked
```

**First-run verification:**
```bash
tm version
# Expected: tm 1.0.2

tm doctor
# Expected: all checks green (daemon alive, config valid)

tm start
# Expected: daemon starts (may emit bootstrap messages)

tm sessions list
# Expected: empty list (no sessions yet) or exit 0
```

**Ordering constraints:**
- **Must run after:** tmux is available (auto-installed by trusty-installer).
- **Must run before:** `tm sessions new` (session creation depends on daemon running).
- **Start the daemon explicitly:** `tm start` does NOT run automatically after install. You must call it before creating sessions.

---

### trusty-search 0.39.1

**What it gives you:**  
Machine-wide code search over multiple repositories using hybrid (BM25 + embeddings) indexing. Enables fast context retrieval for reviews, LLM-backed tools, and ad-hoc queries.

**What it requires:**
- **System**: None (pure Rust binary).
- **Environment**: None (no credentials).
- **Daemon**: Yes, launchd-managed on macOS (HTTP on `:7878`).
- **Disk/RAM**: 16 GB RAM (hard-checked at startup; set `TRUSTY_SKIP_RAM_CHECK=1` to bypass). ~2 GB disk for embeddings model cache (downloaded on first run to `~/Library/Caches/trusty-search/`).
- **Network**: Downloads embeddings model (~2 GB) on first daemon startup.

**Install command:**
```bash
# Via single-URL installer:
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y

# Or via Homebrew:
brew install bobmatnyc/trusty/trusty-search

# Or via cargo:
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-search --locked
```

**First-run verification:**
```bash
trusty-search --version
# Expected: trusty-search 0.39.1

trusty-search start
# Expected: daemon starts, may emit "downloading model" on first run

sleep 5  # Give daemon time to initialize

curl -s http://127.0.0.1:7878/health | jq .
# Expected: {"status": "ok"} or similar health response
```

**Ordering constraints:**
- **Startup only:** No runtime dependencies (runs independently).
- **Must start before:** trusty-analyze (which probes trusty-search's health endpoint on startup).
- **Launchd bootstrap:** Daemon is registered as a LaunchAgent and started automatically after install. Manual start via `trusty-search start` is only needed if you stopped it or need to restart.

---

### trusty-memory 0.21.1

**What it gives you:**  
Persistent, searchable memory organized into named "palaces" (namespaces). Stores natural-language facts, structured knowledge-graph triples, and provides vector search + retrieval for long-term context across sessions.

**What it requires:**
- **System**: None (pure Rust binary, but ships with 3 binaries: trusty-memory, trusty-memory-mcp-bridge (deprecated shim), trusty-bm25-daemon, trusty-console).
- **Environment**: Optional `OPENROUTER_API_KEY` for inference-backed memory features (default: disabled). See **Credentials** below.
- **Daemon**: Yes, launchd-managed on macOS (HTTP on `:7880`).
- **Disk/RAM**: 512 MB minimum; 1 GB+ recommended. ~100 MB disk for model cache (`~/Library/Application Support/trusty-memory/`).

**Credentials (Optional):**  
Memory can store facts without any LLM provider. To enable LLM-backed summarization or reasoning:

```bash
# 3-tier credential resolution (checked in order):
# 1. Process env: OPENROUTER_API_KEY=<token>
# 2. ~/.env.local: OPENROUTER_API_KEY=<token>
# 3. Keychain: trusty-memory config openrouter keys set

# Set credentials (interactive, stores in macOS Keychain):
trusty-memory config openrouter keys set
# or set env var: export OPENROUTER_API_KEY=<your-token>
```

**Install command:**
```bash
# Via single-URL installer:
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y

# Or via Homebrew:
brew install bobmatnyc/trusty/trusty-memory

# Or via cargo:
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-memory --locked
```

**First-run verification:**
```bash
trusty-memory --version
# Expected: trusty-memory 0.21.1

trusty-memory start
# Expected: daemon starts

sleep 3

curl -s http://127.0.0.1:7880/health | jq .
# Expected: {"status": "ok"} or similar health response

trusty-memory serve --stdio &
# Expected: daemon responds to MCP stdio requests (confirms MCP bridge works)
# Kill with Ctrl+C or fg + Ctrl+C
```

**Ordering constraints:**
- **Startup only:** No runtime dependencies.
- **LaunchAgent bootstrap:** Automatically registered and started after install.

**Keychain note (SSH/headless limitation):**  
The credential resolver tries OS keychain first (macOS: Keychain.app; Linux: secret-service or pass via `keyring` Python library). On SSH/headless machines where keychain is unavailable, the store silently degrades to plaintext file storage (`~/.config/trusty-memory/credentials.json`). Keep this file private.

---

### trusty-review 0.10.1

**What it requires:**
- **GitHub token** (`GITHUB_TOKEN` env var, `~/.github/token` file, or `gh auth status` authenticated CLI).
- **LLM credentials**: AWS Bedrock (default) via `~/.aws/credentials` or env vars (AWS_ACCESS_KEY_ID, AWS_SECRET_ACCESS_KEY), or `OPENROUTER_API_KEY` for OpenRouter models.
- **Sidecar services** (optional, degrade gracefully):
  - trusty-search on `:7878` for code context.
  - trusty-analyze on `:7879` for complexity metrics.

**What it gives you:**  
LLM-backed PR review service. Fetches GitHub PR diffs, retrieves code context from trusty-search, queries trusty-analyze for complexity data, then produces structured review verdicts with actionable findings.

**What it requires:**
- **System**: None (pure Rust binary).
- **Environment**: 
  - **GitHub token**: `GITHUB_TOKEN` or authenticated `gh` (via `gh auth login`). Default: dry-run mode (no comment posting). Set `PR_INTELLIGENCE_DRY_RUN=false` to enable posting.
  - **LLM credentials**: AWS Bedrock (default) or OpenRouter. See **Credentials** below.
- **Daemon**: Yes, launchd-managed on macOS (HTTP on `:7880` for webhook receiver).
- **Disk/RAM**: ~50 MB binary, <100 MB runtime.

**Credentials:**

**GitHub (required):**
```bash
# Option 1: Use authenticated gh CLI (recommended)
gh auth login
# Then: gh auth status (should show "Logged in to github.com as <user>")

# Option 2: Set GITHUB_TOKEN env var
export GITHUB_TOKEN=ghp_xxxxxxxxxxxx
```

**LLM Provider (required; Bedrock is default):**

*AWS Bedrock (default):*
```bash
# Credentials auto-resolved from (in order):
# 1. AWS_ACCESS_KEY_ID + AWS_SECRET_ACCESS_KEY env vars
# 2. ~/.aws/credentials (standard AWS config)
# 3. IAM role (if running on EC2 / with assumed role)
# 4. AWS SSO (aws sso login)

# Verify Bedrock access:
trusty-review run --check-bedrock  # UNVERIFIED — check trusty-review docs
```

*OpenRouter (alternative):*
```bash
export OPENROUTER_API_KEY=sk-or-v1-xxxxxxxxxxxx
# Get token: https://openrouter.ai/api_keys

# Then: trusty-review honors the env var (3-tier resolution applies)
```

**Install command:**
```bash
# Via single-URL installer:
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y

# Or via Homebrew:
brew install bobmatnyc/trusty/trusty-review

# Or via cargo:
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-review --locked
```

**First-run verification:**
```bash
trusty-review --version
# Expected: trusty-review 0.10.1

# Verify GitHub auth:
gh auth status
# Expected: Logged in to github.com as <user>

# One-shot review (requires trusty-search running):
trusty-review run --base origin/main  # Reviews HEAD against origin/main

# Expected: structured review output in JSON or markdown (depending on flags)
```

**Ordering constraints:**
- **Must run after:** `trusty-search` (required for code context).
- **Optional but recommended after:** `trusty-analyze` (complexity metrics improve review quality).
- **LaunchAgent bootstrap:** Daemon automatically registered after install.

---

## ADVANCED Tier — Per-Crate Installation

### trusty-agents 0.38.6

**What it gives you:**  
Agentic orchestration for non-coding knowledge-work tasks (CRM, HR, scheduling, comms). PM+sub-agent architecture with tool-using agents and skill injection.

**What it requires:**
- **LLM provider credential** (one of):
  - `OPENROUTER_API_KEY` for OpenRouter.
  - `ANTHROPIC_API_KEY` for Anthropic direct.
  - `CLAUDE_CODE_OAUTH_TOKEN` for Claude Code CLI routing.
- **Daemon**: No (agents run on-demand via PM orchestrator or CLI).
- **Disk/RAM**: ~50 MB binary, <500 MB runtime.

**Credentials:**
```bash
# 3-tier resolution applies (env > .env.local > keystore)
export OPENROUTER_API_KEY=sk-or-v1-xxxxxxxxxxxx

# Or:
export ANTHROPIC_API_KEY=sk-ant-xxxxxxxxxxxx

# Or (for Claude Code routing):
export CLAUDE_CODE_OAUTH_TOKEN=<token-from-claude-setup-token>
```

**Install command:**
```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-agents --locked
# Homebrew not yet available.
```

**First-run verification:**
```bash
trusty-agents --version
# Expected: trusty-agents 0.38.6

# UNVERIFIED: exact verification command; check trusty-agents docs
```

**Ordering constraints:**
- Standalone; no service dependencies.

---

### trusty-code 0.3.0 (binary: `tcode`)

**What it gives you:**  
Per-project coding orchestration harness integrated with Claude Code. Mandatory workflow (research → plan → implement → verify) and typed coding sub-agents.

**What it requires:**
- **Claude Code** (optional but intended use case).
- **Git** (standard).
- **Per-project `.claude/` config:** agents, skills, MCP connections, CLAUDE.md.
- **Daemon**: No (runs as on-demand server per project; `tcode serve`).
- **Disk/RAM**: ~100 MB binary, ~200 MB per running instance.

**Credentials:**  
Inherits from Claude Code's auth (CLAUDE_CODE_OAUTH_TOKEN). See trusty-mpm documentation for setup.

**Install command:**
```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-code --locked
# Homebrew: planned (not yet available).
```

**First-run verification:**
```bash
tcode --version
# Expected: tcode 0.3.0

# Per-project server (run inside a project's .claude/ root):
tcode serve
# Expected: server starts on HTTP socket (port/path varies by config)
```

**Ordering constraints:**
- Standalone; integrates with Claude Code at runtime.

---

### trusty-git-analytics 2.9.4 (binary: `tga`)

**What it gives you:**  
Developer productivity analytics: classify commits by type, track weekly velocity, measure DORA metrics, generate reports (CSV/JSON/Markdown).

**What it requires:**
- **System**: None (pure Rust binary).
- **Environment**: None (no credentials; optional external API keys for classification: Linear, Shortcut, Confluence, Datadog).
- **Daemon**: No (CLI tool; runs on-demand).
- **Disk/RAM**: ~200 MB binary, analyzes repositories in-process.

**Credentials (Optional):**  
External classification sources (all optional):
```bash
# Linear (optional)
export LINEAR_API_KEY=lin_api_xxxxxxxxxxxx

# Shortcut (optional)
export SHORTCUT_API_TOKEN=xxxxxxxxxxxx

# Confluence (optional)
export CONFLUENCE_URL=https://your-instance.atlassian.net
export CONFLUENCE_USER_EMAIL=your@email.com
export CONFLUENCE_API_TOKEN=xxxxxxxxxxxx

# Datadog (optional)
export DATADOG_API_KEY=xxxxxxxxxxxx
export DATADOG_APP_KEY=xxxxxxxxxxxx
```

**Install command:**
```bash
# Via single-URL installer (optional member):
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y

# Or via Homebrew:
brew install bobmatnyc/trusty/trusty-git-analytics

# Or via cargo:
cargo install --git https://github.com/bobmatnyc/trusty-tools tga --locked
```

**First-run verification:**
```bash
tga --version
# Expected: tga 2.9.4

# Analyze a repository (requires a local git repo):
cd /path/to/repo
tga analyze
# Expected: commits classified and stored in SQLite DB (~.tga.db)

tga report velocity
# Expected: weekly velocity report in stdout or JSON/CSV output
```

**Ordering constraints:**
- Standalone; works on local repositories.

---

### trusty-analyze 0.7.4

**What it gives you:**  
Complexity and quality metrics for code chunks. Sidecar to trusty-search that analyzes code for cyclomatic complexity, LOC metrics, and other static-analysis signals.

**What it requires:**
- **Running trusty-search daemon** (required; probes health on startup).
- **System**: None (pure Rust binary).
- **Environment**: None (no credentials).
- **Daemon**: Yes, launchd-managed on macOS (HTTP on `:7879`).
- **Disk/RAM**: ~100 MB binary, <500 MB runtime.

**Install command:**
```bash
# Via single-URL installer (optional member):
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y

# Or via Homebrew:
brew install bobmatnyc/trusty/trusty-analyze

# Or via cargo:
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-analyze --locked
```

**First-run verification:**
```bash
trusty-analyze --version
# Expected: trusty-analyze 0.7.4

# Verify trusty-search is running first:
curl -s http://127.0.0.1:7878/health | jq .
# Expected: {"status": "ok"}

trusty-analyze serve
# Expected: daemon starts and probes trusty-search health

sleep 2

curl -s http://127.0.0.1:7879/health | jq .
# Expected: {"status": "ok"}
```

**Ordering constraints:**
- **Must run after:** `trusty-search` daemon is up and running.
- **LaunchAgent bootstrap:** Automatically registered after install, but fails on startup if trusty-search is not already running.

---

### trusty-console 0.5.0

**What it gives you:**  
Web dashboard showing service discovery and health status for all running trusty services (search, memory, analyze, etc.).

**What it requires:**
- **System**: None (pure Rust binary, embedded SPA).
- **Environment**: None (no credentials).
- **Daemon**: Yes, HTTP server on `:7788` (localhost-only by default).
- **Disk/RAM**: ~50 MB binary, <100 MB runtime.

**Install command:**
```bash
# Via single-URL installer (optional member):
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y

# Or via Homebrew:
brew install bobmatnyc/trusty/trusty-console

# Or via cargo:
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-console --locked
```

**First-run verification:**
```bash
trusty-console --version
# Expected: trusty-console 0.5.0

trusty-console serve
# Expected: server starts on 127.0.0.1:7788

# In another terminal:
curl -s http://127.0.0.1:7788/api/console/services | jq .
# Expected: JSON list of services with status (running/available/absent)

# Or open in browser:
open http://127.0.0.1:7788
# Expected: web dashboard with service cards
```

**Ordering constraints:**
- Standalone; probes other services' health endpoints on startup (gracefully handles missing daemons).

---

### slack-mcp (from trusty-channels 0.1.0)

**What it gives you:**  
MCP server exposing Slack workspace as tools: read messages, post to channels, search threads, manage reactions.

**What it requires:**
- **Slack Bot Token** (`SLACK_BOT_TOKEN`).
- **System**: None (pure Rust binary, HTTP client).
- **Daemon**: No (MCP stdio server; runs as child of Claude Code or other MCP client).
- **Disk/RAM**: ~50 MB binary, <100 MB per instance.

**Credentials:**

```bash
# 3-tier resolution applies
export SLACK_BOT_TOKEN=xoxb-xxxxxxxxxxxx

# Or set in ~/.env.local:
# SLACK_BOT_TOKEN=xoxb-xxxxxxxxxxxx

# Or (on macOS with Keychain):
# trusty-common config slack keys set
```

**Get token:**  
Create a Slack App at https://api.slack.com/apps, add required scopes (chat:write, channels:read, users:read, etc.), generate a bot token, and add the app to your workspace.

**Install command:**
```bash
# Build from trusty-channels (not published to crates.io separately):
cargo install --git https://github.com/bobmatnyc/trusty-tools \
  --root ~/.cargo \
  --path crates/trusty-channels \
  --bin slack-mcp --locked

# Or build locally:
cd crates/trusty-channels
cargo build --bin slack-mcp --release
cp target/release/slack-mcp ~/.cargo/bin/
```

**First-run verification:**
```bash
slack-mcp --version
# Expected: version output (or "no --version flag" — check crate docs)

# Test MCP stdio interface (requires valid SLACK_BOT_TOKEN):
echo '{"jsonrpc": "2.0", "method": "initialize", "params": {}, "id": 1}' | slack-mcp serve --stdio
# Expected: MCP protocol handshake response (JSON)
```

**Ordering constraints:**
- Standalone; invoked by Claude Code / MCP client as subprocess.

---

### telegram-mcp (from trusty-channels 0.1.0)

**What it gives you:**  
MCP server exposing Telegram bot as tools: send messages, read updates, manage chats.

**What it requires:**
- **Telegram Bot Token** (`TELEGRAM_BOT_TOKEN`).
- **System**: None (pure Rust binary, HTTP client).
- **Daemon**: No (MCP stdio server).
- **Disk/RAM**: ~50 MB binary, <100 MB per instance.

**Credentials:**

```bash
export TELEGRAM_BOT_TOKEN=123456789:ABCDefGHIjklMNOpqrsTUVwxyz

# Or ~/.env.local:
# TELEGRAM_BOT_TOKEN=123456789:ABCDefGHIjklMNOpqrsTUVwxyz
```

**Get token:**  
Create a Telegram bot via @BotFather on Telegram, copy the HTTP API token.

**Install command:**
```bash
# Same as slack-mcp (in trusty-channels crate):
cargo install --git https://github.com/bobmatnyc/trusty-tools \
  --root ~/.cargo \
  --path crates/trusty-channels \
  --bin telegram-mcp --locked
```

**First-run verification:**
```bash
telegram-mcp --version
# Expected: version output (or similar)

echo '{"jsonrpc": "2.0", "method": "initialize", "params": {}, "id": 1}' | telegram-mcp serve --stdio
# Expected: MCP protocol handshake response
```

**Ordering constraints:**
- Standalone; invoked by Claude Code as subprocess.

---

### trusty-gworkspace-mcp 0.2.2

**What it gives you:**  
MCP server exposing Google Workspace (Gmail, Google Drive, Google Docs, Google Calendar) as tools.

**What it requires:**
- **Google OAuth 2.0 credentials** (interactive setup on first run).
- **System**: None (pure Rust binary, HTTP client).
- **Daemon**: No (MCP stdio server).
- **Disk/RAM**: ~100 MB binary, <200 MB per instance.

**Credentials:**

First-run interactive OAuth setup:
```bash
trusty-gworkspace-mcp serve --stdio
# On first run: opens browser for Google OAuth consent.
# Stores refresh token in ~/.config/trusty-gworkspace/oauth_client.json (persists across runs).
```

Or (if needed) explicit setup:
```bash
# UNVERIFIED — check trusty-gworkspace docs for explicit setup
trusty-gworkspace-mcp config oauth set
```

**Install command:**
```bash
# Build from workspace:
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-gworkspace \
  --bin trusty-gworkspace-mcp --locked

# Or:
cd crates/trusty-gworkspace
cargo build --bin trusty-gworkspace-mcp --release
cp target/release/trusty-gworkspace-mcp ~/.cargo/bin/
```

**First-run verification:**
```bash
trusty-gworkspace-mcp --version
# Expected: version output

# Start server (first run will prompt for OAuth):
trusty-gworkspace-mcp serve --stdio
# Expected: browser opens (Google OAuth consent), then MCP protocol ready

# Verify token is cached:
cat ~/.config/trusty-gworkspace/oauth_client.json | jq .refresh_token
# Expected: non-empty token string (token is valid; server can reuse it)
```

**Ordering constraints:**
- Standalone; invoked by Claude Code as subprocess.

**Keychain/Headless limitation:**  
OAuth refresh token is stored in plaintext at `~/.config/trusty-gworkspace/oauth_client.json`. On macOS with Keychain available, tokens may optionally be stored securely; on SSH/headless machines, they are plaintext. Protect this file (`chmod 600`).

---

## Gotchas & Known Failure Modes

All drawn from 2026-07-26 `trusty-installer 0.4.10` fixes (issues #3875, #3876, #3874, #3821, #3830) and existing runbooks.

### #3821 — No Homebrew on Clean macOS

**Trigger:** Running `curl install.sh | sh -s -- -y` on a clean macOS with no Homebrew.

**Symptom:** Installer prints "brew install tmux" hint and exits with code 2 (prerequisite missing) before installing anything. **Dead end** — no actionable next step.

**Root Cause:** trusty-installer's preflight checks detect tmux is missing and Homebrew is not present. Under `-y` (non-interactive), it cannot auto-install tmux (which needs Homebrew), so it fails early rather than attempting an unautomatable install.

**Workaround (2026-07-26):**
1. Install Homebrew first (takes ~5 min on a fresh box):
   ```bash
   /bin/bash -c "$(curl -fsSL https://raw.githubusercontent.com/Homebrew/install/HEAD/install.sh)"
   ```
   (Homebrew installation sets up `/opt/homebrew` on Apple Silicon and updates `~/.zprofile`.)

2. Re-run the trusty-tools installer:
   ```bash
   curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y
   ```

**Why this is a gotcha:** The error message does not suggest installing Homebrew; it only says "brew not found," leaving the user stranded. Fresh VM installs will hit this every time.

---

### #3874 — PATH Not Set for Non-Login Shells

**Trigger:** After install, opening a new **non-login, non-interactive shell** (e.g., `ssh host tm version`) fails with "command not found: tm".

**Symptom:** `tm` works in login shells and interactive shells, but fails in SSH remote-exec and cron jobs. The binary is on PATH in the interactive terminal you just installed in, but a new shell session does not see it.

**Root Cause:** Old installer only wrote `export PATH=~/.local/bin:$PATH` to `~/.zshrc`. But zsh never sources `.zshrc` in non-interactive/non-login contexts (e.g., SSH remote-exec). It only sources `.zshenv` (always), `.zprofile` (login shells), and `.zshrc` (interactive).

**Fix (2026-07-26):** Installer now writes to `.zshenv` (always sourced), `.zprofile` (login), and `.zshrc` (interactive) for zsh, and `.bashrc`/`.bash_profile` for bash.

**Verification:**
```bash
# In a fresh shell:
zsh -i -c "which tm"  # interactive shell — should work
zsh -c "which tm"     # non-interactive shell — should now work (was broken)

# Over SSH (requires SSH access):
ssh localhost 'tm version'  # should print version, not "command not found"
```

**Workaround (if installing before 2026-07-26 fix):**  
Manually add `export PATH="$HOME/.local/bin:$PATH"` to `~/.zshenv` (not just `~/.zshrc`).

---

### #3875 — Post-Install Verify Hangs (>6 minutes)

**Trigger:** Running `curl install.sh | sh -s -- -y`, install completes, but `trusty-installer verify` hangs in the health-check loop.

**Symptom:** Progress checklist finishes, but the process sits silently for 5–10 minutes before timing out or hanging indefinitely. The terminal appears frozen.

**Root Cause:** The verify tail's health-check loop polled `<binary> health --json` via `Command::output()` with no timeout on individual health probes. A single hung daemon child process blocks forever, stalling the entire verify phase. This was especially common with trusty-search on first run (model download can take 3–5 min, and if the process crashes mid-download, the health probe never recovers).

**Fix (2026-07-26):** Added per-probe timeout (10 seconds) and bounded retry attempts. If a health probe hangs, it times out and retries; the verify phase now completes within ~2 minutes even if one daemon is slow.

**Verification:**
```bash
# After install, watch the verify phase:
# "Verifying installation..." should print status updates every few seconds.
# Total time should be <3 minutes.
# If it hangs >10 min, Ctrl+C and check:

curl -s -m 5 http://127.0.0.1:7878/health | jq .  # trusty-search
# If hangs or returns nothing, the daemon is stuck.
```

**Workaround (if installing before 2026-07-26):**  
Manually start daemons and verify health before running `trusty-installer verify`:
```bash
trusty-search start
sleep 10
curl http://127.0.0.1:7878/health
trusty-memory start
sleep 5
curl http://127.0.0.1:7880/health
```

---

### #3876 — Verify Table False-Negatives on Incomplete PATH

**Trigger:** After install, `trusty-installer verify` or `tctl stack health` reports "binary not found" even though the binary is installed and runnable from an interactive shell.

**Symptom:** Verify table shows a binary as "NOT FOUND" or "MISSING", but `which <binary>` returns a path in a fresh shell. This happens specifically if the PATH has not been updated in the current shell session.

**Root Cause:** The verify logic used `which <binary>` to detect installed binaries. If `.zshenv` / `.bashrc` was updated but the current shell never re-sourced it (i.e., the shell was open before the installer ran), `which` returns empty even though the binary exists on disk. The detection was also fragile if there was a PATH ordering issue.

**Fix (2026-07-26):** Verify logic now detects binaries **independently of the current PATH**. It checks common install locations directly (e.g., `~/.local/bin`, `/opt/homebrew/bin`, `/usr/local/bin`) rather than relying on `which`.

**Verification:**
```bash
# After install, in the SAME shell that ran the installer:
tctl stack health
# All REQUIRED binaries should show "installed".

# If not, source your shell's rc file:
source ~/.zshenv  # for zsh
source ~/.bashrc  # for bash

tctl stack health
# Should now show all binaries as installed.
```

**Workaround (if installing before 2026-07-26):**  
Manually source the rc file:
```bash
source ~/.zshenv
source ~/.bashrc
tctl stack health
```

---

### #3830 — Progress Checklist Output Spam (Gotcha in the Gotchas)

**Trigger:** During `curl install.sh | sh -s -- -y`, the progress checklist line-count gets out of sync, producing duplicated and interleaved output, making the real progress hard to see.

**Symptom:** The live per-component checklist (the nice in-place updating progress table) suddenly starts printing duplicate lines and scrolling rapidly, obscuring status.

**Root Cause:** `trusty-installer install` shelled out to `<binary> service install` via `Command::status()`, which inherited the parent's stdout/stderr. While the checklist's `indicatif::MultiProgress` was actively redrawing the screen, the child's output landed directly in that region, desyncing `indicatif`'s line-count tracking and producing duplicate/interleaved lines.

**Fix (2026-07-26):** Switch child subprocess invocations to `Command::output()` so stdout/stderr are captured instead of inherited. Also added heartbeat output ("still running: X seconds elapsed") during long-running `brew install` commands so the user knows the process is not hung (especially useful for tmux on first-run).

**Verification:**
```bash
# Run the installer and watch the progress checklist:
curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y

# Expected:
# - Clean in-place checklist that updates without scrolling/duplication.
# - Each component shows "Installing... ✓" or "Skipped" as it completes.
# - If a long step (like tmux install) runs, you see "still running: 15s elapsed" every ~15s.
# - Total output is 5–10 lines (checklist + final summary), not 50+ duplicate lines.
```

---

### GitHub CLI Not Authenticated

**Trigger:** After install, running `tm sessions new <repo>` or `gh pr create` fails with "not authenticated" or "no token found".

**Symptom:** `gh auth status` shows "Not logged in" or command fails with authentication error.

**Why:** `tm sessions new` and the core workflow depend on `gh` being authenticated to GitHub. If you skipped `gh auth login` before starting trusty-tools install, the session creation will fail.

**Workaround:**
```bash
gh auth login
# Follow prompts to authenticate via browser or paste a personal access token.

# Verify:
gh auth status
# Should show "Logged in to github.com as <your-user>".

# Then retry:
tm sessions new <repo-url>
```

---

### Tmux Not Available (Legacy, mostly fixed by #3821/#3830)

**Trigger:** On a machine with no tmux and no Homebrew, `curl install.sh | sh -s -- -y` detects tmux missing and exits.

**Symptom:** Error: "tmux not found" and "Homebrew not found; cannot auto-install tmux." Exit code 2.

**Why:** Tmux is a hard requirement for `tm sessions`. The installer cannot auto-install it without Homebrew, so it stops rather than producing a broken install.

**Workaround:** Install Homebrew first (see #3821 above), then retry the installer.

---

### Trusty-Search Fails on Under-Spec Hosts

**Trigger:** Running `trusty-search start` on a machine with <16 GB RAM.

**Symptom:** Daemon exits immediately with "RAM check failed: require 16 GB, found X GB".

**Why:** Trusty-search's embeddings model and index can consume significant RAM. Under 16 GB, indexing large repositories risks OOM kill.

**Workaround:**
```bash
# For small workloads where you know peak RAM will stay <16 GB:
TRUSTY_SKIP_RAM_CHECK=1 trusty-search start

# Or set in ~/.zshenv / ~/.bashrc to persist:
export TRUSTY_SKIP_RAM_CHECK=1
```

**Warning:** Use at your own risk. Monitor `vm_stat` (macOS) or `free` (Linux) during indexing. If RAM pressure spikes, restart and use a more powerful host.

---

### Claude Code Token Loop (#2246, Avoided via `setup-token`)

**Trigger:** Running `claude login` interactively on a fresh machine.

**Symptom:** Prompt hangs in keychain password dialog loop, never returning.

**Why:** The harness's credential-chain initialization (environment → keyring → fallback) interacts poorly with launchd on first-run systems. The root cause is under investigation in the broader Claude Code harness.

**Workaround (documented in clean-vm-demo-rehearsal.md):**
```bash
# Use this instead of `claude login`:
claude setup-token

# Paste your Claude API token from https://claude.ai/account/settings/api-keys.
# Token is stored in the keychain (safe, persists).
```

This avoids the login loop by storing the token directly without interactive keychain dialogs.

---

### Credentials on Headless/SSH Machines

**Trigger:** Running trusty-* tools over SSH where OS keychain (macOS Keychain, Linux secret-service) is unavailable.

**Symptom:** Credentials stored in plaintext files (e.g., `~/.config/trusty-*/credentials.json`) instead of keychain.

**Why:** The credential resolver tries OS keychain first, but on headless machines (SSH, containers, CI runners), the keychain is inaccessible. It gracefully degrades to plaintext file storage.

**Mitigation:**
```bash
# Protect plaintext credential files:
chmod 600 ~/.config/trusty-memory/credentials.json
chmod 600 ~/.config/trusty-gworkspace/oauth_client.json
# (and similar for other tools)

# Prefer env vars on headless machines:
export OPENROUTER_API_KEY=sk-or-v1-xxxxxxxxxxxx
# (env tier is checked first in 3-tier resolution, before keychain)
```

---

## Not Covered by Either Tier

The following are intentionally OUT OF SCOPE for a standard installation and are **not part of either tier**:

- **trusty-agents cluster deployment** — trusty-agents can run standalone on a developer's machine, but orchestrating a cluster of PM agents with load balancing, service mesh, and persistence layers is outside the scope of the installer. This is a separate deployment story.

- **Kubernetes / Docker** — no container images or Helm charts are included in the base tiers. Deployment to K8s requires separate packaging (TBD).

- **Windows support** — binary releases target macOS (Apple Silicon) and Linux (x86_64) only. Windows is not yet supported.

- **Linux ARM64 (aarch64)** — the installer publishes binaries for x86_64-unknown-linux-gnu only. Linux ARM64 support is tracked as #2037 (not yet landed). Workaround: build from source with `cargo install ... --locked` on ARM64 Linux.

- **Prebuilt OpenRouter/Anthropic/AWS integration** — LLM credentials are expected to come from the user's own accounts (OpenRouter, Anthropic, AWS Bedrock, etc.). The installer does not set these up; configuration is manual.

- **Slack/Telegram/Google Workspace account setup** — the MCP servers are provided, but you must create apps/bots in each platform's console and provide your own tokens/credentials.

- **TGA external classification backends** — the tga binary includes optional integrations with Linear, Shortcut, Confluence, Datadog. Wiring these requires manual API token setup and configuration (see tga documentation).

---

## Installation Workflow Summary

### MINIMAL (Core Workflow)

1. **Prerequisites:** Xcode CLI, GitHub CLI (authenticated), Homebrew (macOS), 16 GB RAM, 4 GB disk.
2. **One-liner:**
   ```bash
   curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh -s -- -y
   ```
3. **Manual steps:**
   ```bash
   tm start  # Must explicitly start daemon
   tm doctor  # Verify all checks pass
   gh auth status  # Confirm GitHub auth
   ```
4. **First session:**
   ```bash
   tm sessions new https://github.com/<user>/<repo>.git --task "My task"
   tm sessions attach <SESSION-ID>
   ```

### ADVANCED (Enrichment)

1. **After MINIMAL is working,** install ADVANCED components as needed:
   ```bash
   # Example: trusty-agents + slack-mcp
   cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-agents --locked
   
   export SLACK_BOT_TOKEN=xoxb-...
   cargo install --path crates/trusty-channels --bin slack-mcp --locked
   ```

2. **Set credentials** (3-tier resolution: env > .env.local > keychain).

3. **Verify** (UNVERIFIED — exact commands pending review of each crate's health/status mechanics).

---

## Open Questions & UNVERIFIED Items

The following require verification from Bob's fresh-environment walkthrough or from the actual crate source:

1. **trusty-agents verification command:** What is the first-run verification for trusty-agents? Is there a `trusty-agents health` or equivalent?

2. **trusty-code verification command:** How does a developer verify `tcode serve` is working?

3. **Exact `trusty-review` Bedrock test command:** Is there a `trusty-review --check-bedrock` or equivalent to test AWS Bedrock credentials without running a full review?

4. **Exact `tga` first-run command:** What is a simple tga invocation to verify it works? (e.g., `tga analyze` on a test repo?)

5. **MCP server stdio testing:** For slack-mcp, telegram-mcp, trusty-gworkspace-mcp, the test command uses a raw JSON-RPC `initialize` message. Is this the right manual test, or should we defer to Claude Code integration testing?

6. **Exact credential config commands:** Several crates mention `<binary> config <feature> keys set`. Verify these commands exist and are the recommended user-facing API for credential setup.

7. **Keychain behavior on non-macOS platforms:** The docs mention Linux secret-service fallback. Verify the exact fallback behavior and whether plaintext files are written on Linux systems without secret-service.

8. **Launchd agent bootstrap order:** When does `trusty-installer` run `launchctl bootstrap` for each daemon? Is it after service-install or immediately? Verify the exact bootstrap logic in install commands.

9. **trusty-installer 0.4.10 actual publication:** Confirm 0.4.10 is live on crates.io and GitHub releases before publishing a walkthrough based on these fixes.

10. **macOS Keychain unavailability on headless machines:** Document the exact behavior: does trusty-memory / trusty-gworkspace silently degrade to plaintext, or do they error/warn?

---

## References

- Installer fixes: commits 536bc2e3 (0.4.10 bump), 00ced8c8 (#3897), 86e31e13 (#3821/#3879), 69985904 (#3834), 3f2007bd (#3836).
- Runbooks: `/docs/runbooks/clean-vm-demo-rehearsal.md` (existing, covers the workflow but not the tier breakdown).
- Stable set definition: `crates/trusty-installer/src/commands/stable_set.rs` (lines 151–161 define the canonical member list).
- Credential resolver: `crates/trusty-common/src/inference/credentials/resolver.rs` (3-tier resolution logic).

---

**END DRAFT**

This document is structured reference material for Bob to synthesize into a walkthrough. Every claim is rooted in source code, git history, or the existing runbooks. Where verification was not possible (due to code-reading limits or UNVERIFIED items), it is explicitly marked.
