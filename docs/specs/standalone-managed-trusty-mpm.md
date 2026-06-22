# DOC-24 — Standalone Managed `trusty-mpm` Driver (register / load / run, config isolation)

**Status:** Draft
**Subsystem:** trusty-mpm — standalone driver / managed configuration
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-22
**Spec ID:** `SPEC-STANDALONE-MPM-01~draft` … `-08~draft` (DOC-24)
**Builds on:** DOC-14 — Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`,
the `SessionRecord` / `ManagedSessionId` / lifecycle contract); DOC-17 — Harness Runner Vision
(`docs/specs/harness-runner-vision.md`, the autonomous-operation north-star and provisioner seam);
DOC-22 — Multi-Repo Session Routing (`docs/specs/multi-repo-session-routing.md`, the named-project
registry and NL→repo resolver that this driver's alias model aligns with); DOC-23 — Learned-Autonomy
Auto-Answer (`docs/specs/learned-autonomy-auto-answer.md`, the autonomy tier the **session manager**
— the durable tmux fleet daemon, **not** `tm` itself — uses when it drives a tm-managed project).
**Cross-ref:** the workspace provisioner (`crates/trusty-mpm/src/provisioner/workspace.rs`,
`WorkspaceProvisioner::provision` / `provision_in`); session launch + per-workspace settings
(`crates/trusty-mpm/src/core/session_launch/mod.rs`, `prepare_session`;
`crates/trusty-mpm/src/core/session_launch/settings.rs`, `inject_trusty_memory_mcp` /
`inject_trusty_search_mcp` / `write_output_style` / `write_project_hooks` /
`preseed_workspace_trust_home` / `remove_global_trusty_memory_hooks` / `deploy_output_style`);
the agent / skill deployers (`crates/trusty-mpm/src/core/agent_deployer.rs`,
`deploy_agents_filtered`; `crates/trusty-mpm/src/core/skill_deployer.rs`, `deploy_skills_filtered`);
framework-path derivation (`crates/trusty-mpm/src/core/.../paths.rs`, `FrameworkPaths::claude_agents_dir`
/ `claude_skills_dir`); claude-config path model (`crates/trusty-mpm/src/core/claude_config.rs`,
`ClaudeConfigPaths` / `ClaudeConfigReader`); the CLI surface (`crates/trusty-mpm/src/bin/tm/cli.rs`,
`src/bin/tm/main.rs`); and epic **(to be filed)** plus the existing project registry (`tm project init/list/info`).

> **Scope note.** This is a **behavior contract** for a **standalone, IDE-attachable, fully-isolated
> `trusty-mpm` driver** and the **Managed Configuration Standard** it writes. It sits **on top of** the
> already-merged provisioner and session-launch core (DOC-14/DOC-17) and the multi-repo registry
> (DOC-22). It specifies: (1) an alias→GitHub-URL registry and the `register` / `load` / `run` /
> `path` / `ls` / `update` / `rm` command lifecycle; (2) the **Managed Configuration Standard** — a
> per-project directory layout where **CLAUDE.md + agents + skills live project-local under
> `repo/.claude/`** (standard Claude Code discovery) while **hooks + MCPs are supplied by tm via
> explicit launch arguments** (`--settings <file>` for hooks, `--mcp-config <file>` for MCPs) rather
> than placed in any discovery location — plus the **isolation invariant** that in managed mode
> trusty-mpm writes **nothing** to the user's global `~/.claude/` or `~/.claude.json`;
> (3) the **two-layer interaction model** — `tm` invoked directly is **always attended/interactive**
> (the claude-mpm replacement), and **autonomy is provided exclusively by the session manager**, the
> durable tmux fleet daemon that drives tm-managed projects under DOC-23; (4) first-class IDE attach
> via stable, discoverable project dirs carrying project-local `.claude/` + `CLAUDE.md` (the IDE
> inherits CLAUDE.md/agents/skills but **not** tm's hooks/MCPs, which require launching via `tm`).
> It does **not** re-spec the session lifecycle, the provisioner internals, the harness runner, the
> NL→repo resolver (DOC-22), or the autonomy tiers (DOC-23) — those are consumed as-is. This spec
> defines the contract; implementing modules are the **what**; the **how** is left to phase planning.

---

## Purpose & Scope

`trusty-mpm` today is a daemon-coupled session manager whose launch path (`tm launch`,
`tm sessions start`, managed provisioning) mutates **global** `~/.claude/` and `~/.claude.json` on
every invocation. That makes it unsafe to run as a standalone daily driver alongside a user's own
claude-mpm `~/.claude` setup: it deploys agents/skills globally, overwrites global output-styles,
strips global hooks, and seeds per-directory trust into the user's `~/.claude.json`.

This spec makes `trusty-mpm` a **standalone, project-aliased driver** that runs Claude Code against
any GitHub repo using a fully isolated **managed configuration**, so it never collides with the
user's own setup and can replace claude-mpm for daily work. Direct `tm` use is **always attended**;
autonomy is the **session manager's** job, not `tm`'s (the two-layer model, §3). In scope: the alias
registry, the `register/load/run/path/ls/update/rm` lifecycle, the Managed Configuration Standard
(project-local CLAUDE.md/agents/skills + argument-supplied hooks/MCPs) and its isolation invariant,
the attended `tm run` contract, and the IDE-attach contract.

**Out of scope** (consumed, not re-specified): the session lifecycle and `SessionRecord` (DOC-14);
the provisioner clone mechanics (`WorkspaceProvisioner`); the harness runner (DOC-17); the NL→repo
resolver and multi-project Telegram UX (DOC-22); the autonomy tiers and decision adjudication
(DOC-23); the trusty-memory / trusty-search / trusty-review daemons themselves (wired, not
implemented here).

## Table of Contents

| ID | Section | Implementing module(s) |
|----|---------|--------------------------|
| SPEC-STANDALONE-MPM-01~draft | [Alias Registry & `register`](#alias-registry--register-spec-standalone-mpm-01draft) | `tm::commands::register`, project registry |
| SPEC-STANDALONE-MPM-02~draft | [`load` — clone + generate managed config (idempotent)](#load--clone--generate-managed-config-idempotent-spec-standalone-mpm-02draft) | `provisioner::workspace`, `core::session_launch` |
| SPEC-STANDALONE-MPM-03~draft | [The Managed Configuration Standard](#the-managed-configuration-standard-spec-standalone-mpm-03draft) | `core::session_launch::settings`, `core::managed_config` (new) |
| SPEC-STANDALONE-MPM-04~draft | [Isolation Invariant — no global `~/.claude` writes](#isolation-invariant--no-global-claude-writes-spec-standalone-mpm-04draft) | `core::agent_deployer`, `core::skill_deployer`, `core::session_launch::settings` |
| SPEC-STANDALONE-MPM-05~draft | [`run` — attended-only (autonomy lives in the session manager)](#run--attended-only-autonomy-lives-in-the-session-manager-spec-standalone-mpm-05draft) | `tm::commands::run`, `core::session_launch` |
| SPEC-STANDALONE-MPM-06~draft | [IDE Attach — stable, discoverable project dir](#ide-attach--stable-discoverable-project-dir-spec-standalone-mpm-06draft) | `tm::commands::path`, managed-config layout |
| SPEC-STANDALONE-MPM-07~draft | [Supporting commands — `path` / `ls` / `update` / `rm`](#supporting-commands--path--ls--update--rm-spec-standalone-mpm-07draft) | `tm::commands::{path,ls,update,rm}` |
| SPEC-STANDALONE-MPM-08~draft | [Per-project MCP wiring (memory / search / review)](#per-project-mcp-wiring-memory--search--review-spec-standalone-mpm-08draft) | `core::session_launch::settings` (inject_* ) |

---

## 1. Motivation & Current State

### 1.1 The collision (current state, verified)

`prepare_session` (`crates/trusty-mpm/src/core/session_launch/mod.rs`) is the hot path behind
`tm launch`, `tm sessions start`, and managed provisioning. On **every** invocation it writes both
project-local and **global** state. The global writes are the collision this spec eliminates:

| # | Global write site (current) | File | Function |
|---|------------------------------|------|----------|
| 1 | Composed agents deployed globally | `~/.claude/agents/<name>.md` (+ `.trusty-mpm-manifest.json`) | `deploy_agents_filtered` (`agent_deployer.rs`, via `mod.rs`), target `FrameworkPaths::claude_agents_dir()` over `dirs::home_dir()` |
| 2 | Skills deployed globally | `~/.claude/skills/<name>/SKILL.md` (+ manifest) | `deploy_skills_filtered` (`skill_deployer.rs`), dest `FrameworkPaths::claude_skills_dir()` |
| 3 | Output styles overwritten globally | `~/.claude/output-styles/<name>.md` | `deploy_output_style` (`settings.rs`), always overwrites |
| 4 | Global hooks mutated | `~/.claude/settings.json` | `remove_global_trusty_memory_hooks` → `clean_global_trusty_memory_hooks` (`settings.rs`) |
| 5 | Per-directory trust seeded globally | `~/.claude.json` (`projects.<abs-path>`) | `preseed_workspace_trust_home` → `preseed_workspace_trust` (`settings.rs`) |

The **project-local** writes already exist and are correct in spirit — the managed standard formalizes
and extends them:

| # | Project-local write (current) | File | Function |
|---|-------------------------------|------|----------|
| 6 | Output-style / spinner / hooks | `<project>/.claude/settings.json` | `write_output_style`, `write_project_hooks` |
| 7 | Project MCP servers | `<project>/.mcp.json` (`trusty-memory`, `trusty-search`) | `inject_trusty_memory_mcp`, `inject_trusty_search_mcp` |
| 8 | Inspectable launch prompt | `<project>/.trusty-mpm/last-instructions.md` | `prepare_session` |
| 9 | Project instructions | `<project>/CLAUDE.md` (if absent) | `prepare_session` |

There is **no** existing "managed configuration" standard or spec (verified: no `docs/specs/` file,
no named type/constant/module enforces an isolation invariant; the only incidental hit is the prose
"framework-managed config" in `src/daemon/state/core.rs`).

### 1.2 The command surface (current state, verified)

The `trusty-mpm` binary (`tm`) exposes a project registry (`tm project init/list/info`) and a rich
session surface (`tm sessions new <repo> --task …`, `tm sessions decommission`, `tm launch`,
`tm connect`, `tm attach`, …). There is **no** alias-keyed `register` / `load` / `run` / `path` /
`update` / `rm <alias>` lexicon. This spec adds that standalone-driver lexicon as the new delta,
reusing the provisioner, session-launch, and registry machinery underneath.

---

## 2. Behavior Contract Sections

### Alias Registry & `register` {#SPEC-STANDALONE-MPM-01~draft}

**ID:** SPEC-STANDALONE-MPM-01~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** `tm register <alias> <github-url>`. `<alias>` is a short stable identifier
  (`^[a-z0-9][a-z0-9._-]*$`); `<github-url>` is any clone-able GitHub URL (https or ssh form).
- **Outputs:** the `(alias → {url, default_branch?, created_at})` mapping is persisted to the
  standalone-driver registry (a TOML/JSON file under the trusty-mpm config root, e.g.
  `~/.trusty-mpm/registry.toml`, distinct from `~/.claude*`). Prints a one-line confirmation
  (`registered <alias> → <url>`). **No clone occurs.**
- **Preconditions:** none beyond a writable config root. The URL is **not** required to be reachable
  at register time (resolution is deferred to `load`).
- **Postconditions:** `tm ls` lists the alias; `tm load <alias>` and the other lifecycle verbs can
  resolve it. Registering an existing alias with the **same** URL is a no-op success; with a
  **different** URL it fails unless `--force` is passed (which rewrites the mapping and leaves any
  already-cloned workspace untouched until the next `load`/`update`).
- **Error conditions:** invalid alias syntax → non-zero exit + diagnostic; unwritable registry →
  non-zero exit; conflicting alias without `--force` → non-zero exit naming the existing URL. All
  diagnostics go to stderr; stdout stays clean.

#### Rationale (WHY)

A name-first registry decouples *naming* a repo from the cost of cloning and config generation, so a
user can declare their whole fleet cheaply and `load` lazily. Keying by alias (not bare UUID or path)
gives the standalone driver a human handle for IDE attach and for the `run`/`path`/`update`/`rm`
verbs. The registry is a thin alias layer over — and is kept consistent with — the DOC-22 named-project
registry, but lives in the trusty-mpm config root, **never** in `~/.claude*`, to honor the isolation
invariant (§SPEC-STANDALONE-MPM-04).

#### Implementing Modules

| Module | Role |
|--------|------|
| `tm::commands::register` (new, `src/bin/tm/commands/`) | Parses args, validates alias, writes registry entry. |
| standalone-driver registry (new, under trusty-mpm config root) | Persists `alias → {url, branch, created_at}`; read by all lifecycle verbs. |

---

### `load` — clone + generate managed config (idempotent) {#SPEC-STANDALONE-MPM-02~draft}

**ID:** SPEC-STANDALONE-MPM-02~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** `tm load <alias>` (alias must be registered). Optional `--ref <git-ref>` to pin a
  branch/tag/sha (defaults to the repo default branch).
- **Outputs:** clones the alias's URL into a **stable, persistent, alias-keyed** project directory
  (the *managed project dir*; layout in §SPEC-STANDALONE-MPM-03), then generates the full managed
  configuration: project-local `.claude/`, `.mcp.json`, `CLAUDE.md`, and deploys agents/skills/trust
  into the **per-project** `CLAUDE_CONFIG_DIR` (never global). Prints the project dir path on success.
- **Preconditions:** alias registered; network access to clone; writable managed-projects root.
- **Postconditions:** the managed project dir exists, is a valid git checkout at `--ref`, and contains
  a complete managed configuration that satisfies the isolation invariant. **Idempotent:** re-running
  `load` on an already-loaded alias is safe — it does **not** re-clone destructively, it `git fetch`es
  and fast-forwards the checkout if clean, regenerates/refreshes instructions, agents, skills, and
  config in place, and leaves user edits in tracked source files alone. A re-run updates
  instructions/agents to the current framework bundle.
- **Error conditions:** unregistered alias → non-zero exit; clone failure (auth, network, bad URL) →
  non-zero exit with git's message surfaced; dirty checkout that blocks fast-forward → non-zero exit
  advising `tm update --force` or manual resolution; partial config generation → the command fails
  atomically where possible (temp-write-then-rename) and reports which step failed.

#### Rationale (WHY)

`load` is the single idempotent entry point that turns a name into a ready-to-drive, isolated
project. Making it idempotent (clone-once, refresh-many) lets it double as "bring this project up to
date with the current framework" without a separate first-run/Nth-run code path, and lets `run`
call it unconditionally (§SPEC-STANDALONE-MPM-05). The stable alias-keyed directory (not a
session-scoped temp dir) is what makes IDE attach (§SPEC-STANDALONE-MPM-06) and persistent daily
work possible. It reuses `WorkspaceProvisioner::provision_in` (caller-supplied project dir) rather
than the session-scoped `provision` so the directory is durable.

#### Implementing Modules

| Module | Role |
|--------|------|
| `tm::commands::load` (new) | Orchestrates resolve → clone/fetch → config generation; idempotent. |
| `provisioner::workspace::WorkspaceProvisioner::provision_in` | Clones into the caller-supplied stable project dir. |
| `core::session_launch::prepare_session` (managed-mode variant) | Generates project-local config + per-project CLAUDE_CONFIG_DIR deploy. |

---

### The Managed Configuration Standard {#SPEC-STANDALONE-MPM-03~draft}

**ID:** SPEC-STANDALONE-MPM-03~draft
**Status:** Draft

#### Behavior Contract (WHAT)

The Managed Configuration is the complete, isolated config that trusty-mpm owns and writes for a
loaded alias. It splits the five Claude Code customization surfaces into **two halves by mechanism**:

- **Static, project-local half — picked up by standard Claude Code discovery.** `CLAUDE.md`,
  `agents/`, and `skills/` live **inside the checkout** under `repo/` (`repo/CLAUDE.md`,
  `repo/.claude/agents/`, `repo/.claude/skills/`). Any process started in `repo/` — `tm`, a plain
  `claude`, or the IDE extension — discovers them automatically; no flags required.
- **Dynamic, argument-supplied half — NOT in any discovery location.** `hooks` and `MCPs` are
  **not** written to `repo/.claude/settings.json` or `repo/.mcp.json`. They are emitted into
  tm-private config files under `.trusty-mpm/` and applied **only** when `tm` launches Claude Code,
  via explicit launch arguments (verified Claude Code CLI flags, v2.1.185):
  - `--settings <tm-settings.json>` — loads a settings file whose `hooks` block carries tm's hooks
    (the same file may also carry trust / output-style keys, so no `~/.claude*` write is needed);
  - `--mcp-config <tm-mcp.json>` — loads tm's MCP servers from a JSON file;
  - `--strict-mcp-config` (optional) — restricts the session to **only** the `--mcp-config` servers,
    ignoring any other MCP configuration, so tm's MCP set is hermetic.

  Because hooks/MCPs ride on launch arguments (not discovery), they apply **only to tm-launched
  sessions** — and a tm session is **exactly reproducible** by invoking `claude` with the same
  `--settings`/`--mcp-config` arguments.

It is defined by four parts:

**(a) Per-project directory layout.** A managed project dir, alias-keyed and stable:

```
<managed-root>/<alias>/                  # e.g. ~/.trusty-mpm/projects/<alias>/  (or ~/trusty-mpm-projects/<alias>/)
├── repo/                                 # the git checkout (clone target; what the IDE opens)
│   ├── .claude/
│   │   ├── agents/<name>.md             # deployed agents — STANDARD DISCOVERY (IDE/plain-claude inherit)
│   │   ├── skills/<name>/SKILL.md       # deployed skills — STANDARD DISCOVERY (+ manifests)
│   │   ├── settings.json                # outputStyle, spinnerTips only (NO hooks here — see below)
│   │   └── settings.local.json          # user-owned local overrides (trusty-mpm never clobbers)
│   └── CLAUDE.md                         # project instructions — STANDARD DISCOVERY (created if absent)
├── .trusty-mpm/                          # tm-PRIVATE; supplied via launch ARGUMENTS, not discovery
│   ├── tm-settings.json                 # hooks block → passed as `--settings` ONLY by tm
│   ├── tm-mcp.json                       # MCP servers (memory/search/review) → passed as `--mcp-config` ONLY by tm
│   ├── last-instructions.md             # inspectable composed launch prompt
│   └── managed.toml                     # marker: alias, url, ref, generated-by version, args-config paths
└── claude-config/                        # OPTIONAL secondary CLAUDE_CONFIG_DIR (see (c)) —
    └── …                                 #   only to fence GLOBAL trust/output-style writes, NOT load-bearing
