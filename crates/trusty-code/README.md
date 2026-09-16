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
| `tcode run-task <agent> <task>` | Run a task through a daemon-owned session. `--pm-model <SLUG>` pins the top-level agent's model, `--engineer-model <SLUG>` the delegated engineer's, and `--max-turns <N>` the top-level loop's turn cap |
| `tcode session …` | List, inspect, and create sessions |
| `tcode attach`, `cancel`, `transcript` | Operate on an existing session |
| `tcode workstream …` | Manage workstreams and their active state |
| `tcode paths show\|import` | Report which config root wins; import a `.claude/` catalog |
| `tcode run-workflow <name>` | Reserved workflow runner; not yet implemented |

## Credential storage

`tcode` stores inference-provider API keys through `trusty-common`'s shared
credential resolver. `trusty-code/Cargo.toml`'s `trusty-common` dependency
does not enable the `keyring-store` feature, so the OS-keychain branch of
`default_store()` never compiles in; every run falls straight to
`FileKeyStore` — a `0600`-permission plaintext TOML file at
`~/.trusty-tools/credentials.toml`, shared with every other trusty-* binary.
A key an operator stored in the OS keychain through a different tool is
invisible to `tcode`.

There is no `tcode doctor` subcommand. Diagnose a stored key with `tcode
config keys test <provider>`, and confirm which config root `tcode` resolved
with `tcode paths show`.

(Whether the keychain should be required repo-wide is a separate, currently
blocked decision: epic #4570.)

## Model selection and the OpenRouter ZDR guardrail (#7955)

`tcode`'s built-in default model (`provider::DEFAULT_MODEL`, currently
`anthropic/claude-sonnet-5`) is an OpenRouter slug served under
zero-data-retention (ZDR). An OpenRouter account with the ZDR guardrail
enabled rejects any model whose routing excludes ZDR-compliant endpoints with
`404 zdr-violation-by-guardrail` — the older default, `openai/gpt-4o-mini`,
hit this on every ZDR account.

### Per-run overrides

A `run-task` run has two agents, each with its own model, and both flags work
on the default (daemon) path and under `--legacy-in-process`. Short aliases
`opus` / `sonnet` / `haiku` are accepted anywhere a slug is, and resolve to
`anthropic/claude-opus-5`, `anthropic/claude-sonnet-5`, and
`anthropic/claude-haiku-4.5`.

