# Install Paths by Audience — Ground Truth

<!-- SLD: Spec-References: #5092, #5109 -->

Part of epic [#5092](https://github.com/bobmatnyc/trusty-tools/issues/5092) (public website). Tracking issue: [#5109](https://github.com/bobmatnyc/trusty-tools/issues/5109).

Why: the website's install walkthrough must publish only commands that actually
work. This document establishes, for nine audiences, the verified install
sequence, what `tctl` (`trusty-installer`) actually does, macOS TCC
requirements, and every place an existing doc disagrees with what the repo
itself does. Every claim below carries a confidence label.

## Confidence legend

- **VERIFIED** — checked directly against source, `Cargo.toml`, the crates.io
  API, or `gh release list`. Citation given as `file:line` or an exact command.
- **INFERRED** — deduced from adjacent evidence (e.g. two independent docs
  agreeing, or a pattern consistent across sibling crates) but not directly
  executed.
- **UNKNOWN** — not established here. What would settle it is stated.

All crates.io lookups were made against `https://crates.io/api/v1/crates/<name>`
on 2026-08-07. All release lookups used `gh release list --repo
bobmatnyc/trusty-tools`. All versions below are "local tree" (this branch's
`Cargo.toml`, off `origin/main`) vs. "published" (crates.io), since the two
routinely diverge by a few patch releases in a fast-moving monorepo.

## The `tctl` mental model (read this before the per-audience sections)

`tctl` (crate `trusty-installer`, binary aliased `tctl`) installs a fixed,
topologically-ordered **STABLE SET** — VERIFIED,
`crates/trusty-installer/src/commands/stable_set.rs:172-179`:

```
trusty-search → trusty-memory → trusty-analyze → trusty-review → tga → trusty-console → trusty-mpm
```

Only these seven crates are `tctl install`-able. **`trusty-agents` and
`trusty-code` are NOT stable-set members** — VERIFIED, the same file's module
doc says a future lane "may add `trusty-agents` to this set" (i.e. it isn't
there yet), and `docs/reference/release-workflow.md:253` states outright:
`trusty-agents (tagent) | N/A — not a tctl install member (#4277)`.
`tctl install trusty-code` or `tctl install trusty-agents` fails with
`unknown member(s): …` — VERIFIED,
`crates/trusty-installer/src/commands/install.rs:136-137` plus the
`select_members`/`select_members_transitive` unknown-name path in
`stable_set.rs`.

**Runtime "requires" edges** (what naming ONE member transitively pulls in) —
VERIFIED, `crates/trusty-installer/src/commands/dependency_graph.rs:63-66`,
the only two edges in the whole graph:

```
trusty-mpm    requires trusty-memory, trusty-search
trusty-review requires trusty-search, trusty-analyze
```

No other member requires another at the runtime/process level — the same file
states explicitly that `trusty-console` merely probes/proxies (service
discovery, not a hard requirement) and `tga` "has no references to any other
stable-set member's runtime surface." This means:

- `tctl install trusty-search` installs **only** trusty-search.
- `tctl install trusty-memory` installs **only** trusty-memory.
- `tctl install trusty-search trusty-memory` installs **only those two** — no
  edge exists between them (see the Audience 3 section for why they're grouped
  as an audience anyway).
- `tctl install trusty-analyze` installs **only** trusty-analyze.
- `tctl install trusty-review` transitively installs trusty-review +
  trusty-search + trusty-analyze (3 members).
- `tctl install trusty-mpm` transitively installs trusty-mpm + trusty-memory +
  trusty-search (3 members) — VERIFIED consistent with
  `docs/getting-started/install-and-run-tm.md:26-30`, which independently
  describes the same three binaries.
- `tctl install tga` installs **only** tga.
- Bare `tctl install` (no member names) installs the **full** stable set (all
  seven) — VERIFIED, `stable_set.rs` doc: "When `names` is empty, returns the
  full stable_set."

**How `tctl install` actually places a binary** — VERIFIED,
`crates/trusty-installer/src/commands/install.rs:1-33`: prebuilt-tarball-first
(GitHub Releases, SHA-256 verified, into `~/.local/bin`) on a **Tier-1**
platform (macOS arm64, Linux x86_64, Linux arm64 — VERIFIED,
`crates/trusty-installer/src/download/platform.rs:60-95`); falls back to
`cargo install <crate> --locked` (literally, no `--path`/`--git` — pulls the
**published crates.io version**) when the prebuilt download fails or the host
isn't Tier-1 — VERIFIED, `crates/trusty-common/src/update/upgrade.rs:179-193`.

## Per-audience install sequences

### 1. `trusty-memory` only

- **crates.io**: VERIFIED published, `0.21.2` (local tree `0.22.0`,
  `crates/trusty-memory/Cargo.toml:2`).
- **Install**:
  ```bash
  # Recommended — tctl (prebuilt on Tier-1, falls back to cargo install)
  tctl install trusty-memory

  # Direct crates.io
  cargo install trusty-memory --locked
  ```
- **tctl**: installs trusty-memory alone — no dependency edges (VERIFIED,
  `dependency_graph.rs`).
- **Prerequisites**: none required to start — VERIFIED,
  `docs/distribution/INSTALL-CONVENTION.md:177-179` ("self-contained, no
  external databases"). `OPENROUTER_API_KEY` is optional, only for the
  embedded chat panel — VERIFIED, same file lines 181-190. Building **from a
  git checkout** needs `pnpm` for the embedded Svelte UI, but the published
  crate ships the UI pre-built: `crates/trusty-memory/ui/dist/**` is committed
  to git (VERIFIED, `git ls-files crates/trusty-memory/ui | grep dist` returns
  3 tracked files), so `cargo install trusty-memory` never invokes `pnpm`.
- **Post-install**: `trusty-memory port` reports the live port (default UI at
  `http://127.0.0.1:<port>`) — VERIFIED,
  `docs/reference/running-mcp-servers.md:30-33`. MCP wiring —
  INFERRED from `crates/trusty-memory/README.md`'s `.mcp.json` snippet
  (`command: trusty-memory, args: ["serve", "--stdio"]`), cross-checked
  against a real CLI parse test: `cli_tests.rs:27` parses
  `["trusty-memory", "serve"]` into `Command::Serve`, VERIFIED.
- **macOS TCC**: **No Full Disk Access needed.** VERIFIED,
  `docs/reference/release-workflow.md:176-178`: "`trusty-memory` and
  `trusty-analyze` read `$HOME` locations only and also do NOT require FDA."
  trusty-memory is not in the Developer-ID `SIGNABLE_BINARIES` table
  (`crates/trusty-installer/src/commands/macos_signing/mod.rs:135-165`), so it
  has no persistent-grant story at all — it simply doesn't need one.
- **Dependencies on other products**: none. Standalone daemon.

### 2. `trusty-search` only

- **crates.io**: VERIFIED published, `0.42.2` (local tree `0.43.0`; latest GH
  release is `trusty-search-v0.42.3`, so crates.io trails the newest tag by
  one patch as of this check).
- **Install**:
  ```bash
  tctl install trusty-search
  # or
  cargo install trusty-search --locked
  ```
- **tctl**: installs trusty-search alone.
- **Prerequisites**: 16 GB RAM minimum, hard-checked at startup
  (`TRUSTY_SKIP_RAM_CHECK=1` to bypass) — VERIFIED,
  `docs/distribution/INSTALL-CONVENTION.md:160`. ~2 GB disk for the first-run
  ONNX model download cache. Apple Silicon CoreML GPU is automatic; NVIDIA
  CUDA is an opt-in `--features cuda` build. UI is pre-built and committed
  (`crates/trusty-search/ui-dist/**`, 3 tracked files — VERIFIED via
  `git ls-files`), so `pnpm` is not required for the published crate.
- **Post-install**: `trusty-search port` / `trusty-search start`. MCP wiring:
  `command: trusty-search, args: ["serve"]` — INFERRED from
  `crates/trusty-search/README.md`, cross-checked against
  `docs/reference/running-mcp-servers.md:11-12` (`cargo run -p trusty-search --
  serve`), two independent sources agreeing.
- **macOS TCC**: **Full Disk Access IS required**, and this is the ONLY
  product in this table that needs it. VERIFIED,
  `docs/reference/release-workflow.md:173-176`: "Full Disk Access scope:
  `trusty-search` and external-volume daemons only." Persistent-grant signing:
  `trusty-search` + bundled `trusty-embedderd` share `SEARCH_SET`, identifier
  `com.trusty.trusty-search` / `com.trusty.trusty-embedderd` — VERIFIED,
  `macos_signing/mod.rs:93-137`. FDA is only actually exercised when index
  data lives on an external/removable volume — VERIFIED,
  `crates/trusty-search/README.md:730-767` (the warm-boot/launchd TCC-denial
  troubleshooting section). Local-disk-only indexes never trigger a prompt.
- **Dependencies on other products**: none (leaf in the dependency graph).

### 3. `trusty-search` + `trusty-memory` together

- **Install**:
  ```bash
  tctl install trusty-search trusty-memory
  # or
  cargo install trusty-search --locked
  cargo install trusty-memory --locked
  ```
- **Install order**: does not matter. VERIFIED — there is no dependency edge
  between the two (`dependency_graph.rs` has exactly two edges total, neither
  involving this pair). They can be installed in either order or in parallel.
- **Why this audience exists — precisely stated**: both daemons link the
  **same** in-process embedding code — `trusty-common`'s `embedder` /
  `embedder-bundled-ort` feature (fastembed + ONNX Runtime) — VERIFIED,
  `crates/trusty-common/Cargo.toml:311,415-430` (trusty-memory's `memory-core`
  feature pulls `embedder` + `embedder-bundled-ort`) and
  `crates/trusty-search/Cargo.toml:57` (same features, direct dependency
  line). This is a **shared-library relationship, not a runtime/process
  dependency** — neither daemon calls the other over the network, and each
  runs and serves fine with the other absent. The two products are
  complementary (trusty-search indexes code; trusty-memory stores semantic/
  conversational memory) rather than layered. Framing either daemon as "backed
  by" the other at the process level is not accurate; both are backed by the
  same `trusty-common` embedder library.
- **Prerequisites**: union of audiences 1 and 2 (16 GB RAM dominates, since
  trusty-search's requirement is the higher of the two).
- **macOS TCC**: union of the two categories above — grant Full Disk Access
  only to trusty-search (only if its indexes live on an external volume);
  trusty-memory needs nothing.
- **MCP wiring**: register both entries in `.mcp.json` independently, per
  their individual sections above.

### 4. `trusty-analyze`

- **crates.io**: VERIFIED published, `0.7.4` (local tree `0.8.0`).
- **Install**:
  ```bash
  tctl install trusty-analyze
  # or
  cargo install trusty-analyze --locked
  ```
- **tctl**: installs trusty-analyze alone; it is an **OPTIONAL** stable-set
  member (may lack a prebuilt for a given platform without failing the whole
  run) — VERIFIED, `stable_set.rs:161-166`, `required: false`.
- **Prerequisites**: 8 GB RAM minimum, ~500 MB disk for the model cache —
  VERIFIED, `docs/distribution/INSTALL-CONVENTION.md:200-204`. Basic
  complexity/smell analysis needs no LLM; the **deep-analysis pass** is
  optional and needs `OPENROUTER_API_KEY` (default) or AWS Bedrock via
  `TRUSTY_LLM_MODEL`/`AWS_REGION` — VERIFIED, same file lines 206-221. UI is
  pre-built and committed (`crates/trusty-analyze/ui/dist/**`, tracked —
  VERIFIED via `git ls-files`), so `pnpm` is not required to install the
  published crate.
- **Post-install**: MCP wiring `command: trusty-analyze, args: ["serve",
  "--mcp"]` — VERIFIED, `crates/trusty-analyze/src/commands/setup.rs:32`
  (`const MCP_SERVER_ARGS: &[&str] = &["serve", "--mcp"];`, the literal
  generator constant `setup.rs`'s `.mcp.json`-writing code consumes), with a
  matching CLI-parse assertion at `setup.rs:345-346,366`.
- **macOS TCC**: **No Full Disk Access needed** — same explicit carve-out as
  trusty-memory: "reads `$HOME` locations only" —
  VERIFIED, `docs/reference/release-workflow.md:176-178`.
- **Dependencies on other products**: none as a leaf (nothing requires
  trusty-analyze to install it), but trusty-review requires trusty-analyze
  transitively (see Audience 5) — that edge runs the other direction.

### 5. `trusty-review`

- **crates.io**: VERIFIED published (`publish = true` explicit,
  `crates/trusty-review/Cargo.toml:12`), `0.11.0` (local tree `0.11.1`).
- **Install**:
  ```bash
  # tctl transitively pulls trusty-search + trusty-analyze too — see below
  tctl install trusty-review
  # or, direct
  cargo install trusty-review --locked
  ```
- **tctl**: `tctl install trusty-review` installs **three** members —
  trusty-review, trusty-search, trusty-analyze — because trusty-review has a
  hard runtime dependency on both. VERIFIED,
  `dependency_graph.rs:63-66` and its doc: the "required-context preflight
  gate" (`crates/trusty-review/src/pipeline/context_gate.rs`, referenced in
  the module doc, issue #590) **skips the review entirely** if either
  dependency is unreachable, absent an explicit degraded-mode opt-in — "a
  review produced WITHOUT that context is actively harmful." trusty-review is
  also a **REQUIRED** stable-set member (`stable_set.rs:161`, `required:
  true`).
- **Prerequisites**: 8 GB RAM, ~500 MB disk for the model cache. LLM
  configuration is **required, not optional**, for code review (unlike
  trusty-analyze's deep pass) — VERIFIED,
  `docs/distribution/INSTALL-CONVENTION.md:250-263`: `OPENROUTER_API_KEY`
  default, AWS Bedrock alternative via the same two env vars as trusty-analyze.
- **Post-install**: MCP wiring `command: trusty-review, args: ["mcp"]` —
  VERIFIED, `crates/trusty-review/src/main.rs:145-149` (the `.mcp.json`
  snippet in the `Mcp` variant's own doc comment). `["serve", "--stdio"]`
  still parses — kept as a back-compat alias for every `.mcp.json` written
  before #6290 (`#[command(alias = "serve")]`, `main.rs:158`; both spellings
  land on `Commands::Mcp`, proven by
  `main.rs::tests::serve_is_still_accepted_as_an_alias`, `main.rs:223-240`) —
  but `["mcp"]` is the current canonical form to publish.
- **macOS TCC**: **No category needed.** VERIFIED,
  `crates/trusty-installer/src/commands/macos_signing/mod.rs:97-99`:
  trusty-review "was evaluated under the same test [as trusty-memory/
  trusty-analyze] and EXCLUDED: it makes no `$HOME` walk and reads no other
  app's files, so there is no grant for a stable identity to preserve." This
  is consistent with, and explains, its absence from both the FDA/App-Data
  carve-out list in `release-workflow.md:173-179` and the `SIGNABLE_BINARIES`
  table in the same `macos_signing/mod.rs` — the absence is a documented
  exclusion, not an oversight.
- **Dependencies on other products**: requires trusty-search + trusty-analyze
  at runtime (hard gate, not soft). Install order does not matter for `tctl`
  (it resolves the closure and orders topologically automatically); if
  installing manually with plain `cargo install`, bring up trusty-search and
  trusty-analyze **before** trusty-review to avoid the degraded-mode warning
  on first run.

### 6. `trusty-mpm` (binary `tm`)

- **crates.io**: VERIFIED published, `1.3.4` (local tree `1.3.5`).
- **Install** (this is the flow the site's main walkthrough already documents
  and it checks out):
  ```bash
  curl -sSf https://raw.githubusercontent.com/bobmatnyc/trusty-tools/main/install.sh | sh
  tctl install trusty-mpm
  tctl status
  ```
  or directly: `cargo install trusty-mpm --locked` (installs both `tm` and
  `trusty-mpm` binaries from the same crate — VERIFIED,
  `crates/trusty-mpm/Cargo.toml:124-131`, two `[[bin]]` entries both pointing
  at `src/bin/tm/main.rs`).
- **tctl**: `tctl install trusty-mpm` transitively installs trusty-mpm +
  trusty-memory + trusty-search (3 members) — VERIFIED (see the mental-model
  section above); this exact 3-binary claim is independently corroborated by
  `docs/getting-started/install-and-run-tm.md:26-30`.
- **Prerequisites**: Claude Code installed (recommended, not hard-enforced at
  install time) — INFERRED from
  `docs/getting-started/install-and-run-tm.md:38`. Rust is optional — only
  needed if the prebuilt binary for the host platform is unavailable.
- **Post-install / config**: real config path is
  **`~/.config/trusty-mpm/config.toml`** (honouring `$XDG_CONFIG_HOME`) —
  VERIFIED, `crates/trusty-mpm/src/bin/tm/commands/managed_root.rs:12,123,198`.
  There is **no separate `trusty-mpmd` daemon binary** — the daemon runs
  in-process as a mode of the same `tm`/`trusty-mpm` binary
  ("Long-running daemon mode … identical to the former trusty-mpmd binary" —
  VERIFIED, `crates/trusty-mpm/src/bin/tm/main.rs:233-235`, past tense,
  confirming the binary was retired/folded in). Lifecycle is controlled via
  `tm`'s own `start`/`stop`/`restart` subcommands, **not** launchd —
  VERIFIED, `stable_set.rs:26-36`, `ManageStrategy::OwnVerb` is derived
  specifically for `binary == "trusty-mpm"`.
- **macOS TCC**: **App Data category, never Full Disk Access.** VERIFIED,
  `docs/reference/release-workflow.md:179-187`: `tm` reads other apps'
  `$HOME` containers (Claude config dirs, tmux state), which triggers the
  distinct "would like to access data from other apps" prompt — a **different**
  category from FDA. Both `trusty-mpm` and `tm` binaries are signed together
  under `MPM_SET` (`com.trusty.trusty-mpm`, `com.trusty.tm`) — VERIFIED,
  `macos_signing/mod.rs:97-152`. Never grant FDA to `tm`/`trusty-mpm` —
  explicit in the same doc: "should never be granted — Full Disk Access."
- **Dependencies on other products**: requires trusty-memory + trusty-search
  at runtime (both MCP servers are injected into every managed session by
  default — VERIFIED, `dependency_graph.rs:16-19`, referencing
  `crates/trusty-mpm/src/core/manifest/default.rs` and
  `crates/trusty-mpm/src/daemon/discover.rs`).

### 7. `trusty-code`

- **crates.io**: VERIFIED published, `0.2.0` (local tree `0.3.0`).
- **Install**:
  ```bash
  cargo install trusty-code --locked
  ```
  `tctl install trusty-code` **fails** — trusty-code is not a stable-set
  member (absent from `stable_set()`'s seven entries) — VERIFIED.
- **Prerequisites**: git (reads git metadata for branch context). Claude Code
  itself is "optional but recommended" — INFERRED,
  `docs/distribution/INSTALL-CONVENTION.md:310-315`. No Svelte UI (no
  `build.rs` in `crates/trusty-code/`, confirmed by absence — VERIFIED,
  directory listing shows no `build.rs`), so no `pnpm` requirement at all. No
  API key is required to start `tcode serve` itself — VERIFIED,
  `crates/trusty-code/src/llm/dispatch.rs:14-20` (module doc: "no key is
  required at construction … a missing key only surfaces if a slug that needs
  it is actually dispatched"). A key becomes required only lazily, the first
  time a chat/task actually dispatches to a provider that needs one; the
  default route is OpenRouter, so `OPENROUTER_API_KEY` is the one most
  operators hit first — VERIFIED, `crates/trusty-code/src/llm/client.rs:329-337`
  (the exact `MissingConfig` error raised inside `build_adapter`, only at chat
  time). Routing a model to `fireworks/*`/`together/*`/`atlascloud/*`/
  `bedrock/*` substitutes that provider's own key instead.
- **Post-install**: one `tcode serve` process per project's `.claude/` root —
  VERIFIED, `crates/trusty-code/README.md:14-16`.
- **macOS TCC**: **INFERRED: no category applies.** trusty-code appears
  nowhere in the FDA/App-Data carve-out list or the `SIGNABLE_BINARIES` table
  (VERIFIED — the full 11-row table in
  `crates/trusty-installer/src/commands/macos_signing/mod.rs:207-253` has no
  `trusty-code`/`tcode` entry), and it makes no
  `discover_claude_settings`/`$HOME`-walk call anywhere in its source
  (VERIFIED — no match for `discover_claude_settings` or `claude_config`
  under `crates/trusty-code/src`), which is the pattern that earns
  trusty-memory/trusty-analyze their Files-and-Folders exposure. Its
  `fs_browse` daemon module (the UI's project picker) reads whichever
  directory the operator navigates to, but by its own doc explicitly
  implements no TCC permission layer: "There is likewise no macOS TCC
  permission-state machine: the app inherits whatever entitlements it is
  granted, and an OS-level refusal surfaces as an ordinary typed error" —
  VERIFIED, `crates/trusty-code/src/fs_browse/mod.rs:23-34`. No
  persistent-identity signing script exists for it either (only
  `install-trusty-search-signed.sh`, `install-trusty-mpm-signed.sh`,
  `install-trusty-agents-signed.sh` exist under `scripts/`). Inference: same
  "None" class as trusty-analyze/trusty-review — no automatic other-app-
  container read, no built-in permission model, and no signable-set entry to
  preserve a grant across reinstalls. Settling this to VERIFIED would still
  need a live launchd run on a clean macOS host.
- **Dependencies on other products**: none identified; it is a per-project
  harness, independent of the tctl stable set.

### 8. `trusty-agents` (binary `tagent`)

- **crates.io**: VERIFIED **not published** —
  `curl https://crates.io/api/v1/crates/trusty-agents` returns `{"errors":
  [{"detail":"crate \`trusty-agents\` does not exist"}]}`. `Cargo.toml` has
  **no `publish = false` field either** — VERIFIED, `grep -n publish
  crates/trusty-agents/Cargo.toml` matches only two unrelated comments about
  its dependencies, none on the `[package]` table itself. It simply has never
  been published, not that it's blocked from being.
- **GitHub Releases**: VERIFIED zero releases exist —
  `gh release list --repo bobmatnyc/trusty-tools --limit 200 | grep -i
  trusty-agents` returns nothing.
- **The only working install path today** is a monorepo clone + local build —
  this is the exact defect the epic brief opened with (the stale
  `cargo install open-mpm` in `docs/trusty-agents/user/quickstart.md`):
  ```bash
  git clone https://github.com/bobmatnyc/trusty-tools
  cd trusty-tools
  cargo install --path crates/trusty-agents --locked
  ```
  VERIFIED, `scripts/install-trusty-agents-signed.sh:14-15,192` uses exactly
  this `cargo install --path` form (never crates.io, never `--git`) as the
  authoritative install mechanism for this crate.
- **tctl**: `tctl install trusty-agents` **fails** — not a stable-set member,
  confirmed twice (`stable_set.rs`'s "future lane" note, and
  `release-workflow.md:253`'s explicit "N/A — not a tctl install member
  (#4277)").
- **Prerequisites**: Rust 1.94+ (source build, mandatory — there is no
  prebuilt). `pnpm` is needed for a **functional** web UI: unlike
  trusty-search/trusty-memory/trusty-analyze, `crates/trusty-agents/ui/dist/`
  is **not** committed to git (VERIFIED, `git ls-files
  crates/trusty-agents/ui | grep dist` returns zero files). Without `pnpm`,
  `build.rs` writes a placeholder `index.html` so the build still succeeds,
  but the embedded UI is non-functional — VERIFIED,
  `crates/trusty-agents/build.rs:1-19` (`RustEmbed` needs SOME directory to
  exist; issue #112). Model-routing API keys: none is strictly required to
  launch `tagent` — VERIFIED,
  `crates/trusty-agents/src/llm/credentials.rs:108` (`pick_credentials`
  returns `None` when nothing resolves; test
  `pick_returns_none_when_nothing_set`, same file). Any ONE of
  `CLAUDE_CODE_OAUTH_TOKEN` > `ANTHROPIC_API_KEY` > `OPENROUTER_API_KEY`
  (checked in that priority order, each resolved via env >
  project/user `.env.local` > the secure `tagent config keys set` store)
  suffices; a `/provider bedrock` or `/provider local` (Ollama) run needs
  neither. Absence prints an onboarding banner recommending OpenRouter rather
  than failing startup — VERIFIED,
  `crates/trusty-agents/src/runtime/cli_def.rs:319-345`
  (`any_credential_resolves`, the banner predicate).
- **Post-install**: `tagent mcp-serve` runs a stdio MCP server exposing
  trusty-agents itself to external MCP clients (Claude Code, etc.) —
  VERIFIED, `crates/trusty-agents/src/runtime/mcp_serve.rs:1-30` (module
  doc). It reuses the same `trusty_mcp::run_stdio_loop` framework
  trusty-memory and trusty-search already ship against, exposing a static
  two-tool surface (`list_agents`, `dispatch_task`). Dispatch is a raw argv
  check ahead of clap parsing — VERIFIED,
  `crates/trusty-agents/src/runtime/mod.rs:203-204`
  (`if args.len() > 1 && args[1] == "mcp-serve"`). MCP wiring:
  `command: tagent, args: ["mcp-serve"]`.
  There is also a separate desktop shell, `Trusty Agents.app`
  (`crates/trusty-agents/ui/src-tauri`), built via `pnpm tauri build` and
  signed through Tauri's own `APPLE_SIGNING_IDENTITY` mechanism — VERIFIED,
  `scripts/install-trusty-agents-signed.sh:20-33`. This is a second,
  independent build artifact from the CLI/daemon binary.
- **macOS TCC**: **App Data category** — same class as `tm`, not FDA.
  VERIFIED, `scripts/install-trusty-agents-signed.sh:4-10`: "`tagent`… reads
  `$HOME`/project `.trusty-agents/` config and state, and — like `tm`
  (#2721) — project-local `.claude/` dirs that live under other apps' data
  categories." Signed under its own `AGENTS_SET`, identifier
  `com.trusty.tagent` — VERIFIED, `macos_signing/mod.rs:106-121,165`.
- **Dependencies on other products**: `trusty-search` is linkable as an
  in-process Rust **library** by trusty-agents (`crate-type = ["rlib"]`,
  explicitly for this consumer — VERIFIED,
  `crates/trusty-search/Cargo.toml:26-30`, comment names trusty-agents
  directly). This is a compile-time library dependency, not a requirement to
  have a separate trusty-search daemon running.

### 9. `tga` (trusty-git-analytics)

- **crates.io**: VERIFIED published (`publish = true` explicit,
  `crates/trusty-git-analytics/Cargo.toml:4`), `2.11.0` — **matches** the
  local tree exactly, the only audience where published and local agree.
- **Package name vs. directory** — directory is
  `crates/trusty-git-analytics/`, package name is **`tga`** — VERIFIED,
  `crates/trusty-git-analytics/Cargo.toml:2` (`name = "tga"`); binary is also
  `tga`.
- **Install**:
  ```bash
  tctl install tga
  # or
  cargo install tga --locked
  ```
- **tctl**: installs tga alone — a leaf in the dependency graph, and an
  **OPTIONAL** stable-set member (`required: false`) — VERIFIED,
  `stable_set.rs:165`.
- **Known-wrong doc — do not copy**: `docs/trusty-git-analytics/user/user-guide.md`
  (owned by [PR #5107](https://github.com/bobmatnyc/trusty-tools/pull/5107),
  read-only here) instructs `cp target/release/tga /usr/local/bin/tga` and,
  for prebuilt binaries, a plain `mv … /usr/local/bin/tga` with no signing
  step — VERIFIED, lines 56-59 and 69-73 of that file. This is exactly the
  macOS cdhash anti-pattern `CLAUDE.md` warns about: a `cp`/`mv`-installed
  binary is ad-hoc-signed with a churning identity, and the *next* exec after
  any file replacement risks a SIGKILL that looks like an OOM kill. `tga`
  itself has **no** Developer-ID signing script (`scripts/install-tga-*.sh`
  does not exist, and `tga` is absent from the `SIGNABLE_BINARIES` table in
  `macos_signing/mod.rs`) — so the correct instruction is the plain, real
  `cargo install tga --locked` / `tctl install tga` path above, which lets
  cargo's own atomic-rename install handle placement safely, not a manual
  `cp`/`mv` over a PATH binary.
- **Prerequisites**: git (git2), SQLite bundled (no external install) —
  VERIFIED, `docs/distribution/INSTALL-CONVENTION.md:292-296`.
- **Post-install**: config resolution is a single `-c`/`--config` CLI flag,
  global across every subcommand, defaulting to `config.yaml` resolved
  relative to the current working directory when omitted — VERIFIED,
  `crates/trusty-git-analytics/src/main.rs:53-55`
  (`#[arg(short, long, default_value = "config.yaml", global = true)]`).
  There is no `~/.config/tga/config.yaml` fallback, no `tga.yaml` alternate
  name, and no environment-variable override anywhere in source — VERIFIED,
  no `dirs::config_dir`/`XDG_CONFIG_HOME`/`TGA_CONFIG` reference exists
  anywhere under `crates/trusty-git-analytics/src`.
  `docs/distribution/INSTALL-CONVENTION.md:298-300`'s claim of that fallback
  path does not match the code and should not be copied into the walkthrough.
  **No MCP transport at
  all** — VERIFIED, `crates/trusty-git-analytics/src/main.rs:268`: "tga has no
  MCP stdio transport — all other subcommands are [handled directly]." tga is
  a pure CLI analytics tool.
- **macOS TCC**: **No category identified, and none is expected.** tga is
  absent from both the FDA/App-Data carve-out doc and the `SIGNABLE_BINARIES`
  table; it reads only local git repository history via `git2`, a class of
  access that has never triggered a TCC prompt for any other crate in this
  repo. Labeled INFERRED, not VERIFIED, since no doc states this explicitly
  the way it does for trusty-memory/trusty-analyze.
- **Dependencies on other products**: none — confirmed leaf,
  `dependency_graph.rs` module doc.

## Prerequisites matrix

| Audience | Rust needed? | pnpm needed? | LLM API key | RAM floor | macOS TCC |
|---|---|---|---|---|---|
| 1. trusty-memory | No (prebuilt/crates.io) | No (UI pre-built, committed) | Optional (`OPENROUTER_API_KEY`, chat only) | none stated | None |
| 2. trusty-search | No (prebuilt/crates.io) | No (UI pre-built, committed) | No | 16 GB | **FDA** (external-volume indexes only) |
| 3. search + memory | No | No | Optional (memory chat) | 16 GB (max of the two) | FDA for search only |
| 4. trusty-analyze | No | No (UI pre-built, committed) | Optional (deep-analysis pass) | 8 GB | None |
| 5. trusty-review | No | N/A (no UI) | **Required** (`OPENROUTER_API_KEY` or Bedrock) | 8 GB | None (VERIFIED — evaluated and excluded) |
| 6. trusty-mpm (`tm`) | Optional (fallback only) | N/A (no UI) | N/A directly (agents it launches may need one) | none stated | **App Data**, never FDA |
| 7. trusty-code | No | N/A (no UI) | Optional — lazy, only when a chat/task actually dispatches (default provider OpenRouter) | none stated | None (INFERRED) |
| 8. trusty-agents (`tagent`) | **Yes, mandatory** (no prebuilt, no crates.io) | Recommended (UI is a placeholder without it) | Optional — any ONE of `CLAUDE_CODE_OAUTH_TOKEN`/`ANTHROPIC_API_KEY`/`OPENROUTER_API_KEY` (priority order); none is strictly required to launch | none stated | **App Data**, never FDA |
| 9. tga | No | N/A (no UI) | No | none stated | None (inferred) |

Shared across all nine: **Rust 1.94 MSRV** if building from source at all
(`CLAUDE.md`); `git` for anything that touches the monorepo directly
(mandatory for audience 8, optional elsewhere).

## Contradictions found

1. **`docs/trusty-agents/user/quickstart.md`** tells readers to
   `git clone https://github.com/bobmatnyc/open-mpm` and
   `cargo install open-mpm` — neither the repo nor the crate exists under that
   name. VERIFIED, lines 10-19 of that file. (This is the defect named in the
   task brief; recorded here for completeness, not newly discovered.)

2. **`docs/trusty-git-analytics/user/user-guide.md`** installs `tga` via
   `cp target/release/tga /usr/local/bin/tga` (source build) and a bare `mv`
   (prebuilt binary), with no signing step — the macOS cdhash anti-pattern
   `CLAUDE.md` warns against. VERIFIED, lines 56-59, 69-73. (Named in the
   coordinator's brief and #5109; recorded here with the corrected command in
   Audience 9 above.)

3. **`docs/distribution/INSTALL-CONVENTION.md:18`** lists `trusty-agents` as a
   "`publish=false` crate," implying it's deliberately blocked from
   publishing. `crates/trusty-agents/Cargo.toml` has **no `publish` field at
   all** in `[package]` — it defaults to publishable and simply has never been
   published. Minor, but it mischaracterizes *why* there's no crates.io
   listing, which matters for a walkthrough deciding whether to wait for a
   future `cargo install trusty-agents` or commit to the git-clone path
   permanently.

4. **Two docs reference a `trusty-mpmd` binary that no longer exists.**
   `docs/distribution/INSTALL-CONVENTION.md:283` shows
   `trusty-mpmd --config /path/to/config.yaml`, and
   `docs/reference/running-mcp-servers.md:15` shows
   `cargo run -p trusty-mpm --bin trusty-mpmd`. `crates/trusty-mpm/Cargo.toml`
   defines exactly two `[[bin]]` targets, `tm` and `trusty-mpm`, both pointing
   at the same `src/bin/tm/main.rs` — there is no `trusty-mpmd` target, so
   both commands fail with "no bin target named `trusty-mpmd`." The source
   itself confirms the binary was retired: `src/bin/tm/main.rs:233-235`
   describes daemon mode as "identical to the **former** trusty-mpmd binary"
   (past tense) — it was folded into `tm`/`trusty-mpm`.

5. **`docs/distribution/INSTALL-CONVENTION.md:280`** also gets the config file
   extension wrong: it says `~/.config/trusty-mpm/config.yaml`. The real path,
   read directly from `managed_root.rs:12,123,198`, is
   `~/.config/trusty-mpm/config.toml` (TOML, not YAML), honouring
   `$XDG_CONFIG_HOME` when set.

6. **`crates/trusty-code/README.md`**'s "From GitHub Releases" section names
   tag `trusty-code-v0.0.0` — the unfilled `{{VERSION}}` placeholder from the
   INSTALL-CONVENTION template, never replaced with a real version. A reader
   following it literally requests a release that has never existed.
   `gh release list --repo bobmatnyc/trusty-tools` shows the real (and only)
   trusty-code release is `trusty-code-v0.2.0`, matching the crates.io
   published version. The "From Source with Cargo" section in the same
   README also only shows the `--git` form and never mentions the simpler
   `cargo install trusty-code --locked` from crates.io, even though the crate
   is published.

## Recommendation: center the walkthrough on `tctl`, with per-crate `cargo install` as the documented escape hatch

For seven of the nine audiences (everything except trusty-code and
trusty-agents), `tctl` is a strictly better default:

- It is the **only** path that gets the runtime dependency graph right for
  free — a reader manually running `cargo install trusty-review` alone would
  get a daemon that silently degrades every review (the #590 context-gate
  behavior) unless they also happen to install trusty-search and
  trusty-analyze; `tctl install trusty-review` does this correctly without
  the reader needing to know the graph exists.
- It is the **only** path that gets Developer-ID signing right automatically
  — `tctl install`'s post-install hook re-signs with the stable
  `com.trusty.*` identifiers, so the TCC grants described in this document
  (FDA for search, App Data for mpm/tagent) survive reinstalls. A reader doing
  raw `cargo install` gets an ad-hoc-signed binary that re-triggers every TCC
  prompt on every upgrade — the exact bug class #873/#2721/#4277 exist to fix.
- It prefers prebuilt tarballs on Tier-1 platforms, so most readers never need
  a Rust toolchain at all.

For **trusty-code** and **trusty-agents**, `tctl` is not an option — they are
not stable-set members — so the walkthrough must fall back to `cargo install
trusty-code --locked` (published, works today) and, for trusty-agents, the
git-clone + `cargo install --path` sequence (the only path that exists).
Because neither of these two products goes through `tctl`'s signing hook, the
walkthrough should point trusty-agents installers at
`scripts/install-trusty-agents-signed.sh` explicitly if they're on macOS and
want a persistent App-Data grant — otherwise they'll re-hit the TCC prompt on
every `cargo install --path` rebuild.

Concretely: **structure the site as "install via tctl" for the seven stable-set
products, with an explicit note that trusty-code and trusty-agents are
separate, source-only installs today** — rather than presenting nine
symmetric per-crate `cargo install` recipes, which would both bury the
dependency-graph and TCC-signing benefits `tctl` provides for free, and
misrepresent trusty-agents/trusty-code as being on the same footing as the
other seven when they demonstrably are not (no tctl membership, and for
trusty-agents, no crates.io package or GitHub Release at all).

## Formerly UNKNOWN, now resolved against source (#5116)

The seven facts below were UNKNOWN as of the pass that produced this
document. All seven are now VERIFIED or INFERRED directly against crate
source — see the per-audience sections linked for the full citation and
reasoning; this table is only an index.

| Claim | Resolution | Citation |
|---|---|---|
| trusty-review's macOS TCC category (Audience 5) | VERIFIED — none; explicitly evaluated and excluded | `crates/trusty-installer/src/commands/macos_signing/mod.rs:97-99` |
| trusty-code's macOS TCC category (Audience 7) | INFERRED — none | `crates/trusty-code/src/fs_browse/mod.rs:23-34`; absence confirmed across `macos_signing/mod.rs:207-253` |
| trusty-code's required vs. optional API keys (Audience 7) | VERIFIED — optional, resolved lazily at first chat/task dispatch, not at startup | `crates/trusty-code/src/llm/dispatch.rs:14-20`, `crates/trusty-code/src/llm/client.rs:329-337` |
| tagent's MCP server registration story (Audience 8) | VERIFIED — `tagent mcp-serve`, a stdio MCP server over the shared `trusty_mcp` framework | `crates/trusty-agents/src/runtime/mcp_serve.rs:1-30`, `crates/trusty-agents/src/runtime/mod.rs:203-204` |
| tagent's required vs. optional API key at first launch (Audience 8) | VERIFIED — optional; any one of three credentials in priority order, none strictly required | `crates/trusty-agents/src/llm/credentials.rs:108`, `crates/trusty-agents/src/runtime/cli_def.rs:319-345` |
| tga's exact config file resolution order (Audience 9) | VERIFIED — single `--config` flag, default `./config.yaml`, no `~/.config/tga/` fallback exists | `crates/trusty-git-analytics/src/main.rs:53-55` |
| trusty-analyze's and trusty-review's exact MCP CLI flags (Audiences 4, 5) | VERIFIED — `["serve", "--mcp"]` for trusty-analyze; `["mcp"]` (canonical) / `["serve", "--stdio"]` (back-compat alias) for trusty-review | `crates/trusty-analyze/src/commands/setup.rs:32`, `crates/trusty-review/src/main.rs:145-158` |

## Gates run

- `bash scripts/check_sld.sh` — see verification output below.
- `bash scripts/check_doc_numbers.sh` — see verification output below.
- `bash scripts/check_line_cap.sh` — see verification output below.
- `bash scripts/check_changelog_fragment.sh` — docs-only change; expected exempt.