```

**(b) Ownership.** trusty-mpm **owns and may overwrite**: `repo/.claude/agents/` and
`repo/.claude/skills/` (deployed framework bundle), `repo/.claude/settings.json` (managed
**non-hook** keys only — outputStyle, spinnerTips), `repo/CLAUDE.md` (refreshed but preserves
user-added sections where feasible), and everything under `.trusty-mpm/` (including the
argument-supplied `tm-settings.json` / `tm-mcp.json`) and any `claude-config/`. trusty-mpm **never**
clobbers `repo/.claude/settings.local.json` or user source files in `repo/`.

**(c) Argument-supplied config (the load-bearing mechanism) + optional `CLAUDE_CONFIG_DIR`.** The
mechanism that makes hooks/MCPs tm-only is the **launch arguments**, not an env var: `tm` invokes
`claude --settings .trusty-mpm/tm-settings.json --mcp-config .trusty-mpm/tm-mcp.json
[--strict-mcp-config]` in `repo/`. `CLAUDE_CONFIG_DIR=<managed-root>/<alias>/claude-config` MAY be
set **secondarily and optionally**, narrowly to prevent any residual *global* writes (e.g. trust into
`~/.claude.json`, global output-styles) from escaping to the user's home — but the spec does **not**
depend on it for the IDE-visible config or for hooks/MCPs (it is undocumented per A1 and ignored by
the IDE extension per A3). See §SPEC-STANDALONE-MPM-04 for the invariant and §6 for the
optionality note.

**(d) The marker file.** `.trusty-mpm/managed.toml` records `{alias, url, ref, settings_arg_path,
mcp_config_arg_path, config_dir?, generated_by_version}` so `path`/`ls`/`update`/`rm` and an
attaching IDE can discover the managed project — and so a human can replay the exact `tm` launch
arguments — deterministically.

**Config model contrast (claude-mpm vs trusty-mpm).** claude-mpm uses **standard discovery for all
five** customization surfaces: running `claude` in a source dir auto-discovers CLAUDE.md + agents +
skills + hooks + MCPs from standard locations, with no custom config file. trusty-mpm uses **standard
discovery for the three static surfaces** (CLAUDE.md + agents + skills, which therefore an IDE or a
plain `claude` in `repo/` **do** inherit) **plus argument-supplied config files for the two dynamic
surfaces** (hooks via `--settings`, MCPs via `--mcp-config`, which are applied **only** when `tm`
launches the session). The practical consequence: a tm session can be **reproduced exactly** by
invoking `claude` with the same `--settings`/`--mcp-config` arguments — and conversely, a session
**not** launched through those arguments gets the static config but **none** of tm's hooks/MCPs.

- **Inputs:** an alias + its resolved checkout (from `load`).
- **Outputs:** the directory tree above, fully populated and isolation-compliant: static
  CLAUDE.md/agents/skills under `repo/` (discoverable); hooks/MCPs in `.trusty-mpm/tm-settings.json`
  and `.trusty-mpm/tm-mcp.json` (argument-supplied, not discoverable).
- **Preconditions:** a valid checkout under `repo/`.
- **Postconditions:** the layout is complete; the marker file records the `--settings` / `--mcp-config`
  argument paths; the static half is discoverable in `repo/`; the dynamic half exists only as
  tm-private argument files; no global `~/.claude*` path was written (§SPEC-STANDALONE-MPM-04).
- **Error conditions:** any failed write fails the generation step and reports the offending path;
  a pre-existing non-managed dir at `<managed-root>/<alias>` without a marker file → refuse and
  advise `tm rm`/`--force`.

#### Rationale (WHY)

Splitting the five surfaces *by mechanism* — static (CLAUDE.md/agents/skills) into `repo/` for
standard discovery, dynamic (hooks/MCPs) into argument-supplied files applied only at `tm` launch —
is what makes the driver simultaneously *IDE-attachable* (the IDE opens `repo/` and inherits the
static half with no flags) and *isolated/replicable* (hooks/MCPs apply only to tm-launched sessions
and can be reproduced by re-passing the same `--settings`/`--mcp-config` arguments). This is the
load-bearing design choice; the optional `CLAUDE_CONFIG_DIR` is a secondary fence for residual global
writes only (§04, §6), deliberately **not** the central mechanism — because it is undocumented (A1)
and the IDE extension ignores it (A3), the spec must not rest the IDE-visible config or hooks/MCPs on
it. The marker file (which records the argument paths) gives every other verb a single source of
truth and makes the standard auditable: a test can assert the tree's shape, that nothing escaped it,
and that a replayed `claude --settings … --mcp-config …` reproduces the session. Splitting managed vs
user-owned files (settings.json vs settings.local.json) preserves the user's ability to edit while
letting `update` refresh framework artifacts safely.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::managed_config` (new) | Defines the layout, marker schema (incl. argument paths), ownership rules; entry point for generation + discovery. |
| `core::session_launch::settings` | Deploys static agents/skills into `repo/.claude/`; writes the argument-supplied `.trusty-mpm/tm-settings.json` (hooks) and `.trusty-mpm/tm-mcp.json` (MCPs). |
| `core::session_launch::prepare_session` | Composes instructions + drives the static deploy + emits the launch-argument config files. |

