# Doc↔code consistency sweep — findings inventory (2026-08-07)

Detection pass for issue [#5136](https://github.com/bobmatnyc/trusty-tools/issues/5136).
Nothing here is fixed. Triage and per-finding tickets follow.

## What was swept

`docs/**` (excluding `research/`, `plans/`, `prd/`, `presentations/`, `design/`,
`reporting/`, `_archive/`, `sessions/` — point-in-time material preserved as-is),
every `crates/*/README.md`, the root `README.md`, and `CLAUDE.md` files. The 27
pages on `docs/public-manifest.tsv` were read in full; crate READMEs and
`docs/reference/` next; the rest by mechanical sweep.

Claim classes checked mechanically: `[[bin]]` names, clap subcommands and flags,
Rust types/traits/functions/modules, constants and env-var names, default ports,
license fields, crates.io publication state and versions, config/data paths,
`[features]`, counts, and internal cross-reference targets.

Method: source is authoritative; another doc is never evidence. Every symbol
claim was checked against the current tree (`grep` over `crates/*/src`, then the
declaring file). Publication state was checked against the crates.io API, the
`publish` field, and `git tag` / `gh release list` — `publish = false` and "no
publish field, never released" are recorded as distinct states.

## Totals

**53 findings identified. The 40 most significant are listed below**, sorted
HIGH → MEDIUM → LOW. The 13 dropped are all LOW: dangling `src/<mod>.rs` citations
where the module is now a directory of the same name (7), stale illustrative
version numbers in sample CLI output (3), and dangling links inside
`docs/trusty-analyze/regression-testing/` snapshot files (3). None would make a
reader run a failing command.

`ALREADY-IN-FLIGHT` rows are recorded so triage does not double-file; the owning
PR is named.

---

## HIGH

A published page, a doc that would make someone run a command that fails or is
unsafe, or a test pinning a wrong value.

### H1 — a test pins the wrong MCP tool name

- **File / symbol:** `crates/trusty-search/src/commands/integrate.rs` ::
  `CURSOR_RULES` (the rules template), `write_cursor_rules`, and the test
  `test_write_rules_creates_file`
- **Claim:** the Cursor rules file `trusty-search` writes into a user's project
  tells the agent to call an MCP tool named `search_code` — four times, including
  `search_code "fn <name>"` usage examples.
- **Source:** no `search_code` tool exists. The stdio dispatcher matches
  `search`, `search_lexical`, `search_semantic`, `search_kg`, `search_all`,
  `search_similar` (`src/mcp/tools/search.rs`), plus the index and misc tools. A
  client following the generated rules gets a tool-not-found error.
- **Test pins the wrong value:** `test_write_rules_creates_file` asserts
  `body.contains("search_code")` with the message *"rules body should mention
  search_code"*. The suite protects the defect — same shape as the
  `__HARNESS_EVENT__` case.
- **Verified by:** `grep -rn '"search_code"' crates/trusty-search/src` returns
  only the test assertion; the tool-name match arms in `src/mcp/tools/*.rs`
  enumerate the real set.
- **Status:** new

### H2 — root README advertises the same non-existent tool

- **File / symbol:** `README.md` :: "Three Flagship MCP Servers" →
  trusty-search → **MCP tools:**
- **Claim:** `search_code`, `search_similar`, `index_file`, …
- **Source:** as H1 — the tool is `search`. The list also omits `search_kg`,
  `search_lexical`, `search_semantic`, `get_call_chain`, `grep`, `typeahead`,
  `upgrade`, `console_metrics`.
- **Verified by:** `grep -rhoE '"name"\s*:\s*"[a-z_]+"' crates/trusty-search/src/mcp/tools/*.rs`
- **Status:** new

### H3 — `trusty-agents-common` README documents a different crate's API

- **File / symbol:** `crates/trusty-agents-common/README.md` ::
  "Agent Execution", "Agent Trait", "Implementing an Agent", "Orchestrator"
- **Claim:** `pub struct AgentRequest`, `pub enum AgentResponse`,
  `pub struct AgentContext`, `pub struct Constraints`, `pub trait Agent`,
  `pub enum AgentError`, `pub struct Orchestrator`, all imported as
  `use trusty_agents_agent_api::{...}`.
- **Source:** `crates/trusty-agents-common/src/lib.rs` exports `ToolResult`,
  `ToolExecutionTier`, `ServiceTier`, `trait ToolExecutor`, `struct AgentPlugin`
  — which is what `Cargo.toml`'s own description says the crate is. Not one
  documented item exists. There is no crate named `trusty_agents_agent_api`
  anywhere in the workspace.
- **Verified by:** `grep -rn "pub (struct|enum|trait)" crates/trusty-agents-common/src/lib.rs`;
  `grep -rn "trusty_agents_agent_api" --include='*.rs' --include='*.toml' crates/` → empty
- **Status:** new — same defect class as the already-ticketed
  `AgentRequest`/`AgentResponse` SPEC finding, but this is the crate README,
  which that ticket does not cover.

### H4 — `trusty-agents-local` README documents a library that does not exist

- **File / symbol:** `crates/trusty-agents-local/README.md` :: `LocalExecutor`,
  `CommandExecutor`, `execute_command`, `pub enum Error`, and the env vars
  `TRUSTY_LOCAL_SANDBOX_ROOT`, `TRUSTY_LOCAL_ALLOWED_COMMANDS`,
  `TRUSTY_LOCAL_TIMEOUT_SECS`, `TRUSTY_LOCAL_MAX_CONCURRENT_TASKS`,
  `TRUSTY_LOCAL_MEMORY_LIMIT_MB`
- **Claim:** 280 lines of API documentation including
  `use trusty_agents_local::LocalExecutor;` and `LocalExecutor::new(Config::default())?`.
- **Source:** the crate is one file — `src/main.rs`, 17 lines — and its
  `Cargo.toml` describes it as "Private launcher: thin pass-through to
  `trusty-agents::run()`". There is no `lib.rs`, so `use trusty_agents_local::…`
  cannot compile at all. None of the five env vars is read anywhere in the repo.
- **Verified by:** `wc -l crates/trusty-agents-local/src/*` → `17 src/main.rs`;
  `grep -rl TRUSTY_LOCAL_ --exclude='*.md' .` → no hits
- **Status:** new

### H5 — `tc-services` README documents four traits that do not exist

- **File / symbol:** `crates/tc-services/README.md` :: `DirectoryService`,
  `RoleService`, `PreferenceService`, `SyncService`
- **Claim:** four `pub trait` blocks with 12 async methods
  (`find_person_by_email`, `org_chart`, `get_person_role`, `set_preference`,
  `sync_from_google_workspace`, …), plus `TRUSTY_CACHE_CAPACITY` /
  `TRUSTY_CACHE_TTL_SECS`.
- **Source:** `crates/tc-services/src/` has three modules exporting
  `GranolaService`, `CtoDbService`, `GworkspaceService` (each a struct with
  `new`/`name`/`schema`) plus `granola_services()`, `cto_db_services()`,
  `gworkspace_services()`. No trait, no named method, neither env var.
- **Verified by:** `grep -rnE "pub (struct|trait|fn|enum) " crates/tc-services/src`
- **Status:** new

### H6 — `trusty-cto-db` README names the wrong env var and a non-existent API

- **File / symbol:** `crates/trusty-cto-db/README.md` :: `Database`,
  `DatabaseConfig`, `pub enum DbError`, `TRUSTY_CTO_DB_PATH`
- **Claim:** `use trusty_cto_db::{Database, DatabaseConfig};` and
  "`TRUSTY_CTO_DB_PATH`: SQLite database file location".
- **Source:** `crates/trusty-cto-db/src/lib.rs` exports free functions only —
  `resolve_db_path`, `open_readonly`, `query_headcount`, `query_budget`,
  `query_risks`, `query_work_classification`, `tool_list_response`,
  `handle_tool_call`, `dispatch`. The env var is
  `pub const ENV_CTO_DB_PATH: &str = "CTO_DB_PATH"` — no `TRUSTY_` prefix, so a
  reader who exports `TRUSTY_CTO_DB_PATH` is silently ignored and the tool reads
  the hardcoded `$HOME/Duetto/cto/data/cto.db` instead.
- **Verified by:** `crates/trusty-cto-db/src/lib.rs::resolve_db_path`
- **Status:** new

### H7 — `trusty-cto-db` SPEC repeats the wrong env var and adds a wrong default

- **File / symbol:** `docs/trusty-cto-db/SPEC.md` :: environment section
- **Claim:** "`TRUSTY_CTO_DB_PATH`: SQLite database file location (default:
  `~/.trusty/cto.db`)"
