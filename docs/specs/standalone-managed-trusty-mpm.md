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
Auto-Answer (`docs/specs/learned-autonomy-auto-answer.md`, the autonomy tier that `--autonomous` opts
into).
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
> per-project directory layout, the files trusty-mpm owns, and the **isolation invariant** that in
> managed mode trusty-mpm writes **nothing** to the user's global `~/.claude/` or `~/.claude.json`;
> (3) the attended-default / autonomous-opt-in interaction model; (4) first-class IDE attach via stable,
> discoverable project dirs carrying project-local `.claude/` + `.mcp.json` + `CLAUDE.md`.
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
user's own setup and can replace claude-mpm for daily work. In scope: the alias registry, the
`register/load/run/path/ls/update/rm` lifecycle, the Managed Configuration Standard and its
isolation invariant, the attended-vs-autonomous interaction contract, and the IDE-attach contract.

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
| SPEC-STANDALONE-MPM-05~draft | [`run` — attended-default / autonomous-opt-in](#run--attended-default--autonomous-opt-in-spec-standalone-mpm-05draft) | `tm::commands::run`, `core::session_launch` |
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
loaded alias. It is defined by four parts:

**(a) Per-project directory layout.** A managed project dir, alias-keyed and stable:

```
<managed-root>/<alias>/                  # e.g. ~/.trusty-mpm/projects/<alias>/  (or ~/trusty-mpm-projects/<alias>/)
├── repo/                                 # the git checkout (clone target; what the IDE opens)
│   ├── .claude/
│   │   ├── settings.json                # outputStyle, spinnerTips, project hooks (trusty-managed)
│   │   └── settings.local.json          # user-owned local overrides (trusty-mpm never clobbers)
│   ├── .mcp.json                         # project-scoped MCP servers (memory/search/review)
│   └── CLAUDE.md                         # project instructions (created if absent; refreshed by load/update)
├── claude-config/                        # the per-project CLAUDE_CONFIG_DIR (isolated global-equivalent)
│   ├── agents/<name>.md                  # deployed agents (+ .trusty-mpm-manifest.json)
│   ├── skills/<name>/SKILL.md            # deployed skills (+ manifest)
│   ├── output-styles/<name>.md           # deployed output styles
│   ├── settings.json                     # managed-session hooks live here, NOT in ~/.claude
│   └── .claude.json-equivalent trust state for this project's checkout only
└── .trusty-mpm/
    ├── last-instructions.md              # inspectable composed launch prompt
    └── managed.toml                      # marker: alias, url, ref, generated-by version, config-dir path
```

**(b) Ownership.** trusty-mpm **owns and may overwrite**: `repo/.claude/settings.json` (managed keys
only — outputStyle, spinnerTips, hooks), `repo/.mcp.json` (managed server entries only, merged),
`repo/CLAUDE.md` (refreshed but preserves user-added sections where feasible), and everything under
`claude-config/` and `.trusty-mpm/`. trusty-mpm **never** clobbers `repo/.claude/settings.local.json`
or user source files in `repo/`.

**(c) The per-project `CLAUDE_CONFIG_DIR`.** Every managed Claude Code process launched for this
alias is invoked with `CLAUDE_CONFIG_DIR=<managed-root>/<alias>/claude-config`, so all
agent/skill/output-style/trust/global-hook state that would otherwise land in `~/.claude/` lands in
the per-project dir instead. (See §SPEC-STANDALONE-MPM-04 for the invariant and §6 for the
load-bearing assumption this rests on.)

**(d) The marker file.** `.trusty-mpm/managed.toml` records `{alias, url, ref, config_dir,
generated_by_version}` so `path`/`ls`/`update`/`rm` and an attaching IDE can discover the managed
project deterministically.

- **Inputs:** an alias + its resolved checkout (from `load`).
- **Outputs:** the directory tree above, fully populated and isolation-compliant.
- **Preconditions:** a valid checkout under `repo/`.
- **Postconditions:** the layout is complete; the marker file exists; the config-dir is populated;
  no global `~/.claude*` path was written (§SPEC-STANDALONE-MPM-04).
- **Error conditions:** any failed write fails the generation step and reports the offending path;
  a pre-existing non-managed dir at `<managed-root>/<alias>` without a marker file → refuse and
  advise `tm rm`/`--force`.

#### Rationale (WHY)

Centralizing every trusty-owned file under one alias-keyed root with an explicit `repo/` vs
`claude-config/` split is what makes the driver both *isolated* (the config-dir replaces global
`~/.claude`) and *IDE-attachable* (the IDE opens `repo/`, inherits project-local `.claude/` +
`.mcp.json` + `CLAUDE.md`). The marker file gives every other verb a single source of truth and
makes the standard auditable: a test can assert the tree's shape and that nothing escaped it.
Splitting managed vs user-owned files (settings.json vs settings.local.json) preserves the user's
ability to edit while letting `update` refresh framework artifacts safely.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::managed_config` (new) | Defines the layout, marker schema, ownership rules; entry point for generation + discovery. |
| `core::session_launch::settings` | Writes `repo/.claude/settings.json`, `repo/.mcp.json` (re-targeted to managed dir). |
| `core::session_launch::prepare_session` | Composes instructions + drives deploy into `claude-config/`. |

---

### Isolation Invariant — no global `~/.claude` writes {#SPEC-STANDALONE-MPM-04~draft}

**ID:** SPEC-STANDALONE-MPM-04~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** any managed/standalone-mode operation (`load`, `run`, `update`).
- **Outputs:** all agent/skill/output-style/trust/hook configuration is written **only** to the
  per-project `claude-config/` dir and/or the project-local `repo/.claude/` and `repo/.mcp.json`.
- **Preconditions:** the driver is in managed mode (the default for `register/load/run`).
- **Postconditions — the invariant:** for the entire duration of a managed operation, trusty-mpm
  writes **nothing** under `~/.claude/` (no `agents/`, `skills/`, `output-styles/`, `settings.json`)
  and **nothing** to `~/.claude.json`. Specifically, the five current global write sites (§1.1) are,
  in managed mode, re-targeted to the per-project `claude-config/`:
  1. `deploy_agents_filtered` target → `<config_dir>/agents/`.
  2. `deploy_skills_filtered` dest → `<config_dir>/skills/`.
  3. `deploy_output_style` home → `<config_dir>/output-styles/`.
  4. `remove_global_trusty_memory_hooks` → operates on `<config_dir>/settings.json`, never `~/.claude/settings.json`.
  5. `preseed_workspace_trust_home` → seeds trust in the per-project config-dir's `.claude.json`
     equivalent, never `~/.claude.json`.
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
| `core::agent_deployer::deploy_agents_filtered` | Accept an explicit target dir; no `home_dir()` fallback in managed mode. |
| `core::skill_deployer::deploy_skills_filtered` | Same: explicit dest, no global fallback. |
| `core::session_launch::settings` | Re-target `deploy_output_style` / hook-clean / trust-seed to the config-dir. |
| `core::managed_config` (new) | Supplies the config-dir path and enforces the fail-closed guard. |

---

### `run` — attended-default / autonomous-opt-in {#SPEC-STANDALONE-MPM-05~draft}

**ID:** SPEC-STANDALONE-MPM-05~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** `tm run <alias> [--task <t>] [--autonomous]`.
- **Outputs:** ensures the alias is loaded (calls `load` if needed; idempotent), then launches Claude
  Code in `repo/` with the managed configuration and `CLAUDE_CONFIG_DIR=<config_dir>`.
  - **Default (no `--autonomous`): ATTENDED interactive.** The user drives the Claude Code session
    directly (this is the claude-mpm replacement). `--task` may pre-seed the first prompt but the
    user retains control; the process is foregrounded / attachable.
  - **`--autonomous` (requires/strongly pairs with `--task`): harness-driven.** The session runs
    supervised under the harness with auto-answered decisions per the DOC-23 learned-autonomy tier.
    This mode is **opt-in** and depends on the DOC-23 adjudicator + autonomy tier being available;
    if that tier is unavailable/disabled, `--autonomous` degrades to attended with a warning (it
    never silently auto-answers).
- **Preconditions:** alias registered; (for `--autonomous`) a task supplied.
- **Postconditions:** a managed Claude Code session is running for the alias with the isolation
  invariant intact; attended mode yields an interactive session, autonomous mode yields a
  supervised harness session.
- **Error conditions:** unregistered alias → non-zero exit; load failure → propagates §02 errors;
  `--autonomous` without `--task` → non-zero exit advising a task; autonomy tier unavailable →
  attended fallback + stderr warning (not a hard failure).

#### Rationale (WHY)

Attended-by-default is the deliberate product stance: the driver must first be a *trustworthy daily
interactive tool* (a claude-mpm replacement) before it is an autonomous fleet runner. Making
autonomy strictly opt-in and degrade-to-attended keeps the safe path the default path and prevents
surprise unattended commits/pushes. `run` calling `load` unconditionally (leaning on §02 idempotency)
gives a single "just run it" verb.

#### Implementing Modules

| Module | Role |
|--------|------|
| `tm::commands::run` (new) | Resolves alias, ensures load, branches attended vs autonomous launch. |
| `core::session_launch` | Launches Claude Code with `CLAUDE_CONFIG_DIR` + managed config. |
| DOC-23 adjudicator (consumed) | Supplies auto-answer in `--autonomous`; absence → attended fallback. |

---

### IDE Attach — stable, discoverable project dir {#SPEC-STANDALONE-MPM-06~draft}

**ID:** SPEC-STANDALONE-MPM-06~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** `tm path <alias>` (and the on-disk managed layout from §03).
- **Outputs:** prints the absolute path to the alias's `repo/` directory — the directory an IDE
  (VS Code / Cursor) or a hand-launched `claude` should open.
- **Preconditions:** alias loaded (a managed dir with a marker file exists).
- **Postconditions:** opening that directory in an IDE or running `claude` in it inherits the **same**
  managed configuration **via the project-local files** (`repo/.claude/`, `repo/.mcp.json`,
  `repo/CLAUDE.md`), because those take precedence over the user's `~/.claude` per Claude Code's
  settings hierarchy (Managed > CLI args > Local > Project > User). Sessions are **read-write**: the
  user and IDE may edit source and `settings.local.json` freely.
- **Error conditions:** alias not loaded → non-zero exit advising `tm load <alias>`; missing/corrupt
  marker → non-zero exit.

#### Rationale (WHY)

Project-local `.claude/` + `.mcp.json` + `CLAUDE.md` are the **portable, IDE-honored** half of the
managed config — they ride with the checkout and are respected by both the CLI and (unlike
`CLAUDE_CONFIG_DIR`, see §6) the VS Code extension. A stable, alias-keyed, discoverable `repo/` path
is therefore the contract that makes IDE attach first-class: the user does not need to know the
config-dir mechanics; they just open `repo/`. Read-write (not read-only) sessions are required for the
driver to be a real daily tool.

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

- **Inputs:** a loaded alias's `repo/.mcp.json` (created/merged during `load`/`update`).
- **Outputs:** the project-scoped `.mcp.json` declares the trusty MCP servers for this project:
  `trusty-memory` (`serve --stdio`), `trusty-search` (`serve`, optionally `--index <id>` pinned to
  the project), and `trusty-review` (`review` stdio adapter) — each as a `stdio` server entry. The
  managed `repo/.claude/settings.json` enables these project MCP servers (e.g.
  `enableAllProjectMcpServers` or an explicit `enabledMcpjsonServers` list) so the managed session
  picks them up without a global `~/.claude.json` trust seed.
- **Preconditions:** a managed project dir exists.
- **Postconditions:** the three trusty servers are wired per-project and enabled for the managed
  session; entries are **merged idempotently** (re-running `load`/`update` does not duplicate or
  clobber user-added MCP servers). No global MCP/trust state is written.
- **Error conditions:** malformed pre-existing `.mcp.json` → leave untouched and report (mirrors the
  current safety stance of `preseed_workspace_trust`); a server binary absent on PATH → wire the
  entry anyway (runtime concern) but surface a `tm doctor`-style warning.

#### Rationale (WHY)

Per-project MCP wiring (vs the current global `~/.claude.json` trust seed) is what lets each managed
project enable exactly its trusty servers in isolation, honoring §SPEC-STANDALONE-MPM-04. Enabling
via project `settings.json` (`enabledMcpjsonServers`) rather than seeding `~/.claude.json` removes the
last global write. Adding `trusty-review` alongside memory/search rounds out the trusty triad for the
managed session. Idempotent merge preserves any MCP servers the user adds by hand.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::session_launch::settings::inject_trusty_memory_mcp` / `inject_trusty_search_mcp` (+ new `inject_trusty_review_mcp`) | Merge server entries into `repo/.mcp.json`. |
| `core::session_launch::settings` (enable list) | Write `enabledMcpjsonServers` into managed `repo/.claude/settings.json` instead of `~/.claude.json`. |

---

## 3. Interaction Model (summary)

| Mode | Trigger | Who drives | Decision handling | Default? |
|------|---------|-----------|-------------------|----------|
| **Attended** | `tm run <alias>` (no `--autonomous`) | The user, interactively | User answers all prompts | **Yes** |
| **Autonomous** | `tm run <alias> --autonomous --task <t>` | The harness, supervised | Auto-answered per DOC-23 tier; degrades to attended if tier unavailable | No (opt-in) |

The driver is a daily-driver-first tool: attended is the safe default, autonomy is an explicit,
degradable opt-in.

## 4. Assumptions & Risks

| # | Assumption / Risk | Status | Mitigation / WI |
|---|-------------------|--------|-----------------|
| A1 | **`CLAUDE_CONFIG_DIR` redirects Claude Code's global config dir per-process.** Confirmed for the **CLI** (Claude Code v1.0.30+ supports it; behaves like `XDG_CONFIG_HOME`). **BUT it is officially undocumented** (not in `--help` or docs; multiple open issues request documentation). | **Partially confirmed — must validate.** | **WI-1** must validate the exact behavior against the pinned Claude Code version and record it; pin a minimum CC version. |
| A2 | **`CLAUDE_CONFIG_DIR` does not suppress project-local `.claude/` creation.** Confirmed: a local `.claude/settings.local.json` may still be written in the workspace even with the var set. This is **acceptable** — the managed layout *wants* project-local files under `repo/`; we just must not assume the var centralizes everything. | **Confirmed (acceptable).** | Managed layout treats `repo/.claude/` as the portable half by design. |
| A3 | **The VS Code / Cursor extension IGNORES `CLAUDE_CONFIG_DIR`** (reads/writes `~/.claude/` regardless). | **Confirmed risk.** | IDE-attach (§06) relies **only** on project-local `repo/.claude/` + `.mcp.json` + `CLAUDE.md`, which the extension *does* honor — **not** on the per-project config-dir. WI-1 documents this boundary; the config-dir isolation protects the CLI path, project-local files protect the IDE path. |
| A4 | **Project-local `.claude/` precedence over user `~/.claude`.** Confirmed: precedence is Managed > CLI args > Local (`settings.local.json`) > Project (`.claude/`) > User (`~/.claude`); `.mcp.json` is the project MCP scope; `enabledMcpjsonServers` enables them. | **Confirmed.** | Relied on by §06/§08. |
| A5 | **Deployers are re-targetable away from `dirs::home_dir()`.** The agent/skill deployers already accept a target/dest dir; output-style/hook/trust helpers resolve `home_dir()` internally and must be parameterized. | **Verified (re-targetable; some helpers need a param).** | WI-2/WI-3 thread the config-dir through; WI-7 asserts no global writes. Fail-closed if a helper cannot be re-targeted (§04). |
| A6 | Concurrent `load`/`update` on the same alias could race on the checkout/config. | Risk. | Per-alias advisory lock (file lock in the managed dir); document in impl. |
| A7 | A user already has a non-managed dir where the managed root wants to write. | Risk. | Marker-file guard (§03): refuse without `--force`. |

## 5. Work-Item (WI) Breakdown

> Scopes: **S** ≈ ≤1 day, **M** ≈ 2–4 days, **L** ≈ ≥1 week. To be filed as an epic for maintainer
> review; this spec is the behavior contract the epic's WIs implement.

| WI | Scope | Work | Realizes | Depends on |
|----|-------|------|----------|------------|
| **WI-1** | **M** | **Formalize + validate the Managed Configuration Standard.** Empirically validate `CLAUDE_CONFIG_DIR` against the pinned Claude Code version (CLI honors it; what it does/doesn't isolate); document the VS Code-extension exclusion; pin a minimum CC version; write the standard doc (layout, marker schema, ownership, isolation invariant) and a conformance checklist. | SPEC-STANDALONE-MPM-03, -04 (assumptions A1–A4) | — |
| **WI-2** | **M** | **Workspace-scope the agent/skill/output-style deploys.** Thread an explicit config-dir target through `deploy_agents_filtered`, `deploy_skills_filtered`, `deploy_output_style`; remove the `home_dir()` global fallback in managed mode (fail closed). | SPEC-STANDALONE-MPM-04 (sites 1–3) | WI-1 |
| **WI-3** | **S** | **Workspace-scope the hook-clean + trust-seed.** Re-target `remove_global_trusty_memory_hooks` and `preseed_workspace_trust_home` to the per-project config-dir; move MCP enablement to project `settings.json` (`enabledMcpjsonServers`) instead of `~/.claude.json`. | SPEC-STANDALONE-MPM-04 (sites 4–5), -08 | WI-1 |
| **WI-4** | **M** | **Standalone-driver registry + `register`/`ls`.** New registry file under the trusty-mpm config root; `tm register`, `tm ls`. Reuse/align with the DOC-22 project registry. | SPEC-STANDALONE-MPM-01, -07 (`ls`) | — |
| **WI-5** | **L** | **`load` + managed-config generation.** Stable alias-keyed `provision_in` clone; full managed layout (`repo/` + `claude-config/` + marker); idempotent refresh; `core::managed_config` module. | SPEC-STANDALONE-MPM-02, -03 | WI-1, WI-2, WI-3, WI-4 |
| **WI-6** | **M** | **`run` (attended/autonomous) + `path`/`update`/`rm`.** Attended-default launch with `CLAUDE_CONFIG_DIR`; `--autonomous` wired to the DOC-23 tier with attended fallback; the supporting verbs over the marker. | SPEC-STANDALONE-MPM-05, -06, -07 | WI-5; DOC-23 (autonomous only) |
| **WI-7** | **M** | **Isolation-invariant integration tests.** Tests that run `load`/`run`/`update` against a sandboxed `$HOME` and assert **zero** writes outside `<managed-root>/<alias>` (no `~/.claude/`, no `~/.claude.json`); idempotency tests for `load`/`update`; marker-guard + fail-closed tests. | SPEC-STANDALONE-MPM-04 (the invariant) | WI-5, WI-6 |
| **WI-8** | **S** | **`trusty-review` MCP wiring + per-project enable.** Add `inject_trusty_review_mcp`; enable the trusty triad via project `settings.json`. | SPEC-STANDALONE-MPM-08 | WI-3, WI-5 |

**Critical path:** WI-1 → (WI-2 ∥ WI-3 ∥ WI-4) → WI-5 → WI-6 → WI-7. WI-8 rides alongside WI-5/WI-6.
**Parallelizable:** WI-2, WI-3, WI-4 after WI-1; WI-8 after WI-3.

## 6. Open Questions / Future Work

1. **Managed root location.** `~/.trusty-mpm/projects/<alias>/` vs `~/trusty-mpm-projects/<alias>/`
   (more IDE-discoverable, outside a dotdir)? Pick one in WI-1; both honor the isolation invariant.
2. **`CLAUDE_CONFIG_DIR` longevity.** It is undocumented (A1). If Anthropic changes/removes it, the
   CLI isolation half needs a fallback (e.g. a per-project `$HOME` shim — heavier, affects git/ssh).
   Track upstream issues; WI-1 records the pinned-version behavior.
3. **IDE config-dir parity.** Since the VS Code extension ignores `CLAUDE_CONFIG_DIR` (A3), an
   IDE-attached session's *global* agent/skill state still comes from the user's `~/.claude`. The
   project-local files cover project scope; is per-project agent/skill isolation needed for the IDE
   path, or is project-scope sufficient? Defer until WI-1 measures the gap.
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
- [DOC-23 — Learned-Autonomy Auto-Answer](./learned-autonomy-auto-answer.md) — the autonomy tier `--autonomous` opts into.
- `crates/trusty-mpm/src/provisioner/workspace.rs` — `WorkspaceProvisioner::provision` / `provision_in`.
- `crates/trusty-mpm/src/core/session_launch/mod.rs` — `prepare_session` (the global/local write hot path).
- `crates/trusty-mpm/src/core/session_launch/settings.rs` — `inject_trusty_memory_mcp`, `inject_trusty_search_mcp`, `write_output_style`, `write_project_hooks`, `preseed_workspace_trust_home`, `remove_global_trusty_memory_hooks`, `deploy_output_style`.
- `crates/trusty-mpm/src/core/agent_deployer.rs` — `deploy_agents_filtered` (global agent deploy site).
- `crates/trusty-mpm/src/core/skill_deployer.rs` — `deploy_skills_filtered` (global skill deploy site).
- `crates/trusty-mpm/src/core/claude_config.rs` — `ClaudeConfigPaths` / `ClaudeConfigReader` (path model).
- `crates/trusty-mpm/src/bin/tm/cli.rs`, `src/bin/tm/main.rs` — the `tm` command surface.
- Claude Code settings precedence (Managed > CLI > Local > Project > User) and `.mcp.json` project scope — Claude Code docs.
- `CLAUDE_CONFIG_DIR` behavior + VS Code-extension exclusion — Claude Code GitHub issues #3833, #30538, #33430 (undocumented; CLI-only).