---

### Isolation Invariant — no global `~/.claude` writes {#SPEC-STANDALONE-MPM-04~draft}

**ID:** SPEC-STANDALONE-MPM-04~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** any managed/standalone-mode operation (`load`, `run`, `update`).
- **Outputs:** all agent/skill/output-style/trust/hook/MCP configuration is written **only** to the
  project-local `repo/.claude/` (static: agents/skills, non-hook settings), the tm-private
  argument-supplied files under `.trusty-mpm/` (dynamic: hooks → `tm-settings.json`, MCPs →
  `tm-mcp.json`), and/or the **optional** per-project `claude-config/` (residual trust/output-style
  fence only).
- **Preconditions:** the driver is in managed mode (the default for `register/load/run`).
- **Postconditions — the invariant:** for the entire duration of a managed operation, trusty-mpm
  writes **nothing** under `~/.claude/` (no `agents/`, `skills/`, `output-styles/`, `settings.json`)
  and **nothing** to `~/.claude.json`. Specifically, the five current global write sites (§1.1) are,
  in managed mode, re-targeted as follows:
  1. `deploy_agents_filtered` target → `repo/.claude/agents/` (project-local, standard discovery).
  2. `deploy_skills_filtered` dest → `repo/.claude/skills/` (project-local, standard discovery).
  3. `deploy_output_style` home → `repo/.claude/settings.json` (`outputStyle` key) or the optional
     `<config_dir>/output-styles/`, **never** `~/.claude/output-styles/`.
  4. `remove_global_trusty_memory_hooks` / hook composition → emits the `hooks` block into the
     argument-supplied `.trusty-mpm/tm-settings.json` (passed via `--settings`), **never**
     `~/.claude/settings.json`.
  5. `preseed_workspace_trust_home` → seeds trust either in the argument-supplied settings file or
     the optional config-dir's `.claude.json` equivalent, **never** `~/.claude.json`.