- **Source:** env var is `CTO_DB_PATH`; the default is
  `$HOME/Duetto/cto/data/cto.db` (`resolve_db_path`). Both halves are wrong.
- **Verified by:** as H6
- **Status:** new

### H8 — `trusty-mpm-gui` README points at the wrong port and wrong env var

- **File / symbol:** `crates/trusty-mpm-gui/README.md` :: "The GUI discovers the
  trusty-mpm daemon via"
- **Claim:** "1. Default location: `http://localhost:7687`  2. Environment
  variable: `TRUSTY_MPM_DAEMON_URL`"
- **Source:** `crates/trusty-mpm-gui/src/state.rs` declares
  `pub const DEFAULT_DAEMON_URL: &str = "http://127.0.0.1:7880"` and reads
  `TRUSTY_MPM_URL`. `TRUSTY_MPM_DAEMON_URL` is read nowhere in the repo. 7687 is
  Neo4j's Bolt port — nothing in this workspace binds it.
- **Verified by:** `grep -rl TRUSTY_MPM_DAEMON_URL --exclude='*.md' .` → no hits
- **Status:** new

### H9 — `trusty-search` README documents a security bypass that does not exist

- **File / symbol:** `crates/trusty-search/README.md` :: "Operator bypass (for
  automation / CI)"
