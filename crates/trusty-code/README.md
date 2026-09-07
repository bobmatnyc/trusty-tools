# trusty-code

**Harness role:** The **Coding Harness** — per-project, Claude-Code-compatible
MPM orchestration. See
[docs/architecture/harnesses.md](../../docs/architecture/harnesses.md) for the
full three-harness architecture and delegation graph.

Why: Each project needs a harness that is already wired to its own `.claude/`
configuration — agents, skills, MCP connections, `CLAUDE.md`, and permissions.
`trusty-code` fills that role. It is the Claude-Code-native orchestration entry
point that runs the PM main-loop, enforces the mandatory workflow (research →
plan → implement → verify), and delegates authority to typed coding sub-agents
according to MPM protocols.

What: Per-project coding orchestration harness. One `tcode serve` process per
`.claude/` project root. Accepts task requests from CLI clients, TUI frontends,
and MCP callers. An original coding harness, tracked under epic #2052.

## Installation

### From GitHub Releases (recommended for binary users)

Prebuilt binaries are available for macOS (Apple Silicon) and Linux (x86_64).

1. Open [GitHub Releases](https://github.com/bobmatnyc/trusty-tools/releases),
   choose the newest `trusty-code-v<version>` release, and download the
   archive for your platform.

2. Extract and install:
   ```bash
   tar xzf trusty-code-*.tar.gz
   chmod +x tcode
   sudo mv tcode /usr/local/bin/    # or ~/.local/bin/ if you prefer user install
   ```

3. Verify the installation:
   ```bash
   tcode --version
   ```

### From Source with Cargo

Requires Rust 1.94 or later ([install Rust](https://rustup.rs/)).

```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools trusty-code --locked
```

This builds from the latest commit on `main` and installs the binary to `~/.cargo/bin/`. Make sure `~/.cargo/bin/` is on your PATH.

To install a specific release, replace `<version>` with its tag version:
```bash
cargo install --git https://github.com/bobmatnyc/trusty-tools --tag trusty-code-vx.y.z trusty-code --locked
```

### With Homebrew (planned — not yet available)

```bash
brew tap bobmatnyc/trusty
brew install trusty-code
```

This installation method is under development. For now, use GitHub Releases or `cargo install`.

Once available, this will provide:
- Automatic updates via `brew upgrade trusty-code`
- Standard macOS / Linux PATH integration
- Optional dependency resolution (e.g., system libraries for ONNX Runtime)

### Prerequisites & Special Cases

#### Prerequisites

- **Claude Code** (optional but recommended): this is a per-project orchestration harness that integrates with Claude Code's internal agent APIs. Standalone usage is not yet documented.
- **Git**: standard; the tool reads git metadata for branch context.

Configuration and usage details are documented in the `trusty-code` crate README.

### Verify Installation

All installations can be verified by running:

```bash
tcode --version
```

The output includes the installed semantic version and build provenance.

## Status

`tcode` is an active per-project orchestration harness. Its daemon supports
stdio and HTTP JSON-RPC transports; the CLI drives task, session, transcript,
workstream, and TUI flows. `run-workflow` remains the incomplete surface.

## Binaries

| Binary | Description |
|--------|-------------|
| `tcode` | Per-project Claude-Code-compatible MPM orchestration harness |

## Primary subcommands

| Subcommand | Description |
|------------|-------------|
| `tcode serve [--project <PATH>] --stdio\|--http` | Start a project-bound or projectless orchestration server |
| `tcode tui [--project <PATH>]` | Launch the interactive TUI and attach to or start an HTTP daemon |
| `tcode run-task <agent> <task>` | Run a task through a daemon-owned session |
| `tcode session …` | List, inspect, and create sessions |
| `tcode attach`, `cancel`, `transcript` | Operate on an existing session |
| `tcode workstream …` | Manage workstreams and their active state |
| `tcode paths show\|import` | Report which config root wins; import a `.claude/` catalog |
| `tcode run-workflow <name>` | Reserved workflow runner; not yet implemented |

## Configuration layout (`.trusty-code`)

Trusty Code owns two directories. It READS from three, and WRITES to only one
of them.

### Project configuration — `<project>/.trusty-code/`

Agents, skills, plugins, `settings.json`, and `CLAUDE.md` are looked up in this
order, highest precedence first:

| # | Directory | Role |
|---|---|---|
| 1 | `<project>/.trusty-code/<entry>` | Trusty Code's own — read AND written |
| 2 | `<project>/.claude/<entry>` | Claude Code compatibility input — read only |
| 3 | `<project>/.open-mpm/<entry>` | Pre-Claude-Code legacy input — read only |

The first candidate that exists **and can be opened** wins. When none exists,
the resolved path is `<project>/.trusty-code/<entry>` anyway, so a clean project
with no `.claude/` directory installs and runs normally.

**Fallback is never silent.** A candidate that exists but cannot be read
(permissions, a broken mount) is skipped rather than fatal — a harness that
refuses to start over an unreadable optional config is worse than one that
starts with less config — but it logs at `warn` to stderr naming the path tried,
and `tcode paths show` lists it. A `settings.json` that resolves but then fails
to read or parse falls through to the next harness-mode tier with the same
warning.

**Writes go to `.trusty-code/` only.** `.claude/` and `.open-mpm/` are inputs.
A write aimed outside `<project>/.trusty-code/` — including one that reaches
outside through a symlink — is refused, not redirected.

Run `tcode paths show [--json]` to see which root won for each entry.

### Private state — `~/.trusty-code/`

Transcripts, logs, compression telemetry, daemon discovery files, and the
workstream store are per-user runtime state, not project configuration. They
live in `~/.trusty-code/`, which is created at mode `0700`; an existing
directory with a permissive mode is tightened on the next run. `tcode paths
show` reports whether the mode is currently owner-only.

### Importing an existing `.claude/` catalog

```bash
tcode paths import --dry-run    # print the plan, write nothing
tcode paths import              # apply exactly that plan
```

The import copies `.claude/agents/**`, `.claude/skills/**`, and
`.claude/settings.json` to the same relative paths under `.trusty-code/`. It is
deterministic (the plan is a function of the tree, sorted by target), so the dry
run and the real run cannot disagree, and it is reversible — the report lists
exactly the files it created and nothing else was touched.

Four sources are refused rather than copied, each named in the output:

- the target already exists — an import never overwrites a user-authored file;
- the source is, or reaches through, a symlink out of `.claude/`;
- the source carries the executable bit;
- `settings.json` carries a secret-bearing key, or will not parse. Put
  credentials in the environment or the secure store, never in a file the
  project commits.

Plugins are deliberately not copied: their provenance cannot be vouched for, so
`.claude/plugins/` stays discoverable in place through the compatibility root.

**What counts as a secret-bearing key.** The key is split into words on
separators and camelCase boundaries, then matched word-exactly against `token`,
`secret`, `password`, `passphrase`, `credential`, `authorization`, `apikey`,
`accesskey` and `privatekey`. One entry covers every spelling: `api_key`,
`API-KEY`, `x-api-key`, `xApiKey` and `APIKEY` all match. Matching on words
rather than substrings is what keeps ordinary keys like `tokenizer` and
`max_tokens` from being refused.

**Limitation — keys, not values.** The scan reads key NAMES only. A credential
stored under an unrelated key (`"endpoint": "https://user:pw@host"`) is not
detected, and this check is not a substitute for a secret scanner on the
repository itself.

## Build

```bash
cargo build -p trusty-code
cargo run -p trusty-code -- --version
cargo test -p trusty-code --no-fail-fast
```

## Design Constraints

- **Owns its own layout** — project configuration under
  `<project>/.trusty-code/`, private mutable state under `~/.trusty-code/`
  (mode `0700`). See [Configuration layout](#configuration-layout-trusty-code).
- **Claude-Code compatible** — still reads `.claude/` config, agents, skills,
  MCP descriptors, `CLAUDE.md`, and permission grants exactly as Claude Code
  does, as a fallback behind its own directory. Never writes there.
- **Per-agent model routing** — each agent may specify its own model
  (AWS Bedrock or OpenRouter).
- **Single-instance per project** — one `tcode serve` process per `.claude/`
  root.
- **Event-driven** — publishes daemon-owned session and task events to attached
  clients.

## Architecture Role

trusty-code is the bottom layer of the three-harness stack:

```
trusty-agents (general agentic)  →  delegates coding tasks to trusty-code
trusty-mpm (meta-harness)        →  launches and oversees trusty-code sessions
trusty-code (coding harness)     →  executes per-project coding workflows
```

See [docs/architecture/harnesses.md](../../docs/architecture/harnesses.md) and
[ADR-0004](../../docs/adr/0004-three-harnesses-shared-event-driven-common.md).