- **Error conditions:** if re-targeting cannot be honored (e.g. a deployer is hardwired to
  `dirs::home_dir()`), the operation **must fail closed** rather than silently fall back to a global
  write. A managed operation that would touch a `~/.claude*` path is a contract violation.

#### Rationale (WHY)

This is the core promise of the standalone driver: a user's own claude-mpm `~/.claude` setup must be
untouched so the driver can run concurrently and *replace* it for daily work without fear. Fail-closed
(not fall-back-to-global) is mandatory because a silent global write is exactly the collision we are
eliminating — a degraded-but-isolated failure is acceptable; a working-but-global one is not. The
invariant is mechanically testable (assert no writes outside `<managed-root>`), which is why it is its
own ID and gets a dedicated integration-test work item (WI-7).

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::agent_deployer::deploy_agents_filtered` | Accept an explicit target dir → `repo/.claude/agents/`; no `home_dir()` fallback in managed mode. |
| `core::skill_deployer::deploy_skills_filtered` | Same: explicit dest → `repo/.claude/skills/`, no global fallback. |
| `core::session_launch::settings` | Emit hooks into `.trusty-mpm/tm-settings.json` (for `--settings`); re-target output-style/trust to `repo/.claude/settings.json` or the optional config-dir. |
| `core::managed_config` (new) | Supplies the argument-config paths (+ optional config-dir) and enforces the fail-closed guard. |

---

### `run` — attended-only (autonomy lives in the session manager) {#SPEC-STANDALONE-MPM-05~draft}

**ID:** SPEC-STANDALONE-MPM-05~draft
**Status:** Draft

#### The two-layer model

Autonomy does **not** live in `tm`. There are two distinct layers:

- **Layer 1 — `tm register` / `load` / `run` (this spec's command surface): standalone, attended,
  config-isolated runner.** This is the **claude-mpm replacement**. `tm run <alias>` invoked directly
  is **always attended/interactive** — the user drives the session. There is **no `--autonomous`
  flag** on `tm run`, and no task-dispatch-for-autonomy framing on the `tm` surface.
- **Layer 2 — the session manager (the durable tmux fleet daemon): the semi-autonomous orchestrator.**
  When autonomy is wanted, **the session manager** invokes `tm`/`claude` against a tm-managed project
  and **supervises** it (spawn → observe → auto-answer → supervise), governed by the DOC-23
  learned-autonomy tier. **The session manager IS the semi-autonomous driver.** All
  autonomous/task-dispatch behaviors belong to Layer 2, not to `tm`.

This spec specifies Layer 1 (attended `tm run`). Layer 2's autonomy is consumed from DOC-23 / the
session-manager spec and is **out of scope** here except for the seam: a managed project produced by
`load` is exactly what the session manager drives.

#### Behavior Contract (WHAT)

- **Inputs:** `tm run <alias> [--task <t>]`. (No `--autonomous` flag — `tm run` is attended-only.)
- **Outputs:** ensures the alias is loaded (calls `load` if needed; idempotent), then launches Claude
  Code **attended/interactive** in `repo/` with the managed configuration — passing the dynamic
  config via launch arguments `--settings .trusty-mpm/tm-settings.json --mcp-config
  .trusty-mpm/tm-mcp.json [--strict-mcp-config]` (the static CLAUDE.md/agents/skills come from
  standard discovery in `repo/`), optionally with `CLAUDE_CONFIG_DIR=<config_dir>` set as the
  secondary global-write fence (§03(c), §04). The user drives the Claude Code session directly (this
  is the claude-mpm replacement); `--task` may pre-seed the first prompt but the user retains
  control; the process is foregrounded / attachable.
- **Preconditions:** alias registered.
- **Postconditions:** an **attended, interactive** managed Claude Code session is running for the
  alias with the isolation invariant intact and tm's hooks/MCPs applied via the launch arguments.
- **Error conditions:** unregistered alias → non-zero exit; load failure → propagates §02 errors.

#### Rationale (WHY)

`tm run` is attended-by-definition because the safe, interactive daily-driver path is the *only* path
`tm` itself offers — autonomy is a property of the **orchestrator that drives `tm`** (the session
manager, Layer 2), not of `tm`. Keeping the layers separate prevents surprise unattended
commits/pushes from a hand-run `tm` and gives a single "just run it" interactive verb (`run` calls
`load` unconditionally, leaning on §02 idempotency). Because the session is launched with explicit
`--settings`/`--mcp-config` arguments, it is also exactly reproducible (§03).

#### Implementing Modules

| Module | Role |
|--------|------|
| `tm::commands::run` (new) | Resolves alias, ensures load, launches an **attended** Claude Code session with the `--settings` / `--mcp-config` launch arguments. |
| `core::session_launch` | Launches Claude Code with the argument-supplied config (+ optional `CLAUDE_CONFIG_DIR`) for an interactive session. |
| Session manager (Layer 2, consumed; DOC-23) | The semi-autonomous orchestrator that *drives* tm-managed projects; **not** implemented by `tm run`. |

---

### IDE Attach — stable, discoverable project dir {#SPEC-STANDALONE-MPM-06~draft}

**ID:** SPEC-STANDALONE-MPM-06~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** `tm path <alias>` (and the on-disk managed layout from §03).
- **Outputs:** prints the absolute path to the alias's `repo/` directory — the directory an IDE
  (VS Code / Cursor) or a hand-launched `claude` should open.
- **Preconditions:** alias loaded (a managed dir with a marker file exists).
- **Postconditions — the IDE-attach contract:** opening `repo/` in an IDE or running a plain `claude`
  in it inherits, **via standard project-local discovery**, the **static** half of the managed
  configuration — `repo/CLAUDE.md`, `repo/.claude/agents/`, `repo/.claude/skills/`, and the non-hook
  `repo/.claude/settings.json` — because those take precedence over the user's `~/.claude` per Claude
  Code's settings hierarchy (Managed > CLI args > Local > Project > User). It does **NOT** inherit
  tm's **hooks or MCPs**: those are supplied only via tm's launch arguments
  (`--settings .trusty-mpm/tm-settings.json`, `--mcp-config .trusty-mpm/tm-mcp.json`) and are **not**
  placed in any discovery location. To get tm's hooks/MCPs, the session must be launched via `tm`
  (or by passing the same `--settings`/`--mcp-config` arguments to `claude`). Sessions are
  **read-write**: the user and IDE may edit source and `settings.local.json` freely.
- **Error conditions:** alias not loaded → non-zero exit advising `tm load <alias>`; missing/corrupt
  marker → non-zero exit.

#### Rationale (WHY)

Project-local `CLAUDE.md` + `.claude/agents/` + `.claude/skills/` are the **portable, IDE-honored,
statically-discovered** half of the managed config — they ride with the checkout and are respected by
both the CLI and the VS Code extension. A stable, alias-keyed, discoverable `repo/` path is therefore
the contract that makes IDE attach first-class for the static half: the user just opens `repo/`. The
**dynamic** half (hooks/MCPs) is deliberately *not* IDE-inherited — it is argument-supplied so it
applies only to tm-launched sessions; this is the price of making tm sessions reproducible and
isolated, and the contract states it explicitly so the IDE/`tm` boundary is unambiguous (it does not
rely on `CLAUDE_CONFIG_DIR`, which the extension ignores — see A3/§6). Read-write (not read-only)
sessions are required for the driver to be a real daily tool.

#### Implementing Modules

| Module | Role |
|--------|------|
| `tm::commands::path` (new) | Resolves alias → marker → prints `repo/` absolute path. |
| `core::managed_config` (new) | Provides discovery via the marker file. |

---

### Supporting commands — `path` / `ls` / `update` / `rm` {#SPEC-STANDALONE-MPM-07~draft}

**ID:** SPEC-STANDALONE-MPM-07~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **`tm ls [--json]`:** lists registered aliases with `{alias, url, ref, loaded?, repo_path}`. Output
  to stdout; `--json` for machine consumption. Never mutates.
- **`tm update <alias> [--force]`:** `git fetch` + fast-forward the checkout **and** regenerate the
  managed config (agents/skills/output-styles/instructions to the current framework bundle), in place,
  isolation-invariant intact. `--force` allows resetting a dirty checkout (stashing or discarding per
  flag semantics, documented at impl). Idempotent; equivalent to the refresh half of `load`.
- **`tm rm <alias> [--purge]`:** deregisters the alias. Without `--purge`, leaves the managed project
  dir on disk (re-`register`+`load` re-adopts it). With `--purge`, removes the managed project dir
  (the checkout + `claude-config/` + `.trusty-mpm/`) after confirmation. Never touches `~/.claude*`.
- **`tm path <alias>`:** defined in §SPEC-STANDALONE-MPM-06 (cross-referenced; the resolver is shared).
- **Inputs/Outputs/Pre/Post/Errors:** each verb resolves the registry + marker; mutating verbs
  (`update`, `rm`) fail closed on a missing/corrupt marker rather than guessing; `ls` tolerates
  partially-loaded aliases (reports `loaded:false`). All diagnostics to stderr.

#### Rationale (WHY)

These verbs complete the lifecycle so the driver is operable day-to-day without manual git or
filesystem surgery: `ls` for discovery, `update` for staying current, `rm` for teardown with an
explicit purge gate so deregistration is cheap and reversible by default. Keeping `rm` non-purging by
default avoids accidental loss of local work; making `update` share `load`'s refresh path avoids a
second drift-prone code path.

#### Implementing Modules

| Module | Role |
|--------|------|
| `tm::commands::ls` / `update` / `rm` / `path` (new) | The four supporting verbs over the registry + marker. |
| standalone-driver registry + `core::managed_config` | Shared resolution and discovery. |

---

### Per-project MCP wiring (memory / search / review) {#SPEC-STANDALONE-MPM-08~draft}

**ID:** SPEC-STANDALONE-MPM-08~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** a loaded alias's tm-private `.trusty-mpm/tm-mcp.json` (created/merged during
  `load`/`update`), which `tm` passes via `--mcp-config` at launch.
- **Outputs:** the argument-supplied `.trusty-mpm/tm-mcp.json` declares the trusty MCP servers for
  this project: `trusty-memory` (`serve --stdio`), `trusty-search` (`serve`, optionally
  `--index <id>` pinned to the project), and `trusty-review` (`review` stdio adapter) — each as a
  `stdio` server entry. `tm` launches with `--mcp-config .trusty-mpm/tm-mcp.json` (optionally
  `--strict-mcp-config` to make tm's set hermetic), so the managed session picks them up **only when
  launched by `tm`** — **not** from a discovery-path `repo/.mcp.json` and **without** any global
  `~/.claude.json` trust seed.
- **Preconditions:** a managed project dir exists.
- **Postconditions:** the three trusty servers are wired into the argument file and applied to the
  tm-launched session; entries are **merged idempotently** (re-running `load`/`update` does not
  duplicate or clobber user-added MCP servers). No global MCP/trust state is written; no discovery-path
  `repo/.mcp.json` is required.
- **Error conditions:** malformed pre-existing `.trusty-mpm/tm-mcp.json` → leave untouched and report
  (mirrors the current safety stance of `preseed_workspace_trust`); a server binary absent on PATH →
  wire the entry anyway (runtime concern) but surface a `tm doctor`-style warning.

#### Rationale (WHY)

Supplying MCPs via the `--mcp-config` launch argument (vs the current global `~/.claude.json` trust
seed, and vs a discovery-path `repo/.mcp.json`) is what lets each managed project apply exactly its
trusty servers **only to tm-launched sessions** in isolation, honoring §SPEC-STANDALONE-MPM-04 and
keeping the IDE/plain-`claude` path free of tm's MCPs (§06). It also removes the last global write and
makes the MCP set reproducible: re-passing the same `--mcp-config` argument replays it. Optional
`--strict-mcp-config` guarantees the session sees *only* tm's servers. Adding `trusty-review`
alongside memory/search rounds out the trusty triad. Idempotent merge preserves any MCP servers the
user adds by hand to the argument file.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::session_launch::settings::inject_trusty_memory_mcp` / `inject_trusty_search_mcp` (+ new `inject_trusty_review_mcp`) | Merge server entries into `.trusty-mpm/tm-mcp.json` (the `--mcp-config` argument file). |
| `core::session_launch::settings` (launch args) | Pass `--mcp-config .trusty-mpm/tm-mcp.json` (optionally `--strict-mcp-config`) at `tm` launch instead of seeding `~/.claude.json` or a discovery `.mcp.json`. |