- **Claim:** "Set `TRUSTY_ALLOW_UNLISTED=1` in the daemon's environment to
  disable the allowlist check entirely", with a runnable
  `TRUSTY_ALLOW_UNLISTED=1 trusty-search start`.
- **Source:** no production code reads it. The only other occurrence in the
  tree is a comment inside `crates/trusty-search/src/allowlist/tests.rs`
  referring to "TRUSTY_ALLOW_UNLISTED logic in server.rs" — a file that carries
  no such logic. A documented security control that silently does nothing is
  worth a ticket even though it currently fails safe.
- **Verified by:** `grep -rl TRUSTY_ALLOW_UNLISTED --exclude='*.md' .` → only the
  test comment
- **Status:** new

### H10 — public page documents a `trusty-review report` flag that has no clap field

- **File / symbol:** `docs/trusty-analyze/README.md` :: "Role in Technical DD
  Report Generation" — **on the public manifest** (`/tools/trusty-analyze`)
- **Claim:** "When `trusty-review report --repo <path>` is invoked …"
- **Source:** `crates/trusty-review/src/cli_report.rs::ReportArgs` has fields
  `manifest`, `template`, `instructions`, `out`, `synthesize`,
  `investigate_max_files`, `investigate_max_bytes`, `corpus`, `corpus_add`,
  `benchmark`, `no_mermaid`, `analyze`. There is no `repo`; `--manifest <FILE>`
  is required and has no default, so the documented invocation fails twice.
- **Verified by:** full field list of `ReportArgs`
- **Status:** new — same shape as the already-ticketed `--config` case

### H11 — the same non-existent flag in the report guide

- **File / symbol:** `docs/trusty-review/reports/README.md` :: usage block
- **Claim:** `trusty-review report --repo <path> --template <name> [--out <dir>]`
- **Source:** as H10.
- **Verified by:** as H10
- **Status:** new

### H12 — public page names a crate that was deleted

- **File / symbol:** `docs/trusty-memory/README.md` :: opening line — **on the
  public manifest** (`/tools/trusty-memory`)
- **Claim:** "Memory palace storage daemon + MCP server frontend
  (`crates/trusty-memory-core` + `crates/trusty-memory`)."
- **Source:** `crates/trusty-memory-core/` does not exist; the engine lives
  behind `trusty-common`'s `memory-core` feature. This page's own
  `spec/ARCHITECTURE.md` states the crate "no longer exists" — the README
  contradicts the spec it links to.
- **Verified by:** `ls crates/` (27 entries, no `trusty-memory-core`)
- **Status:** new

### H13 — `trusty-code` README sends readers to release assets that 404

- **File / symbol:** `crates/trusty-code/README.md` :: "From GitHub Releases"
- **Claim:** "Look for assets tagged `trusty-code-v0.0.0`", then
  `trusty-code-v0.0.0-aarch64-apple-darwin.tar.gz` /
  `trusty-code-v0.0.0-x86_64-unknown-linux-gnu.tar.gz`, and
  `cargo install --git … --tag trusty-code-v0.0.0 trusty-code --locked`.
- **Source:** the only trusty-code tag/release is `trusty-code-v0.2.0`
  (crate version is now `0.3.0`). Every `v0.0.0` reference resolves to nothing —
  the download and the `--tag` install both fail.
- **Verified by:** `git tag -l 'trusty-code*'`; `gh release list`
- **Status:** new

### H14 — `cargo install trusty-git-analytics` installs nothing

- **File / symbol:** `docs/trusty-git-analytics/developer/migration-from-python.md`
  :: install section
- **Claim:** `cargo install trusty-git-analytics`
- **Source:** the package name is `tga` (`crates/trusty-git-analytics/Cargo.toml`
  `name = "tga"`, published at 2.11.0). `trusty-git-analytics` is not on
  crates.io at all, so the command errors out. `crates/trusty-git-analytics/README.md`
  and `docs/trusty-git-analytics/user/user-guide.md` correctly say
  `cargo install tga`.