| Flag | Env fallback | What it pins | Precedence |
|------|--------------|--------------|------------|
| `--pm-model <SLUG>` (#8030) | `TCODE_PM_MODEL` | The TOP-LEVEL agent's own model | flag > env > the agent's front-matter `model:` > `DEFAULT_MODEL` |
| `--engineer-model <SLUG>` (#1035) | `TCODE_ENGINEER_MODEL` | The delegated `python-engineer`'s model | flag > env > that agent's own config |
| `--max-turns <N>` (#8128) | `TCODE_MAX_TURNS` | The top-level loop's turn cap | flag > env > the built-in cap of 8 |

`--max-turns 0` is rejected: a zero-turn loop makes no LLM call and would
report an empty run as a normal one. An unparseable `TCODE_MAX_TURNS` is
treated as unset, logged at `warn`, matching `TCODE_RUN_DEADLINE_SECONDS`.

If a chat call still 404s on your chosen model (a ZDR exclusion, an unknown
slug, or a retired one), `tcode` surfaces an actionable error naming the
model and the two remedies, rather than a bare HTTP status:

- Pick a different model with `run-task --pm-model <slug>` /
  `--engineer-model <slug>` (see "Per-run overrides" above) or the agent's
  `model` / `[llm].model_override` config.
- Confirm your OpenRouter credentials are set with `tcode config keys list`.

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
directory with a permissive mode is tightened on the next run. Every `tcode`
run that resolves the directory applies both — not only `tcode paths import`
(#6999). `tcode paths show` reports whether the mode is currently owner-only,
and resolves the path without creating it.

Files and subdirectories already inside `~/.trusty-code/` keep their own modes;
only the root is chmod'd. `0700` on the root blocks the ordinary traversal path
into them.

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

**Exit code.** A clean import exits `0`. If any entry was refused, the command
exits `1` (#6999) — so a script can branch on it instead of parsing the plan.
`--dry-run` reports the code the real run would, which keeps the preview and the
run in agreement. Re-running a completed import therefore exits `1`: every
target already exists, so every entry is refused.

Plugins are deliberately not copied: their provenance cannot be vouched for, so
`.claude/plugins/` stays discoverable in place through the compatibility root.

#### How an imported agent's frontmatter maps onto `tcode`

An agent `.md` written for Claude Code — trusty-mpm's whole catalog, and most
of what a user already has in `.claude/agents/` — uses a frontmatter dialect
`tcode` reads directly. The mapping:

| Frontmatter key | Becomes |
|---|---|
| `name:`, `role:`, `description:` | the agent's identity, as declared |
| `model:` | the agent's model, normalised to a full slug |
| `max_tokens:` | the LLM output budget |
| `extends:` | resolved against the sibling `BASE-*.md` files in the same directory |
| `skills:` | the agent's declared skills, reported by `agents.describe` |
| `tcode_tools:` | the tool allowlist, verbatim — `tcode`'s own vocabulary |
| `tools:` | translated from Claude Code's vocabulary into the allowlist, when `tcode_tools:` is absent |
| anything else | ignored, not an error |

`tools:` and `tcode_tools:` name different vocabularies, so they cannot be read
as one list. `tcode_tools:` always wins when both are present. When only
`tools:` is present, each Claude Code name maps onto the `tcode` tools serving
the same capability:

| Claude Code | `tcode` |
|---|---|
| `Read` | `read_file`, `list_dir` |
| `Write` | `write_file`, `write_files` |
| `Edit` | `edit` |
| `Grep` / `Glob` | `grep` / `glob` |
| `Bash`, `BashOutput`, `KillShell` | `bash` |
| `Skill` | `use_skill` |
| `Task` | `delegate_to_agent` |
| `mcp__trusty-search` | `search_code` |

A name with no `tcode` equivalent (`WebFetch`, `WebSearch`, every other
`mcp__*` server) is dropped rather than refused — a Claude Code catalog names
tools this runtime does not host, and rejecting the document over one of them
would make the import unusable. `finish_task` is always granted: it has no
Claude Code spelling, and without it an agent cannot return a result.

Two consequences worth knowing before you import:

- **Bring the `BASE-*.md` templates.** An agent declaring `extends: base-ops`
  composes against the sibling files in the same directory. Import the whole
  `.claude/agents/` tree, not a hand-picked pair of files.
- **An imported agent is narrower than an embedded one.** The embedded roster
  ignores `tools:` entirely (it is composed in-memory, not loaded from disk),
  so a roster agent declaring no `tcode_tools:` may call anything. The same
  file imported to disk is gated to its translated grant. That is the intended
  direction: the author wrote a grant, and on disk it is enforced.

**What counts as a secret-bearing key.** The key is split into words on
separators and camelCase boundaries, each word is de-pluralised, then matched
word-exactly against `token`, `secret`, `password`, `passphrase`, `credential`,
`authorization`, `apikey`, `accesskey` and `privatekey`. One entry covers every
spelling: `api_key`, `API-KEY`, `x-api-key`, `xApiKey`, `APIKEY` and `apiKeys`
all match. Matching on words rather than substrings keeps `tokenizer` and
`secretary` from being refused.

A short exemption list covers whole keys that name a COUNT rather than a
credential — `max_tokens`, `min_tokens`, `token_count`, `token_limit` and
friends. Without it, de-pluralising `tokens` would refuse the LLM sampling
parameters this crate's own settings carry. The exemption matches the entire key
only, so `max_tokens_api_key` is still refused.

**Limitation — keys, not values.** The scan reads key NAMES only. A credential
stored under an unrelated key (`"endpoint": "https://user:pw@host"`) is not
detected, and this check is not a substitute for a secret scanner on the
repository itself.

## Agent inspection — `agents.describe`

`agents.list` answers "what could I dispatch": one row per agent with its name,
tier, description, model, and a `has_warnings` flag. `agents.describe` answers
the question after it — which definition actually runs under a name, and whether
anything about it needs attention.

The embedded roster covers a full delivery workflow. Besides `pm`, `engineer`
and the review/QA agents, it lists four workflow specialists the PM delegates
to in turn: `ticketing` (search, file, label and transition GitHub issues via
`gh`), `version-control` (branch, commit, push, open the PR via `git` and
`gh`), `local-ops` (build, test, lint, version bump, changelog) and
`documentation` (README and reference prose). `ticketing` and `version-control`
report their artifact URL on an `ISSUE:` / `PR:` line so the PM can carry it
into the next brief.

```json
{"method": "agents.describe", "params": {"name": "engineer"}}
```

```json
{
  "name": "engineer",
  "tier": "project",
  "path": "/w/.trusty-code/agents/engineer.md",
  "role": "engineer",
  "description": "General-purpose software engineer.",
  "model": "sonnet",
  "tools": {"allowed": null},
  "skills": ["toolchains-rust-core"],
  "instructions": {"length_bytes": 18422},
  "provenance": {
    "manifest": "present",
    "origin": "bundled",
    "framework_owned": true,
    "checksum": "match",
    "deployed_at": "2026-09-07T11:04:19Z",
    "source_chain": ["base-agent", "base-engineer", "engineer"]
  },
  "warnings": []
}
```

- **`tier`** — `embedded`, `project`, `user`, `plugin`, or `broken`. The name is
  resolved through the same disk-wins/embedded-fallback chain dispatch uses, so
  inspection and dispatch can never disagree about which definition wins.
- **`tools.allowed`** — the agent's tool allowlist. `null` means every
  registered tool is permitted, matching how the runtime reads it.
- **`instructions`** — the resolved prompt's length in bytes. Pass
  `"include_instructions": true` to get the text itself in
  `instructions.text`; a composed roster prompt runs to tens of kilobytes, so it
  is opt-in.
- **`provenance`** — read back from the deployed-agent manifest beside the file.
  `manifest` is `present`, `absent`, `corrupt`, or `not-applicable` (the agent
  has no file on disk). `origin` and `framework_owned` say whether Trusty Code
  wrote the file; `checksum` says whether it still matches what was written.
- **`warnings`** — everything an operator would otherwise have to diff for: a
  disk copy that diverges from the bundled roster, a file the manifest does not
  track, a missing ledger in `.trusty-code/agents/`, an unreadable one, or a
  parse failure. `agents.list`'s `has_warnings` is `true` for exactly the rows
  that would return a non-empty list here.

**A broken file is described, not raised.** An agent whose `.md` exists but
fails to parse or compose comes back with `tier: "broken"` and the parse error
in `warnings`, because the reason a name fell back to the embedded roster is
what the operator is looking for. A name that resolves nowhere is a `not_found`
error listing the agents that do exist.

**Capability grants are not reported, because they do not exist yet.** An
agent's declared authority in Trusty Code today is the flat `tools.allowed`
allowlist and nothing else. The write, shell, network, credential, and
delegation grants #2074 calls for are a later slice; until they are enforced,
this payload carries no `grants` key rather than a field that would imply the
runtime checks something it does not.

There is no `tcode agents` CLI family — this surface is JSON-RPC only. Use
`tcode paths show` for the directory-level view of which root won and what the
ledger tracks.

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