---

## 3. Interaction Model (summary) — the two-layer model

Autonomy lives in the **session manager**, **not** in `tm`. `tm` invoked directly is **always
attended**; there is **no `--autonomous` flag** on `tm run`.

| Layer | Surface | Trigger | Who drives | Decision handling |
|-------|---------|---------|-----------|-------------------|
| **Layer 1 — `tm` (this spec)** | `tm register` / `load` / `run` | `tm run <alias> [--task <t>]` | The user, interactively (attended-only) | User answers all prompts; `tm` never auto-answers |
| **Layer 2 — session manager** | the durable tmux fleet daemon | the session manager spawns `tm`/`claude` against a tm-managed project | The session manager, semi-autonomously (spawn → observe → auto-answer → supervise) | Auto-answered per the DOC-23 learned-autonomy tier |

Layer 1 (`tm`) is the **claude-mpm replacement**: a trustworthy, attended, config-isolated
interactive runner. Layer 2 (the **session manager**) is the **semi-autonomous orchestrator** that
drives tm-managed projects and supervises them under DOC-23. All autonomous / task-dispatch behavior
belongs to Layer 2 and is **out of scope** for this spec (consumed via the session-manager / DOC-23
specs); a managed project produced by `load` is exactly the seam the session manager drives.

## 4. Assumptions & Risks