- **Verified by:** crates.io API — `trusty-git-analytics` → not published;
  `tga` → 2.11.0
- **Status:** new

### H15 — `cargo install trusty-gworkspace` resolves to a 0.0.0 name placeholder

- **File / symbol:** `crates/trusty-gworkspace/README.md` :: install note
- **Claim:** "`cargo install --path crates/trusty-gworkspace` (or
  `cargo install trusty-gworkspace`) to get the …"
- **Source:** the local crate is at `0.2.2`; crates.io holds only `0.0.0`, a
  reserved-name placeholder published 2026-05-20. The crates.io path installs a
  stub, not the MCP server. `Cargo.toml` carries no `publish` field, so this is
  the "publishable but never released" state, not `publish = false`.
- **Verified by:** crates.io API `max_version` = `0.0.0` vs local `0.2.2`
- **Status:** new

### H16 — `--bin trusty-mpmd` does not exist

- **File / symbol:** `docs/reference/running-mcp-servers.md` :: "MPM daemon" —
  **on the public manifest** (`/reference/running-mcp-servers`)
- **Claim:** `RUST_LOG=info cargo run -p trusty-mpm --bin trusty-mpmd`
- **Source:** `crates/trusty-mpm/Cargo.toml` declares exactly two `[[bin]]`
  targets, `tm` and `trusty-mpm`. The command fails with "no bin target named
  `trusty-mpmd`". The correct form is `cargo run -p trusty-mpm -- daemon`, which
  `crates/trusty-mpm/README.md`'s own migration table gives.