| # | Assumption / Risk | Status | Mitigation / WI |
|---|-------------------|--------|-----------------|
| A1 | **`CLAUDE_CONFIG_DIR` redirects Claude Code's global config dir per-process.** Confirmed for the **CLI** (Claude Code v1.0.30+ supports it; behaves like `XDG_CONFIG_HOME`). **BUT it is officially undocumented** (not in `--help` or docs; multiple open issues request documentation). **De-emphasized:** the spec no longer rests the IDE-visible config or hooks/MCPs on it — those use the argument-supplied mechanism (A8). `CLAUDE_CONFIG_DIR` is now **optional/secondary**, used only to fence residual global trust/output-style writes. | **Secondary/optional; must still validate if used.** | **WI-1** validates the exact behavior against the pinned CC version *if* the optional fence is adopted; the load-bearing path no longer depends on it. |
| A2 | **`CLAUDE_CONFIG_DIR` does not suppress project-local `.claude/` creation.** Confirmed: a local `.claude/settings.local.json` may still be written in the workspace even with the var set. This is **acceptable** — the managed layout *wants* project-local files under `repo/`; we just must not assume the var centralizes everything. | **Confirmed (acceptable).** | Managed layout treats `repo/.claude/` as the statically-discovered half by design. |
| A3 | **The VS Code / Cursor extension IGNORES `CLAUDE_CONFIG_DIR`** (reads/writes `~/.claude/` regardless). | **Confirmed risk — now moot for the load-bearing path.** | IDE-attach (§06) relies **only** on standard project-local discovery of `repo/CLAUDE.md` + `repo/.claude/agents/` + `repo/.claude/skills/`, which the extension *does* honor — **not** on the per-project config-dir. tm's hooks/MCPs are intentionally **not** IDE-inherited (argument-supplied, §06/A8). WI-1 documents this boundary. |
| A4 | **Project-local `.claude/` precedence over user `~/.claude`.** Confirmed: precedence is Managed > CLI args > Local (`settings.local.json`) > Project (`.claude/`) > User (`~/.claude`); CLI args (`--settings` / `--mcp-config`) sit above project/user scope. | **Confirmed.** | Relied on by §03/§06/§08. |
| A8 | **Claude Code CLI accepts argument-supplied settings + MCP config files (the load-bearing mechanism).** **Verified** against the installed CLI (v2.1.185, `claude --help`): `--settings <file-or-json>` ("Path to a settings JSON file … to load additional settings from" — the file carries the `hooks` block); `--mcp-config <configs...>` ("Load MCP servers from JSON files or strings (space-separated)"); `--strict-mcp-config` ("Only use MCP servers from `--mcp-config`, ignoring all other MCP configurations"). `--bare`/`--safe-mode` help text confirms hooks/MCPs are otherwise standard-discovery customizations. | **Verified (CLI v2.1.185).** | §03/§05/§06/§08 use these flags as the canonical mechanism; WI-1 re-confirms against the pinned CC version. |
| A5 | **Deployers are re-targetable away from `dirs::home_dir()`.** The agent/skill deployers already accept a target/dest dir; output-style/hook/trust helpers resolve `home_dir()` internally and must be parameterized. | **Verified (re-targetable; some helpers need a param).** | WI-2/WI-3 thread the config-dir through; WI-7 asserts no global writes. Fail-closed if a helper cannot be re-targeted (§04). |
| A6 | Concurrent `load`/`update` on the same alias could race on the checkout/config. | Risk. | Per-alias advisory lock (file lock in the managed dir); document in impl. |
| A7 | A user already has a non-managed dir where the managed root wants to write. | Risk. | Marker-file guard (§03): refuse without `--force`. |

## 5. Work-Item (WI) Breakdown

> Scopes: **S** ≈ ≤1 day, **M** ≈ 2–4 days, **L** ≈ ≥1 week. To be filed as an epic for maintainer
> review; this spec is the behavior contract the epic's WIs implement.

| WI | Scope | Work | Realizes | Depends on |
|----|-------|------|----------|------------|
| **WI-1** | **M** | **Formalize + validate the Managed Configuration Standard.** Confirm the argument-supplied mechanism against the pinned Claude Code version (`--settings` for hooks, `--mcp-config` for MCPs, `--strict-mcp-config`; verified on v2.1.185, A8); validate the **optional** `CLAUDE_CONFIG_DIR` fence (CLI honors it; IDE ignores it) only if adopted; pin a minimum CC version; write the standard doc (layout, marker schema incl. argument paths, ownership, isolation invariant, the static-discovery vs argument-supplied split) and a conformance checklist incl. session-replay via re-passed args. | SPEC-STANDALONE-MPM-03, -04 (assumptions A1–A4, A8) | — |
| **WI-2** | **M** | **Project-local-scope the agent/skill/output-style deploys.** Thread an explicit target through `deploy_agents_filtered`, `deploy_skills_filtered` → `repo/.claude/agents/`, `repo/.claude/skills/` (standard discovery); `deploy_output_style` → `repo/.claude/settings.json` / optional config-dir; remove the `home_dir()` global fallback in managed mode (fail closed). | SPEC-STANDALONE-MPM-04 (sites 1–3) | WI-1 |
| **WI-3** | **S** | **Emit hooks/trust into the argument-supplied config + scope the trust-seed.** Re-target hook composition (`remove_global_trusty_memory_hooks` + the managed hooks) to emit the `hooks` block into `.trusty-mpm/tm-settings.json` (the `--settings` argument file); re-target `preseed_workspace_trust_home` to that file or the optional config-dir; **stop** seeding `~/.claude.json`. | SPEC-STANDALONE-MPM-04 (sites 4–5), -08 | WI-1 |
| **WI-4** | **M** | **Standalone-driver registry + `register`/`ls`.** New registry file under the trusty-mpm config root; `tm register`, `tm ls`. Reuse/align with the DOC-22 project registry. | SPEC-STANDALONE-MPM-01, -07 (`ls`) | — |
| **WI-5** | **L** | **`load` + managed-config generation.** Stable alias-keyed `provision_in` clone; full managed layout (`repo/` with statically-discovered CLAUDE.md/agents/skills + `.trusty-mpm/` argument files `tm-settings.json`/`tm-mcp.json` + optional `claude-config/` + marker recording the argument paths); idempotent refresh; `core::managed_config` module. | SPEC-STANDALONE-MPM-02, -03 | WI-1, WI-2, WI-3, WI-4 |
| **WI-6** | **M** | **Attended `run` + `path`/`update`/`rm`.** Attended-only `tm run` launch that passes `--settings .trusty-mpm/tm-settings.json --mcp-config .trusty-mpm/tm-mcp.json [--strict-mcp-config]` (optionally `CLAUDE_CONFIG_DIR` as the secondary fence); **no `--autonomous` flag** (autonomy belongs to Layer 2 / the session manager); the supporting verbs over the marker. | SPEC-STANDALONE-MPM-05, -06, -07 | WI-5 |
| **WI-7** | **M** | **Isolation-invariant integration tests.** Tests that run `load`/`run`/`update` against a sandboxed `$HOME` and assert **zero** writes outside `<managed-root>/<alias>` (no `~/.claude/`, no `~/.claude.json`); assert hooks/MCPs live only in the argument files and apply only when launched with the args (and that the static half is discoverable in `repo/`); idempotency tests for `load`/`update`; marker-guard + fail-closed tests. | SPEC-STANDALONE-MPM-04 (the invariant) | WI-5, WI-6 |
| **WI-8** | **S** | **`trusty-review` MCP wiring into the argument file.** Add `inject_trusty_review_mcp`; assemble the trusty triad into `.trusty-mpm/tm-mcp.json` passed via `--mcp-config`. | SPEC-STANDALONE-MPM-08 | WI-3, WI-5 |