- **Verified by:** `[[bin]]` blocks in `crates/trusty-mpm/Cargo.toml`
- **Status:** `ALREADY-IN-FLIGHT` — file owned by
  [PR #5107](https://github.com/bobmatnyc/trusty-tools/pull/5107)

### H17 — `trusty-search port` documented as 7879 in two places

- **File / symbol:** `docs/reference/running-mcp-servers.md` (public manifest)
  and `crates/trusty-search/README.md` :: "## CLI"
- **Claim:** `trusty-search port  # bare port: 7879`, `--addr` →
  `127.0.0.1:7879`, `--json` → `{"addr":"127.0.0.1","port":7879}` — three lines,
  duplicated verbatim across both files.
- **Source:** `crates/trusty-search/src/service/constants.rs` ::
  `pub const DEFAULT_PORT: u16 = 7878`. 7879 is trusty-analyze's port
  (`crates/trusty-analyze/src/service/events.rs::DEFAULT_PORT`), which
  `docs/trusty-analyze/README.md` states correctly. A reader who hardcodes 7879
  for search hits the analyze daemon.
- **Verified by:** both `DEFAULT_PORT` declarations
- **Status:** `docs/reference/running-mcp-servers.md` is `ALREADY-IN-FLIGHT`
  ([PR #5107](https://github.com/bobmatnyc/trusty-tools/pull/5107));
  `crates/trusty-search/README.md` is **new**

### H18 — public page lists retired binaries as current `[[bin]]` targets

- **File / symbol:** `docs/trusty-mpm/README.md` :: opening paragraph —
  **on the public manifest** (`/tools/trusty-mpm`)
- **Claim:** "one crate … with feature-gated `[[bin]]` targets: the CLI
  (`tm` / `trusty-mpm`), the daemon (`trusty-mpmd`), an in-session MCP server, a
  TUI (`trusty-mpm-tui`), and a Telegram bot (`trusty-mpm-telegram`)"
- **Source:** two `[[bin]]` targets, `tm` and `trusty-mpm`.
  `crates/trusty-mpm/README.md` documents the retirement explicitly:
  "The standalone shim binaries `trusty-mpmd`, `trusty-mpm-tui`,
  `trusty-mpm-telegram` … are now subcommands." The public page contradicts both
  source and the crate README.
- **Verified by:** `[[bin]]` blocks in `crates/trusty-mpm/Cargo.toml`
- **Status:** `ALREADY-IN-FLIGHT` —
  [PR #5107](https://github.com/bobmatnyc/trusty-tools/pull/5107)

### H19 — public page is titled for a crate that does not exist

- **File / symbol:** `docs/trusty-agents/README.md` :: title and body —
  **on the public manifest** (`/tools/trusty-agents`)
- **Claim:** "# open-mpm — documentation", "`open-mpm` (`crates/open-mpm/`) is a
  Rust-native AI agent orchestration harness", and it "consumes the shared
  trusty-* libraries (trusty-search, trusty-memory-core, trusty-symgraph)".
- **Source:** the crate is `crates/trusty-agents` (bin `tagent`, v0.38.6).
  `crates/open-mpm/`, `trusty-memory-core`, and `trusty-symgraph` are all gone —
  the latter two absorbed into `trusty-common` feature flags.
- **Verified by:** `ls crates/`; the consolidation note in
  `docs/reference/crate-map.md`
- **Status:** `ALREADY-IN-FLIGHT` —
  [PR #5107](https://github.com/bobmatnyc/trusty-tools/pull/5107)

### H20 — `cargo install open-mpm`

- **File / symbol:** `docs/trusty-agents/user/quickstart.md` :: install block —
  **on the public manifest** (`/guides/trusty-agents/quickstart`)
- **Claim:** `cargo install open-mpm`
- **Source:** the crate was renamed to `trusty-agents`, which is not on
  crates.io. `open-mpm` exists on crates.io only as a `0.0.0` reserved-name
  placeholder, so the command "succeeds" into a stub rather than erroring —
  worse than a clean failure.
- **Verified by:** crates.io API: `open-mpm` `max_version` `0.0.0`,
  created 2026-05-20; `trusty-agents` not published
- **Status:** `ALREADY-IN-FLIGHT` —
  [PR #5107](https://github.com/bobmatnyc/trusty-tools/pull/5107); this is one of
  the pre-existing calibration cases, recorded only so triage sees the crates.io
  placeholder nuance.

---

## MEDIUM

Wrong, but only misleads a contributor.

### M1 — "21 crates" is wrong in three files and self-contradicted in one

- **Files / symbols:** `README.md` (lead paragraph, "Full Crate Index (All 21
  Crates)", "Workspace Info"); `CLAUDE.md` (Project Overview);
  `docs/getting-started/claude-mpm-vs-trusty-mpm.md` ("Architecture & Design" →
  Root README bullet); `docs/reference/crate-map.md` (code fence says
  "20 members (matches `ls crates/`)", closing paragraph says "all 27 `crates/*`
  glob members")
- **Source:** `ls crates | wc -l` → **27**. The root README's own index table
  lists 20 entries and omits `trusty-channels`, `trusty-code-gui`,
  `trusty-embedderd-py`, `trusty-kb`, `trusty-publish-guard`, `trusty-sld-lint`,
  `trusty-tui`. `crate-map.md` disagrees with itself inside one file.
- **Status:** new

### M2 — root README architecture diagram is five crates out of date

- **File / symbol:** `README.md` :: "## Architecture" ASCII diagram
- **Claim:** boxes for `trusty-symgraph`, `trusty-embedder`, `trusty-mcp-core`,
  `trusty-rpc`, `trusty-tickets`.
- **Source:** none of the five exists. All were absorbed into `trusty-common`
  behind the `symgraph`, `embedder`, `mcp`, `rpc`, `tickets` features — a
  consolidation `docs/reference/crate-map.md` documents correctly.
- **Status:** new

### M3 — `crate-map.md` lists a crate that does not exist

- **File / symbol:** `docs/reference/crate-map.md` :: code-structure tree
- **Claim:** `├── cto-assistant/  # CTO assistant CLI (publish=false)`
- **Source:** `crates/cto-assistant` does not exist.
- **Status:** new

### M4 — `crate-map.md` conflates two publication states

- **File / symbol:** `docs/reference/crate-map.md` :: `trusty-agents`,
  `trusty-agents-common` rows
- **Claim:** both annotated `(publish=false)`.
- **Source:** neither `Cargo.toml` declares a `publish` field. `trusty-agents`
  is publishable but has never been released; **`trusty-agents-common` IS
  published — crates.io holds 0.4.0** while the tree is at 0.5.0. Labelling a
  live published crate `publish=false` is exactly the conflation the sweep was
  looking for. (`trusty-agents-local` and `trusty-mpm-gui` carry a real
  `publish = false` and are labelled correctly.)
- **Verified by:** crates.io API; `publish` field greps across `crates/*/Cargo.toml`
- **Status:** new

### M5 — "Each crate owns its own `README.md`"

- **File / symbol:** `docs/reference/crate-map.md` :: paragraph after the tree
- **Source:** five crates have none — `trusty-kb`, `trusty-progress`,
  `trusty-publish-guard`, `trusty-sld-lint`, `trusty-tui`.
- **Status:** new

### M6 — `trusty-search` README cites a file and function that do not exist

- **File / symbol:** `crates/trusty-search/README.md` :: "## MCP tools"
- **Claim:** "The MCP server registers **18 tools** (authoritative source:
  `src/mcp/tools.rs` `tool_definitions`)".
- **Source:** `src/mcp/tools.rs` is a directory, `src/mcp/tools/`, and there is
  no `tool_definitions` function anywhere in the crate. 21 tools are registered;
  the table omits `typeahead`, `upgrade`, `console_metrics`. (Contrast
  `crates/trusty-memory/README.md`, whose equivalent citation —
  `src/tools/definitions.rs` `tool_definitions` — is correct.)
- **Status:** new

### M7 — public page says there is no GUI

- **File / symbol:** `docs/getting-started/claude-mpm-vs-trusty-mpm.md` :: FAQ
  "Is there a GUI for trusty-mpm?" — **on the public manifest**
- **Claim:** "Not yet, but `trusty-console` (in development) will provide a web
  dashboard."
- **Source:** `crates/trusty-mpm-gui` is a Tauri desktop GUI at v0.2.12 with a
  `trusty-mpm-gui` binary, and `tm gui` is a shipped subcommand
  (`crates/trusty-mpm/src/bin/tm/cli/mod.rs` :: `Gui`). `trusty-console` is at
  0.5.0, published to crates.io and carrying a Homebrew formula — not "in
  development".
- **Status:** new

### M8 — public page names the wrong config path

- **File / symbol:** `docs/getting-started/claude-mpm-vs-trusty-mpm.md` ::
  differences table, "Configuration" row; repeated in the FAQ — **on the public
  manifest**
- **Claim:** "Unified `~/.trusty-tools/config/` + project overrides"
- **Source:** `crates/trusty-common/src/crate_config.rs` defines the convention
  as `~/.trusty-tools/<crate>/config.yaml` (issue #1220). There is no
  `~/.trusty-tools/config/` directory in the scheme.
- **Status:** new

### M9 — public uninstall instructions leave `tm` on PATH

- **File / symbol:** `docs/getting-started/install-and-run-tm.md` :: "Q: How do I
  uninstall?" — **on the public manifest**
- **Claim:** `rm ~/.local/bin/trusty-mpm ~/.local/bin/trusty-memory ~/.local/bin/trusty-search ~/.local/bin/tctl`
- **Source:** `crates/trusty-mpm/Cargo.toml` ships two binaries, `tm` and
  `trusty-mpm`, and this page's own "What You Just Installed" section says the
  primary name is `tm`. The uninstall omits it, so the tool the reader actually
  invokes survives.
- **Status:** new

### M10 — `trusty-code` Homebrew is documented as unavailable but shipped

- **File / symbol:** `crates/trusty-code/README.md` :: "With Homebrew (planned —
  not yet available)"
- **Claim:** "This installation method is under development."
- **Source:** `bobmatnyc/homebrew-trusty` carries `trusty-code.rb`, so
  `brew install trusty-code` works today.
- **Verified by:** `gh api repos/bobmatnyc/homebrew-trusty/contents/Formula`
- **Status:** new

### M11 — root README undercounts the Homebrew tap

- **File / symbol:** `README.md` :: "### With Homebrew"
- **Claim:** "Then install any of the six published binaries", listing search,
  memory, analyze, review, mpm, git-analytics.
- **Source:** the tap has ten formulae — the six above plus `trusty-code`,
  `trusty-console`, `trusty-controller`, `trusty-installer`.
- **Status:** new

### M12 — `trusty-embedderd` is described as a fastembed wrapper

- **Files / symbols:** `README.md` (Shared Libraries table);
  `docs/reference/crate-map.md` (tree + per-crate list);
  `docs/trusty-embedderd/README.md` ("Index of documentation for the FastEmbed
  sidecar daemon") — the last is **on the public manifest**
  (`/tools/trusty-embedderd`)
- **Claim:** "fastembed wrapper" / "FastEmbed sidecar daemon".
- **Source:** `crates/trusty-embedderd/Cargo.toml` declares no `fastembed`
  dependency; it pulls `trusty-common` with the `embedder`/`embedder-client`
  features and its own description reads "Unified ONNX embedding daemon with
  HTTP + UDS transports and BatchQueue".
- **Status:** new

### M13 — root README states the wrong embedding model variant

- **File / symbol:** `README.md` :: Shared Libraries table, `trusty-embedderd` row
- **Claim:** "all-MiniLM-L6-v2 **INT8 quantised**, 384-dim output"
- **Source:** `crates/trusty-common/src/embedder/fast_embedder.rs` ::
  `resolve_default_embedding_model` returns `EmbeddingModel::AllMiniLML6V2`
  (fp32) unless `TRUSTY_EMBEDDER_MODEL=int8|quantized|q` is set. The default
  flipped away from INT8 in #3486/#3493, and #3530 was filed against exactly this
  stale-name class in the daemon's own `/health` output.
- **Status:** new

### M14 — root README mis-describes `trusty-console`

- **File / symbol:** `README.md` :: Supporting Tools table
- **Claim:** "`trusty-console` | Terminal UI for system monitoring"
- **Source:** `crates/trusty-console/Cargo.toml` — "Web console that detects and
  surfaces running trusty services as a home page with service cards"; it binds
  `DEFAULT_PORT = 7788` over axum and proxies `/api/mpm` to the trusty-mpm
  daemon. It is not a terminal UI.
- **Status:** new

### M15 — root README mis-describes `trusty-code`

- **File / symbol:** `README.md` :: Supporting Tools table
- **Claim:** "`trusty-code` | Code generation and analysis utilities"
- **Source:** `crates/trusty-code/Cargo.toml` — "Per-project
  Claude-Code-compatible MPM orchestration harness (bin: `tcode`)". It is a
  coding harness, not a utility library, and the row does not name its binary.
- **Status:** new

### M16 — `CLAUDE.md` edition count is off by one

- **File / symbol:** `CLAUDE.md` :: "Common Pitfalls" → "Edition mismatch"
- **Claim:** "11 crates pin `edition = "2021"` explicitly"
- **Source:** `grep -l '^edition = "2021"' crates/*/Cargo.toml | wc -l` → **10**.
- **Status:** new

### M17 — `trusty-bm25-daemon` README cites a crate that never existed

- **File / symbol:** `crates/trusty-bm25-daemon/README.md` :: "## Why" and the
  references list
- **Claim:** "matches the architecture of the sibling `trusty-embed-daemon`
  (PR #157)" and "`crates/trusty-embed-daemon/` — the sibling embed subprocess
  this design is modelled on."
- **Source:** no `trusty-embed-daemon` directory exists, and no crate by that
  name is in the workspace or on crates.io. The sibling is `trusty-embedderd`.
- **Status:** new

### M18 — a spec asserts a CI gate that does not exist

- **File / symbol:** `docs/specs/SPEC-PMINSTR-01-p1-p2-instruction-restructure.md`
  :: §"claude-md-guard"
- **Claim:** "runs `scripts/check_claude_md_not_tracked.sh` on every push/PR" and
  "Do not loosen `claude-md-guard.yml`".
- **Source:** neither `scripts/check_claude_md_not_tracked.sh` nor
  `.github/workflows/claude-md-guard.yml` exists. The 15 workflows present are
  agent-assets, al2023-build, capabilities-drift, cargo-audit,
  changelog-fragment, ci, doc-numbers, e2e-docker, generation-artifact-lint,
  line-cap, release, sld-lint, test-pointers, token-drift, version-parity.
- **Status:** new

### M19 — `docs/specs/README.md` link resolves into a nonexistent nested path

- **File / symbol:** `docs/specs/README.md` :: catalog row for
  `intent-conformance`
- **Claim:** link target `docs/specs/intent-conformance.md`
- **Source:** the link is repo-relative but rendered relative to `docs/specs/`,
  so it resolves to `docs/specs/docs/specs/intent-conformance.md`. The file
  exists at `docs/specs/intent-conformance.md`; the link needs to be bare.
  `docs/specs/spec-linked-documentation.md` has two links with the same defect,
  and `docs/specs/intent-conformance.md` carries a literal `docs/specs/{file}.md`
  placeholder link.
- **Verified by:** relative-link resolution over all non-archival docs (18 broken
  targets total)
- **Status:** new

### M20 — `harnesses.md` cites eleven paths under a deleted crate

- **File / symbol:** `docs/architecture/harnesses.md` :: throughout —
  **on the public manifest** (`/reference/harnesses`)
- **Claim:** `crates/open-mpm/`, `crates/open-mpm/src/ctrl/`,
  `.../src/intent/`, `.../src/tools/mcp_service_tools.rs`, `.../src/events.rs`,
  `.../src/workflow/`, `.../src/bus/mod.rs`, `crates/open-mpm-agent-api/`,
  `crates/open-mpm/README.md`, plus a `HarnessKind` type and
  `crates/trusty-mpm/src/core/session_launch.rs`.
- **Source:** `crates/open-mpm` and `crates/open-mpm-agent-api` do not exist
  (the crate is `trusty-agents`); `session_launch.rs` and `HarnessKind` are not
  in the tree.
- **Status:** `ALREADY-IN-FLIGHT` —
  [PR #5128](https://github.com/bobmatnyc/trusty-tools/pull/5128)

### M21 — `trusty-search/CLAUDE.md` cites five paths that do not exist

- **File / symbol:** `crates/trusty-search/CLAUDE.md`
- **Claim:** `src/mcp/tools.rs`, `src/context/bm25.rs`,
  `src/core/migration/m00N.rs`, `src/commands/start.rs`,
  `src/core/memory_policy.rs`
- **Source:** none resolve. `src/mcp/tools/` and `src/commands/start/` are now
  directories; the other three have no counterpart under those names. This is the
  500-SLOC split churn landing on a file agents read every session.
- **Status:** new

### M22 — `docs/trusty-memory/spec` cites the deleted crate as a live path

- **Files / symbols:** `docs/trusty-memory/spec/ARCHITECTURE.md`,
  `docs/trusty-memory/decisions/0001-frontend-core-split.md`
- **Claim:** path references to `crates/trusty-memory-core/`
- **Source:** the directory does not exist. `ARCHITECTURE.md` says so in prose
  two lines away from one of its own dangling path citations, so the fix is
  mechanical.
- **Status:** new

### M23 — `release-workflow.md` cites a module file that is a directory

- **File / symbol:** `docs/reference/release-workflow.md` :: signing section
- **Claim:** `crates/trusty-installer/src/commands/macos_signing.rs`
- **Source:** `crates/trusty-installer/src/commands/macos_signing/` is a
  directory. Same class as M21 — a module split that docs did not follow.
- **Status:** new

---

## LOW

Cosmetic staleness.

### L1 — `crate-map.md` describes `trusty-code` as a Phase 0 scaffold

- **File / symbol:** `docs/reference/crate-map.md` :: `trusty-code` row
- **Claim:** "Phase 0 scaffold; extraction tracked in #587"
- **Source:** `trusty-code` is at v0.3.0, published to crates.io (0.2.0), has a
  Homebrew formula, ships the `tcode` binary and a GUI sibling crate.
- **Status:** new

### L2 — install guide's sample `tctl status` output is version-stale

- **File / symbol:** `docs/getting-started/install-and-run-tm.md` ::
  "Expected output" blocks — **on the public manifest**
- **Claim:** `trusty-search 0.31.0`, `trusty-memory 0.18.2`, `trusty-mpm 0.16.0`,
  `trusty-analyze 0.7.0`, `trusty-review 0.6.4`, `tga 2.8.0`,
  `trusty-console 0.3.1`; and `tm doctor` reporting "55 agent(s)",
  "262 skill(s)", "19 skill file(s)".
- **Source:** current tree versions are 0.43.0 / 0.22.0 / 1.3.5 / 0.8.0 / 0.11.1
  / 2.11.0 / 0.5.0. Illustrative sample output, but every number is wrong and one
  (`trusty-mpm 0.16.0`) is seven minor versions behind a 1.x release.
- **Status:** new

### L3 — `trusty-memory` README cites two modules that are now directories

- **File / symbol:** `crates/trusty-memory/README.md` :: REST API section
- **Claim:** "verified against `src/web.rs` and `src/service.rs`"
- **Source:** both are directories (`src/web/`, `src/service/`). The modules
  exist; only the `.rs` suffix is stale.
- **Status:** new

### L4 — `trusty-gworkspace` README cites a module that is now a directory

- **File / symbol:** `crates/trusty-gworkspace/README.md` :: "The authoritative
  list with JSON Schemas is in [`src/tools.rs`](src/tools.rs)"
- **Source:** `crates/trusty-gworkspace/src/tools/` is a directory.
- **Status:** new

### L5 — `DOC-46` describes a proposed gate in the present tense

- **File / symbol:** `docs/specs/DOC-46-adr-standard.md` :: §6 and the rollout
  list
- **Claim:** §"Propose a new script: `scripts/check_adr.sh`" (correctly
  conditional) followed by "**CI catches violations** — `scripts/check_adr.sh`
  runs on all ADR PRs and fails if …" (asserted).
- **Source:** `scripts/check_adr.sh` does not exist and no workflow references
  it. The spec is internally inconsistent about whether the gate is built.
- **Status:** new

---

## Unresolved

Nothing in this sweep ended unresolved. Every row above was settled against a
declaring file, a manifest field, a git tag, the crates.io API, or the Homebrew
tap contents.

One item was **investigated and cleared** rather than filed, recorded here so it
is not re-investigated: `crates/trusty-review/README.md` documents
`TRUSTY_REVIEW_CONTEXT_CONFORMANCE_ENABLED` and siblings, which a literal grep
does not find in source. They are built at runtime by
`crates/trusty-review/src/integrations/context/config.rs` via
`format!("TRUSTY_REVIEW_CONTEXT_{env_key}_ENABLED")`, so the documentation is
correct. Any future mechanical env-var gate must handle constructed names or it
will report this as a false positive.