**Critical path:** WI-1 → (WI-2 ∥ WI-3 ∥ WI-4) → WI-5 → WI-6 → WI-7. WI-8 rides alongside WI-5/WI-6.
**Parallelizable:** WI-2, WI-3, WI-4 after WI-1; WI-8 after WI-3.
**Out of scope (Layer 2):** session-manager-driven autonomy over tm-managed projects (DOC-23 /
session-manager spec) — `tm run` itself stays attended-only.

## 6. Open Questions / Future Work

1. **Managed root location.** `~/.trusty-mpm/projects/<alias>/` vs `~/trusty-mpm-projects/<alias>/`
   (more IDE-discoverable, outside a dotdir)? Pick one in WI-1; both honor the isolation invariant.
2. **`CLAUDE_CONFIG_DIR` longevity (now secondary).** It is undocumented (A1) and is no longer the
   load-bearing mechanism — hooks/MCPs ride on `--settings`/`--mcp-config` arguments (A8), and
   CLAUDE.md/agents/skills ride on standard discovery in `repo/`. `CLAUDE_CONFIG_DIR` is retained
   only as an **optional** fence for residual *global* writes (trust/output-style). If Anthropic
   changes/removes it, only that secondary fence is affected (fallback: write those into the
   argument-supplied settings file or a per-project `$HOME` shim). Track upstream issues; WI-1
   records the pinned-version behavior if the fence is adopted.
3. **IDE hooks/MCP parity (by design, not a gap).** An IDE-attached or plain-`claude` session in
   `repo/` inherits the static half (CLAUDE.md/agents/skills) via discovery but **deliberately not**
   tm's hooks/MCPs (those are argument-supplied and apply only to tm-launched sessions, §06). Is an
   IDE-friendly way to opt into tm's hooks/MCPs wanted (e.g. a documented "launch via `tm`" or a
   helper that prints the exact `--settings`/`--mcp-config` args), or is the tm-only contract
   sufficient? Decide in WI-1/WI-6.
4. **Alias ↔ DOC-22 project registry unification.** Should the standalone alias registry *be* the
   DOC-22 named-project registry, or a thin layer over it? WI-4 decides; prefer one store.
5. **Multi-checkout per alias.** One `repo/` per alias today. Worktree-per-task (multiple concurrent
   sessions on one alias) is future work; the marker schema should leave room for it.
6. **`update` dirty-checkout policy.** Exact `--force` semantics (stash vs discard vs refuse) — settle
   in WI-6.

## 7. References

- [DOC-14 — Session Manager Agent](./session-manager-agent.md) — `SessionRecord`, lifecycle, provisioning hooks.
- [DOC-17 — Harness Runner Vision](./harness-runner-vision.md) — autonomous-operation north-star, provisioner seam.
- [DOC-22 — Multi-Repo Session Routing](./multi-repo-session-routing.md) — named-project registry, NL→repo resolver (alias alignment).
- [DOC-23 — Learned-Autonomy Auto-Answer](./learned-autonomy-auto-answer.md) — the autonomy tier the **session manager** (Layer 2) uses to drive tm-managed projects; **not** a `tm` flag.
- `crates/trusty-mpm/src/provisioner/workspace.rs` — `WorkspaceProvisioner::provision` / `provision_in`.
- `crates/trusty-mpm/src/core/session_launch/mod.rs` — `prepare_session` (the global/local write hot path).
- `crates/trusty-mpm/src/core/session_launch/settings.rs` — `inject_trusty_memory_mcp`, `inject_trusty_search_mcp`, `write_output_style`, `write_project_hooks`, `preseed_workspace_trust_home`, `remove_global_trusty_memory_hooks`, `deploy_output_style`.
- `crates/trusty-mpm/src/core/agent_deployer.rs` — `deploy_agents_filtered` (global agent deploy site).
- `crates/trusty-mpm/src/core/skill_deployer.rs` — `deploy_skills_filtered` (global skill deploy site).
- `crates/trusty-mpm/src/core/claude_config.rs` — `ClaudeConfigPaths` / `ClaudeConfigReader` (path model).
- `crates/trusty-mpm/src/bin/tm/cli.rs`, `src/bin/tm/main.rs` — the `tm` command surface.
- Claude Code settings precedence (Managed > CLI > Local > Project > User) and standard discovery of CLAUDE.md/agents/skills/hooks/MCPs — Claude Code docs.
- **Verified Claude Code CLI flags (the argument-supplied mechanism), `claude --help`, v2.1.185:**
  `--settings <file-or-json>` ("Path to a settings JSON file … to load additional settings from"; carries the `hooks` block);
  `--mcp-config <configs...>` ("Load MCP servers from JSON files or strings (space-separated)");
  `--strict-mcp-config` ("Only use MCP servers from `--mcp-config`, ignoring all other MCP configurations");
  `--bare` / `--safe-mode` help text confirms hooks/MCPs are otherwise standard-discovery customizations.
- `CLAUDE_CONFIG_DIR` behavior + VS Code-extension exclusion — Claude Code GitHub issues #3833, #30538, #33430 (undocumented; CLI-only; now the **optional/secondary** fence, not the load-bearing mechanism).
