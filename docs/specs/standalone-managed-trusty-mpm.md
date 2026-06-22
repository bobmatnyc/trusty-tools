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
> per-project directory layout where **CLAUDE.md + project agents + project skills live project-local
> under `repo/.claude/`** (standard Claude Code discovery) while the **global hooks + global
> skills/slash-commands + global MCPs are supplied by tm out of a single tm-owned user-level config
> dir** that tm points Claude Code at via a **required custom `CLAUDE_CONFIG_DIR`** (so the user's real
> global `~/.claude` — which carries claude-mpm's own global hooks/MCPs — is **excluded entirely** and
> cannot step on tm's) — plus the **isolation invariant** that in managed mode trusty-mpm writes
> **nothing** to the user's real global `~/.claude/` or `~/.claude.json`;
> (3) the **two-layer interaction model** — `tm` invoked directly is **always attended/interactive**
> (the claude-mpm replacement), and **autonomy is provided exclusively by the session manager**, the
> durable tmux fleet daemon that drives tm-managed projects under DOC-23; (4) first-class IDE attach
> via stable, discoverable project dirs carrying project-local `.claude/` + `CLAUDE.md` (the IDE
> inherits CLAUDE.md/project agents/skills but **not** tm's global hooks/MCPs — the IDE ignores
> `CLAUDE_CONFIG_DIR`, so faithful sessions launch via `tm`).
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
(project-local CLAUDE.md/project agents/skills + a single tm-global config dir holding global
hooks/skills-slash-commands/MCPs, reached via a required custom `CLAUDE_CONFIG_DIR`) and its isolation
invariant, the attended `tm run` contract, and the IDE-attach contract.

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
| SPEC-STANDALONE-MPM-08~draft | [Global MCP wiring (memory / search / review) in the tm-global config](#global-mcp-wiring-memory--search--review-spec-standalone-mpm-08draft) | `core::session_launch::settings` (inject_* ) |

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
  (the *managed project dir*; layout in §SPEC-STANDALONE-MPM-03), then generates the **project-local**
  half of the managed configuration: `repo/.claude/` (project agents/skills + non-hook settings),
  `repo/CLAUDE.md`, and the `.trusty-mpm/` metadata. The **global** half (global hooks +
  skills/slash-commands + MCPs) is not regenerated here — it lives in the single tm-global config dir
  established once at `tm install` / `tm config` time, which `run` reaches via the required
  `CLAUDE_CONFIG_DIR` (§03(c)). `load` writes **nothing** to the user's real `~/.claude*`. Prints the
  project dir path on success.
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
| `core::session_launch::prepare_session` (managed-mode variant) | Generates the project-local config under `repo/` + `.trusty-mpm/` metadata; resolves the tm-global `CLAUDE_CONFIG_DIR` for `run`. |

---

### The Managed Configuration Standard {#SPEC-STANDALONE-MPM-03~draft}

**ID:** SPEC-STANDALONE-MPM-03~draft
**Status:** Draft

#### Behavior Contract (WHAT)

The Managed Configuration is the complete, isolated config that trusty-mpm owns and writes. It splits
the five Claude Code customization surfaces into **two halves by *where they live*** — a **single,
shared tm-global config dir** (the user-level layer, established once) and the **per-project checkout**
(the project layer, regenerated per `load`):

- **Project-local half — picked up by standard Claude Code discovery, per checkout.** `CLAUDE.md`,
  **project** `agents/`, and **project** `skills/` live **inside the checkout** under `repo/`
  (`repo/CLAUDE.md`, `repo/.claude/agents/`, `repo/.claude/skills/`). Any process started in `repo/`
  — `tm`, a plain `claude`, or the IDE extension — discovers them automatically; no flags required.
  This half is regenerated/refreshed by each `load`/`update`, exactly as today.
- **tm-global half — a single user-level config dir, established once, reached via a *required*
  custom `CLAUDE_CONFIG_DIR`.** The **global hooks**, **global skills / slash-commands**, and
  **global MCP servers** (trusty-memory / search / review) live in **one** tm-owned user-level config
  directory — **not** per project. `tm` launches Claude Code with
  `CLAUDE_CONFIG_DIR=<tm-global-config-dir>` so Claude Code merges *that* dir as the user-level layer
  **instead of the user's real `~/.claude`**. This is the **load-bearing isolation primitive**: the
  maintainer's real `~/.claude` carries claude-mpm's own global hooks + MCPs, which Claude Code would
  otherwise merge on top of project config and "step on" tm's; pointing `CLAUDE_CONFIG_DIR` at the
  tm-global dir **excludes the real global entirely**. This dir is created and maintained **once**
  (at `tm install` / `tm config` time), **not** regenerated per project.

Because the global hooks/skills-slash-commands/MCPs live in the tm-global config dir (the user-level
layer selected by `CLAUDE_CONFIG_DIR`), they apply to **every** tm-launched session uniformly — and
the **real** `~/.claude` (and its claude-mpm hooks/MCPs) is excluded, so there is **no bidirectional
pollution**.

It is defined by four parts:

**(a) Directory layout — one global config dir + per-alias project dirs.**

The single tm-global config dir (established once; the user-level layer for *all* tm sessions):

```
~/.trusty-mpm/claude-config/             # the ONE tm-global CLAUDE_CONFIG_DIR (NOT per-project)
├── settings.json                        # the ONE set of GLOBAL hooks (+ trust / output-style keys)
├── skills/<name>/SKILL.md               # GLOBAL skills / slash-commands (shared across all aliases)
├── .mcp.json (or settings MCP block)    # GLOBAL MCP servers: trusty-memory / search / review
├── .credentials.json                    # REQUIRED auth — CLAUDE_CONFIG_DIR relocates creds here (A9);
│                                         #   seed it, OR run with ANTHROPIC_API_KEY (+--bare). WI-10.
└── …                                    # whatever else Claude Code's user-level layer holds
```

Each managed project dir, alias-keyed and stable, holds only the **project-local** half:

```
<managed-root>/<alias>/                  # e.g. ~/.trusty-mpm/projects/<alias>/  (or ~/trusty-mpm-projects/<alias>/)
├── repo/                                 # the git checkout (clone target; what the IDE opens)
│   ├── .claude/
│   │   ├── agents/<name>.md             # deployed PROJECT agents — STANDARD DISCOVERY (IDE/plain-claude inherit)
│   │   ├── skills/<name>/SKILL.md       # deployed PROJECT skills — STANDARD DISCOVERY (+ manifests)
│   │   ├── settings.json                # outputStyle, spinnerTips only (global hooks live in tm-global dir)
│   │   └── settings.local.json          # user-owned local overrides (trusty-mpm never clobbers)
│   └── CLAUDE.md                         # project instructions — STANDARD DISCOVERY (created if absent)
└── .trusty-mpm/                          # tm-PRIVATE per-project metadata
    ├── last-instructions.md             # inspectable composed launch prompt
    ├── tm-settings.json                 # OPTIONAL per-run --settings override / replication file
    ├── tm-mcp.json                       # OPTIONAL per-run --mcp-config override / replication file
    └── managed.toml                     # marker: alias, url, ref, generated-by version, tm-global-config-dir path
```

**(b) Ownership.** trusty-mpm **owns and may overwrite**: the entire tm-global config dir
(`~/.trusty-mpm/claude-config/` — global hooks, global skills/slash-commands, global MCPs, and the
seeded `.credentials.json` when credential path (a) is used), plus, per
checkout, `repo/.claude/agents/` and `repo/.claude/skills/` (deployed framework bundle of **project**
agents/skills), `repo/.claude/settings.json` (managed **non-hook** keys only — outputStyle,
spinnerTips), `repo/CLAUDE.md` (refreshed but preserves user-added sections where feasible), and
everything under `.trusty-mpm/`. trusty-mpm **never** clobbers `repo/.claude/settings.local.json`,
the user's **real** `~/.claude*`, or user source files in `repo/`.

**(c) The tm-global config dir via required `CLAUDE_CONFIG_DIR` (the load-bearing mechanism); arg
files secondary.** The mechanism that delivers the global hooks/skills-slash-commands/MCPs **and**
excludes the real `~/.claude` is the env var: `tm` invokes `claude` with
`CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config` and cwd `repo/`. This is **required and primary**, and
the isolation behavior is **CONFIRMED** (A1). `CLAUDE_CONFIG_DIR` is **undocumented**, so WI-1 keeps a
lightweight regression guard that re-verifies it against the pinned Claude Code version on major
upgrades (non-blocking); the VS Code/Cursor extension **ignores** it (A3) — so faithful sessions launch
via `tm`.
The verified `--settings <file>` / `--mcp-config <file>` flags (A8) are **secondary / supplementary**:
they are useful for per-run overrides and for *manually replicating* a tm session with the **same
arguments**, but they are **no longer** the primary delivery mechanism for hooks/MCPs (those ride in
the tm-global config dir). When tm emits `.trusty-mpm/tm-settings.json` / `tm-mcp.json` at all, it is
as an optional override/replication aid, not the load-bearing path. See §SPEC-STANDALONE-MPM-04 for
the invariant and §6 for the longevity note.

**Credential precondition (the cost of relocating the user-level layer) — WI-10 IMPLEMENTED.**
Because `CLAUDE_CONFIG_DIR` relocates the **entire** `~/.claude/` tree-equivalent, it also relocates
`~/.claude/.credentials.json` (validated 2026-06-22 / v2.1.185, A9). A session launched against a
*clean* tm-global config dir is therefore **unauthenticated** ("Not logged in") and cannot make API
calls. **NOTE:** `.credentials.json` carries MCP OAuth tokens, NOT primary Claude Max/Pro session
auth — primary auth uses the macOS Keychain keyed by the `CLAUDE_CONFIG_DIR` path.

The two WI-10 auth paths (both implemented, 2026-06-22):
- **(a) Keychain (default — `tm login`).** Run `tm login` once. This launches
  `claude auth login` under `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config` so the OAuth flow
  creates a keychain entry for that path. All subsequent `tm run` sessions authenticate on the
  Claude Max/Pro plan via the keychain (no further setup needed).
- **(b) API key + `--bare` (CI/automation).** When `ANTHROPIC_API_KEY` is set in the environment,
  `tm run` automatically adds `--bare` to the `claude` invocation. `--bare` bypasses
  keychain/OAuth reads and uses the API key directly; no `tm login` needed.

`tm run` emits a non-blocking hint to run `tm login` when neither path is detectable
(keychain entries cannot be probed without spawning `claude auth status`).

**(d) The marker file.** `.trusty-mpm/managed.toml` records `{alias, url, ref, claude_config_dir,
settings_arg_path?, mcp_config_arg_path?, generated_by_version}` so `path`/`ls`/`update`/`rm` and an
attaching IDE can discover the managed project — and so a human can replay the exact `tm` launch
(`CLAUDE_CONFIG_DIR` plus any optional override args) — deterministically.

**Config model contrast (claude-mpm vs trusty-mpm).** claude-mpm uses the user's **real global
`~/.claude` discovery for all five** customization surfaces: running `claude` in a source dir merges
the real global CLAUDE.md/agents/skills/hooks/MCPs on top of project config — which is exactly why the
maintainer's claude-mpm global hooks/MCPs would step on tm's. trusty-mpm instead points
`CLAUDE_CONFIG_DIR` at a **tm-global config dir** that supplies the **global** surfaces (global hooks
+ global skills/slash-commands + global MCPs) while **excluding the real `~/.claude` entirely**, and
layers the **project-local** `repo/.claude` (CLAUDE.md + project agents/skills, standard discovery) on
top. Effective config = **tm-global (hooks + skills/slash-commands + MCPs) ⊕ project-local
`repo/.claude` (CLAUDE.md + project agents/skills)**, real `~/.claude` excluded → no bidirectional
pollution. The practical consequence: a tm session can be **reproduced** by launching `claude` with
the same `CLAUDE_CONFIG_DIR` (and, optionally, the same `--settings`/`--mcp-config` override args) in
`repo/`; conversely, a session launched **without** that `CLAUDE_CONFIG_DIR` (e.g. the IDE, which
ignores it) gets the project-local config plus the **real** global — i.e. the step-on tm avoids.

- **Inputs:** an alias + its resolved checkout (from `load`), plus the already-established tm-global
  config dir (from `tm install` / `tm config`).
- **Outputs:** the per-alias project tree above, fully populated and isolation-compliant: project-local
  CLAUDE.md/agents/skills under `repo/` (discoverable); the global hooks/skills-slash-commands/MCPs
  already present in the tm-global config dir (not regenerated per project).
- **Preconditions:** a valid checkout under `repo/`, and an existing tm-global config dir.
- **Postconditions:** the project layout is complete; the marker file records the tm-global
  `CLAUDE_CONFIG_DIR` path (and any optional override-arg paths); the project-local half is
  discoverable in `repo/`; the global half lives in the tm-global config dir; no **real** global
  `~/.claude*` path was written (§SPEC-STANDALONE-MPM-04).
- **Error conditions:** any failed write fails the generation step and reports the offending path;
  a missing tm-global config dir → fail advising `tm install`/`tm config`; a pre-existing non-managed
  dir at `<managed-root>/<alias>` without a marker file → refuse and advise `tm rm`/`--force`.

#### Rationale (WHY)

Splitting the surfaces *by where they live* — project-local (CLAUDE.md/project agents/skills) into
`repo/` for standard discovery, and the **global** hooks/skills-slash-commands/MCPs into a **single**
tm-global config dir selected by a **required** custom `CLAUDE_CONFIG_DIR` — is what makes the driver
simultaneously *isolated* (the real `~/.claude` with claude-mpm's hooks/MCPs is excluded entirely, so
the two never step on each other) and *simple* (the global layer is established **once**, not
regenerated per project). `CLAUDE_CONFIG_DIR` is the **load-bearing** primitive: it is the only thing
that swaps Claude Code's user-level layer away from the real `~/.claude`, and that swap is **CONFIRMED**
(A1). Because the flag is undocumented, WI-1 keeps a lightweight, **non-blocking** regression guard that
re-verifies it against the pinned Claude Code version on major upgrades; because the IDE extension
ignores it (A3), faithful sessions launch via `tm` and IDE attach deliberately relies only on
project-local discovery (§06). The `--settings`/`--mcp-config` flags (A8) are kept documented but **secondary** —
useful for per-run overrides and for manually replicating a tm launch with the same arguments, not the
primary delivery path. The marker file (which records the `CLAUDE_CONFIG_DIR` path) gives every other
verb a single source of truth and makes the standard auditable: a test can assert the project tree's
shape, that nothing escaped to the real `~/.claude*`, and that a session launched with the same
`CLAUDE_CONFIG_DIR` reproduces the configuration. Splitting managed vs user-owned files
(settings.json vs settings.local.json) preserves the user's ability to edit while letting `update`
refresh framework artifacts safely.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::managed_config` (new) | Defines the layout, marker schema (incl. the tm-global `CLAUDE_CONFIG_DIR` path), ownership rules; entry point for project-config generation + discovery. |
| `core::session_launch::settings` | Deploys project agents/skills into `repo/.claude/`; establishes/maintains the single tm-global config dir (global hooks + skills/slash-commands + MCPs); optionally emits the secondary `.trusty-mpm/tm-settings.json` / `tm-mcp.json` override/replication files. |
| `core::session_launch::prepare_session` | Composes instructions + drives the project-local deploy + resolves the tm-global `CLAUDE_CONFIG_DIR` for launch. |

---

### Isolation Invariant — no global `~/.claude` writes {#SPEC-STANDALONE-MPM-04~draft}

**ID:** SPEC-STANDALONE-MPM-04~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** any managed/standalone-mode operation (`tm install`/`tm config`, `load`, `run`,
  `update`).
- **Outputs:** all configuration is written **only** to (i) the single **tm-global config dir**
  (`~/.trusty-mpm/claude-config/` — the global hooks, global skills/slash-commands, global MCPs;
  selected at launch by the required `CLAUDE_CONFIG_DIR`) and (ii) the per-alias project-local
  `repo/.claude/` (project agents/skills, non-hook settings) plus `.trusty-mpm/` metadata. trusty-mpm
  writes **nothing** to the user's **real** `~/.claude/` or `~/.claude.json`.
- **Preconditions:** the driver is in managed mode (the default for `register/load/run`).
- **Postconditions — the invariant:** for the entire duration of a managed operation, trusty-mpm
  writes **nothing** under the user's **real** `~/.claude/` (no `agents/`, `skills/`, `output-styles/`,
  `settings.json`) and **nothing** to the real `~/.claude.json`. Isolation is achieved by (a) the
  custom `CLAUDE_CONFIG_DIR` swapping Claude Code's user-level layer away from the real `~/.claude`
  (excluding claude-mpm's global hooks/MCPs) and (b) all tm writes targeting only the tm-global config
  dir and the project workspace. Specifically, the five current global write sites (§1.1) are, in
  managed mode, re-targeted as follows:
  1. `deploy_agents_filtered` target → `repo/.claude/agents/` (project-local, standard discovery).
  2. `deploy_skills_filtered` dest → `repo/.claude/skills/` (project agents) **or** the tm-global
     config dir's `skills/` (global skills/slash-commands) — **never** the real `~/.claude/skills/`.
  3. `deploy_output_style` home → `repo/.claude/settings.json` (`outputStyle` key) or the tm-global
     config dir, **never** the real `~/.claude/output-styles/`.
  4. `remove_global_trusty_memory_hooks` / hook composition → writes the **one** global `hooks` block
     into the tm-global config dir's `settings.json` (loaded because `CLAUDE_CONFIG_DIR` selects that
     dir as the user-level layer), **never** the real `~/.claude/settings.json`.
  5. `preseed_workspace_trust_home` → seeds trust in the tm-global config dir's `.claude.json`
     equivalent (under `CLAUDE_CONFIG_DIR`), **never** the real `~/.claude.json`.
- **Error conditions:** if re-targeting cannot be honored (e.g. a deployer is hardwired to
  `dirs::home_dir()`), the operation **must fail closed** rather than silently fall back to a real
  global write. A managed operation that would touch the user's real `~/.claude*` path is a contract
  violation.

#### Rationale (WHY)

This is the core promise of the standalone driver: a user's own claude-mpm `~/.claude` setup must be
untouched **and** excluded from the tm session, so the driver can run concurrently and *replace* it
for daily work without the two stepping on each other. The required custom `CLAUDE_CONFIG_DIR` is what
guarantees both directions — tm writes only to its tm-global config dir and the project workspace, and
the real `~/.claude` (with claude-mpm's global hooks/MCPs) is never even loaded into the tm session.
Fail-closed (not fall-back-to-real-global) is mandatory because a silent real-global write is exactly
the collision we are eliminating — a degraded-but-isolated failure is acceptable; a
working-but-globally-polluting one is not. The invariant is mechanically testable (assert no writes
outside the tm-global config dir and `<managed-root>`), which is why it is its own ID and gets a
dedicated integration-test work item (WI-7).

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::agent_deployer::deploy_agents_filtered` | Accept an explicit target dir → `repo/.claude/agents/`; no real-`home_dir()` fallback in managed mode. |
| `core::skill_deployer::deploy_skills_filtered` | Same: explicit dest → `repo/.claude/skills/` (project skills) or the tm-global config dir's `skills/` (global skills/slash-commands), no real-global fallback. |
| `core::session_launch::settings` | Write the one global `hooks` block + global MCPs into the tm-global config dir; re-target output-style/trust to `repo/.claude/settings.json` or the tm-global config dir. |
| `core::managed_config` (new) | Supplies the tm-global `CLAUDE_CONFIG_DIR` path (+ project paths) and enforces the fail-closed guard. |

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
  Code **attended/interactive** with `cwd = repo/` and **`CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config`
  (the tm-global config dir, required)** so the global hooks + global skills/slash-commands + global
  MCPs are supplied and the user's real `~/.claude` is excluded (§03(c), §04). The project-local
  CLAUDE.md + project agents/skills come from standard discovery in `repo/`. Optionally, per-run
  override args (`--settings .trusty-mpm/tm-settings.json` / `--mcp-config .trusty-mpm/tm-mcp.json`,
  secondary) may be passed for overrides or replication. The user drives the Claude Code session
  directly (this is the claude-mpm replacement); `--task` may pre-seed the first prompt but the user
  retains control; the process is foregrounded / attachable.
- **Preconditions:** alias registered; the tm-global config dir established (`tm install`/`tm config`);
  **the tm-global config dir carries valid credentials** (see *Credential precondition* below).
- **Credential precondition (required for API calls) — WI-10 IMPLEMENTED.** Because the required
  `CLAUDE_CONFIG_DIR` relocates the **entire** `~/.claude/` tree-equivalent — including the macOS
  Keychain entry used for Claude Max/Pro OAuth (A9) — a session launched against a *clean* tm-global
  config dir starts **unauthenticated** (`claude` reports "Not logged in"). **NOTE:** `.credentials.json`
  (MCP OAuth tokens) is seeded by `ensure_global_config_dir` but does NOT establish primary session
  auth. `tm run` supports two WI-10 auth paths:
  - **(a) Keychain (default — `tm login` once):** `tm login` runs `claude auth login` under
    `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config` so the OAuth flow creates a Keychain entry for
    that path. All subsequent `tm run` sessions authenticate via the Keychain on the user's Claude
    Max/Pro plan (no further setup needed, no `ANTHROPIC_API_KEY` required).
  - **(b) API key + `--bare` (CI/automation):** when `ANTHROPIC_API_KEY` is set, `tm run`
    automatically adds `--bare` to the `claude` invocation. `--bare` bypasses Keychain/OAuth reads
    and uses the API key directly; no `tm login` step needed.
  When neither path is detected, `tm run` emits a non-blocking stderr hint pointing to `tm login`
  (Keychain entries cannot be probed without spawning `claude auth status`, so no hard failure).
- **Postconditions:** an **attended, interactive** managed Claude Code session is running for the
  alias with the isolation invariant intact — global hooks/MCPs supplied via the tm-global
  `CLAUDE_CONFIG_DIR`, the real `~/.claude` excluded, project-local config discovered from `repo/`,
  and the session **authenticated** (seeded `.credentials.json` or `ANTHROPIC_API_KEY`+`--bare`).
- **Error conditions:** unregistered alias → non-zero exit; load failure → propagates §02 errors;
  **no credential path available** (neither a seeded `.credentials.json` in the tm-global config dir
  nor `ANTHROPIC_API_KEY` in the environment) → fail with a diagnostic advising `tm install`/`tm config`
  (seed credentials) or exporting `ANTHROPIC_API_KEY` (+`--bare`), since the session would otherwise
  launch unauthenticated (A9).

#### Rationale (WHY)

`tm run` is attended-by-definition because the safe, interactive daily-driver path is the *only* path
`tm` itself offers — autonomy is a property of the **orchestrator that drives `tm`** (the session
manager, Layer 2), not of `tm`. Keeping the layers separate prevents surprise unattended
commits/pushes from a hand-run `tm` and gives a single "just run it" interactive verb (`run` calls
`load` unconditionally, leaning on §02 idempotency). Because the session is launched with the tm-global
`CLAUDE_CONFIG_DIR` (and `cwd = repo/`), it is also reproducible (§03) — re-launching `claude` with the
same `CLAUDE_CONFIG_DIR` in `repo/` reproduces the configuration; the optional `--settings`/`--mcp-config`
override args make a replication command fully explicit.

#### Implementing Modules

| Module | Role |
|--------|------|
| `tm::commands::run` (new) | Resolves alias, ensures load, checks WI-10 auth path (`ANTHROPIC_API_KEY` → `--bare`; otherwise keychain hint), then launches an **attended** Claude Code session with `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config` and `cwd = repo/`. `build_launch_command` (in `core::standalone::run`) adds `--bare` when the API key is set. |
| `tm login` (new, WI-10) | New `Login` CLI variant; `login_cmd` in `commands::standalone` calls `build_login_command` → `claude auth login` under `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config` with inherited stdio so the user completes the OAuth flow. |
| `core::session_launch` | Launches Claude Code with the tm-global `CLAUDE_CONFIG_DIR` (and optional override args) for an interactive session. |
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
  in it inherits, **via standard project-local discovery**, the **project-local** half of the managed
  configuration — `repo/CLAUDE.md`, `repo/.claude/agents/`, `repo/.claude/skills/`, and the non-hook
  `repo/.claude/settings.json` — because those take precedence over the user-level layer per Claude
  Code's settings hierarchy (Managed > CLI args > Local > Project > User). It does **NOT** inherit
  tm's **global hooks or global MCPs**: those live in the tm-global config dir selected by
  `CLAUDE_CONFIG_DIR`, and the **VS Code / Cursor extension ignores `CLAUDE_CONFIG_DIR`** (A3) — so an
  IDE-attached session instead loads the project-local config layered on the user's **real**
  `~/.claude` (i.e. claude-mpm's global hooks/MCPs step on tm's). To get a faithful tm session
  (tm-global hooks/MCPs, real `~/.claude` excluded), launch via `tm` (or `claude` with the same
  `CLAUDE_CONFIG_DIR` set, where the harness honors it). Sessions are **read-write**: the user and IDE
  may edit source and `settings.local.json` freely.
- **Error conditions:** alias not loaded → non-zero exit advising `tm load <alias>`; missing/corrupt
  marker → non-zero exit.

#### Rationale (WHY)

Project-local `CLAUDE.md` + `.claude/agents/` + `.claude/skills/` are the **portable, IDE-honored,
statically-discovered** half of the managed config — they ride with the checkout and are respected by
both the CLI and the VS Code extension. A stable, alias-keyed, discoverable `repo/` path is therefore
the contract that makes IDE attach first-class for the project-local half: the user just opens `repo/`.
The **global** half (global hooks/skills-slash-commands/MCPs) is deliberately *not* IDE-inherited — it
lives in the tm-global config dir reached via `CLAUDE_CONFIG_DIR`, which the **VS Code/Cursor extension
ignores** (A3), so an IDE session instead picks up the user's real `~/.claude` (the step-on). This is
the price of using `CLAUDE_CONFIG_DIR` as the isolation primitive, and the contract states it
explicitly so the IDE/`tm` boundary is unambiguous: faithful, isolated sessions launch via `tm`.
Read-write (not read-only) sessions are required for the driver to be a real daily tool.

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
  **project-local** managed config (project agents/skills/output-styles/instructions to the current
  framework bundle), in place, isolation-invariant intact. (The shared tm-global config dir — global
  hooks/skills-slash-commands/MCPs — is maintained by `tm install`/`tm config`, not by per-alias
  `update`.) `--force` allows resetting a dirty checkout (stashing or discarding per flag semantics,
  documented at impl). Idempotent; equivalent to the refresh half of `load`.
- **`tm rm <alias> [--purge]`:** deregisters the alias. Without `--purge`, leaves the managed project
  dir on disk (re-`register`+`load` re-adopts it). With `--purge`, removes the managed project dir
  (the `repo/` checkout + `.trusty-mpm/`) after confirmation. It does **not** touch the shared
  tm-global config dir (`~/.trusty-mpm/claude-config/`, owned by `tm install`/`tm config`) and never
  touches the user's real `~/.claude*`.
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

### Global MCP wiring (memory / search / review) in the tm-global config {#SPEC-STANDALONE-MPM-08~draft}

**ID:** SPEC-STANDALONE-MPM-08~draft
**Status:** Draft

#### Behavior Contract (WHAT)

- **Inputs:** the single **tm-global config dir** (`~/.trusty-mpm/claude-config/`), established/merged
  at `tm install` / `tm config` time, which `tm` selects at launch via the required
  `CLAUDE_CONFIG_DIR`.
- **Outputs:** the tm-global config dir declares the trusty MCP servers **once, globally** for every
  tm-launched session: `trusty-memory` (`serve --stdio`), `trusty-search` (`serve`, optionally
  `--index <id>`), and `trusty-review` (`review` stdio adapter) — each as a `stdio` server entry in
  the tm-global config's MCP block (`.mcp.json` or the settings MCP section under
  `CLAUDE_CONFIG_DIR`). Because `tm` launches with `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config`,
  the session picks them up **only when launched by `tm`** (or by `claude` with the same
  `CLAUDE_CONFIG_DIR`) — **not** from a discovery-path `repo/.mcp.json` and **without** any write to
  the user's **real** `~/.claude.json`.
- **Preconditions:** the tm-global config dir exists (`tm install`/`tm config`).
- **Postconditions:** the three trusty servers are wired into the tm-global config and applied to
  every tm-launched session; entries are **merged idempotently** (re-running `tm config` does not
  duplicate or clobber user-added MCP servers). No write to the real `~/.claude.json`; no
  discovery-path `repo/.mcp.json` is required.
- **Error conditions:** malformed pre-existing tm-global MCP config → leave untouched and report
  (mirrors the current safety stance of `preseed_workspace_trust`); a server binary absent on PATH →
  wire the entry anyway (runtime concern) but surface a `tm doctor`-style warning.

> **Secondary override.** A per-run `.trusty-mpm/tm-mcp.json` passed via `--mcp-config` (optionally
> with `--strict-mcp-config`) remains available as an **optional** per-session override/replication
> aid (§03(c), A8) — but the global trusty triad lives in the tm-global config dir, not in a per-project
> argument file.

#### Rationale (WHY)

Putting the trusty MCP servers in the **single** tm-global config dir (vs the current global
`~/.claude.json` trust seed, and vs a discovery-path `repo/.mcp.json`) is what lets **every** managed
session apply exactly the trusty triad **in isolation** — the tm-global config is the user-level layer
selected by `CLAUDE_CONFIG_DIR`, so the real `~/.claude` MCPs are excluded, honoring
§SPEC-STANDALONE-MPM-04 and keeping the IDE/plain-`claude` path free of tm's MCPs (§06). Maintaining
the set **once** (at `tm config`) rather than regenerating a per-project arg file removes drift and the
last real-global write. Adding `trusty-review` alongside memory/search rounds out the trusty triad.
Idempotent merge preserves any MCP servers the user adds by hand to the tm-global config.

#### Implementing Modules

| Module | Role |
|--------|------|
| `core::session_launch::settings::inject_trusty_memory_mcp` / `inject_trusty_search_mcp` (+ new `inject_trusty_review_mcp`) | Merge server entries **once** into the tm-global config dir's MCP block (selected at launch by `CLAUDE_CONFIG_DIR`). |
| `core::session_launch::settings` (launch) | Launch with `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config` (instead of seeding the real `~/.claude.json` or a discovery `.mcp.json`); optional `--mcp-config`/`--strict-mcp-config` per-run override. |

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
| A1 | **`CLAUDE_CONFIG_DIR` redirects Claude Code's user-level config dir per-process, *excluding* the real `~/.claude` — CONFIRMED as the isolation mechanism.** **CONFIRMED** (empirically tested **and** maintainer-confirmed): setting `CLAUDE_CONFIG_DIR` to a clean directory provides **full user-level config isolation** — (i) **MCP**: all global/cloud MCPs *and* the user-scoped `trusty-review` disappear ("No MCP servers configured"); **both reads and writes** redirect into the clean dir; **zero leakage** into the real `~/.claude`; (ii) **settings/hooks**: the clean dir's `settings.json` is consulted and the real `~/.claude/settings.json` hooks (PostToolUse/SessionStart/Stop/SubagentStop) are **not** read; (iii) it relocates the **entire `~/.claude/` tree-equivalent** (settings, MCP data, sessions, projects, credentials). So pointing it at the tm-global config dir makes Claude Code merge **that** dir as the user-level layer instead of the real `~/.claude` (the maintainer's claude-mpm global hooks/MCPs are excluded). Empirically validated on Claude Code v2.1.185 (macOS, 2026-06-22) **and confirmed by the maintainer** — this is a settled design assumption, not an open risk. The one residual note: the flag is **undocumented** in `--help`, so re-verify the isolation behavior on **major Claude Code upgrades** (a lightweight regression guard, not a blocking risk). **This is the load-bearing isolation primitive — REQUIRED, primary:** the global hooks + skills/slash-commands + MCPs are delivered through it, and it is what prevents real-`~/.claude` step-on. | **CONFIRMED** — full MCP + settings/hooks isolation, empirically tested on Claude Code 2.1.185 (2026-06-22) **and maintainer-confirmed**. Residual: undocumented in `--help` → re-verify on major CC upgrades (lightweight regression guard). | **WI-1** is a minimal **regression-guard / version-pin** (re-verify the user-level swap + real-`~/.claude` exclusion against the pinned CC version on major upgrades). It is **non-blocking** — the mechanism is confirmed, so implementation can proceed; this WI only guards against a future undocumented-behavior regression. |
| A2 | **`CLAUDE_CONFIG_DIR` does not suppress project-local `.claude/` creation.** Confirmed: a local `.claude/settings.local.json` may still be written in the workspace even with the var set. This is **acceptable** — the managed layout *wants* project-local files under `repo/`; we just must not assume the var centralizes everything. | **Confirmed (acceptable).** | Managed layout treats `repo/.claude/` as the statically-discovered half by design. |
| A9 | **`CLAUDE_CONFIG_DIR` ALSO relocates `~/.claude/.credentials.json` → a clean tm-global config dir starts UNAUTHENTICATED.** Discovered 2026-06-22 / v2.1.185: the var relocates the **entire** `~/.claude/` tree-equivalent (A1) including `.credentials.json`. **KEY INSIGHT (empirically confirmed, 2026-06-22):** primary Claude Max/Pro session auth is the macOS **Keychain** entry keyed by the `CLAUDE_CONFIG_DIR` path — NOT `.credentials.json` (which carries MCP OAuth tokens only). A fresh `~/.trusty-mpm/claude-config/` has no Keychain entry, so `claude` reports "Not logged in". | **Confirmed + MITIGATED by WI-10 (2026-06-22).** | **WI-10 IMPLEMENTED:** two auth paths — **(a) `tm login`** (one-time keychain setup, `claude auth login` under `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config`) and **(b) `ANTHROPIC_API_KEY`+`--bare`** (API-key path, auto-detected by `tm run`). Primary managed-session auth is via the Keychain or API key; seeding `.credentials.json` addresses MCP OAuth tokens only. |
| A3 | **The VS Code / Cursor extension IGNORES `CLAUDE_CONFIG_DIR`** (reads/writes the real `~/.claude/` regardless). | **Confirmed risk — bounds the IDE path.** | Because the load-bearing primitive is `CLAUDE_CONFIG_DIR` (A1) and the IDE ignores it, an IDE-attached session loads project-local config layered on the **real** `~/.claude` (claude-mpm step-on) and does **not** get tm's global hooks/MCPs. IDE-attach (§06) therefore relies **only** on standard project-local discovery of `repo/CLAUDE.md` + `repo/.claude/agents/` + `repo/.claude/skills/`, which the extension *does* honor; faithful, isolated sessions launch via `tm`. WI-1 documents this boundary. |
| A4 | **Project-local `.claude/` precedence over the user-level layer.** Confirmed: precedence is Managed > CLI args > Local (`settings.local.json`) > Project (`.claude/`) > User (the user-level layer — the real `~/.claude`, or the **tm-global config dir** when `CLAUDE_CONFIG_DIR` redirects it). | **Confirmed.** | Relied on by §03/§06/§08 — project-local `repo/.claude` overlays the tm-global user-level layer. |
| A8 | **Claude Code CLI accepts argument-supplied settings + MCP config files (the *secondary* override mechanism).** **Verified** against the installed CLI (v2.1.185, `claude --help`): `--settings <file-or-json>` ("Path to a settings JSON file … to load additional settings from" — the file carries the `hooks` block); `--mcp-config <configs...>` ("Load MCP servers from JSON files or strings (space-separated)"); `--strict-mcp-config` ("Only use MCP servers from `--mcp-config`, ignoring all other MCP configurations"). `--bare`/`--safe-mode` help text confirms hooks/MCPs are otherwise standard-discovery customizations. **Reframed as secondary:** these flags are kept for per-run overrides and for *manually replicating* a tm launch (same arguments) — they are **no longer** the primary delivery mechanism for hooks/MCPs (that is the tm-global `CLAUDE_CONFIG_DIR`, A1). | **Verified (CLI v2.1.185); secondary.** | §03/§08 keep them documented as the optional override/replication path; WI-1 re-confirms against the pinned CC version. |
| A5 | **Deployers are re-targetable away from `dirs::home_dir()`.** The agent/skill deployers already accept a target/dest dir; output-style/hook/trust helpers resolve `home_dir()` internally and must be parameterized. | **Verified (re-targetable; some helpers need a param).** | WI-2/WI-3 thread the config-dir through; WI-7 asserts no global writes. Fail-closed if a helper cannot be re-targeted (§04). |
| A6 | Concurrent `load`/`update` on the same alias could race on the checkout/config. | Risk. | Per-alias advisory lock (file lock in the managed dir); document in impl. |
| A7 | A user already has a non-managed dir where the managed root wants to write. | Risk. | Marker-file guard (§03): refuse without `--force`. |

## 5. Work-Item (WI) Breakdown

> Scopes: **S** ≈ ≤1 day, **M** ≈ 2–4 days, **L** ≈ ≥1 week. To be filed as an epic for maintainer
> review; this spec is the behavior contract the epic's WIs implement.

| WI | Scope | Work | Realizes | Depends on |
|----|-------|------|----------|------------|
| **WI-1** | **S** | **Regression-guard / version-pin `CLAUDE_CONFIG_DIR` + write the standard.** The load-bearing isolation mechanism is **CONFIRMED** (empirically tested on CC 2.1.185, macOS, 2026-06-22, **and maintainer-confirmed**, A1) — full user-level swap to the tm-global config dir with the real `~/.claude` excluded. This WI is therefore a **minimal regression guard, not a blocking prerequisite and not a "does it work" investigation**: pin a known-good minimum CC version and re-verify `CLAUDE_CONFIG_DIR` isolation (user-level swap + real-`~/.claude` exclusion) against that pinned version on **major** Claude Code upgrades (the flag is undocumented, so guard against a future regression). Also: confirm the IDE ignores it (A3) and document that boundary, re-confirm the **secondary** `--settings`/`--mcp-config` override flags (A8), and capture the credential-relocation gotcha (A9) so WI-10's precondition is accounted for. Write the standard doc (one tm-global config dir + per-alias project layout, marker schema incl. the `CLAUDE_CONFIG_DIR` path, ownership, isolation invariant, the project-local-discovery vs tm-global split, the credential precondition) and a conformance checklist incl. session-replay via the same `CLAUDE_CONFIG_DIR`. **Non-blocking:** the build does not wait on this WI to prove the mechanism — the mechanism is confirmed. | SPEC-STANDALONE-MPM-03, -04 (assumptions A1–A4, A8, A9) | — |
| **WI-9** | **M** | **Establish + maintain the single tm-global config dir (`tm install` / `tm config`).** Create and maintain `~/.trusty-mpm/claude-config/` holding the **one** set of global hooks, the global skills / slash-commands, and the global MCP servers (memory/search/review); idempotent merge that preserves user-added entries; this is established **once** (not per project) and is what `CLAUDE_CONFIG_DIR` selects at launch. | SPEC-STANDALONE-MPM-03, -04, -08 | — (mechanism confirmed, A1; WI-1 runs alongside as a non-blocking guard) |
| **WI-10** | **S** | **IMPLEMENTED (2026-06-22, refs #1548).** Managed-session auth — `tm login` (keychain, default) + `ANTHROPIC_API_KEY`/`--bare` fallback (CI/automation). Primary auth uses the macOS Keychain keyed by `CLAUDE_CONFIG_DIR` path; `.credentials.json` is MCP OAuth tokens, NOT session auth. `tm login` → `claude auth login` under `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config` (one-time setup; creates the Keychain entry). `tm run` auto-adds `--bare` when `ANTHROPIC_API_KEY` is set (bypasses Keychain/OAuth). Non-blocking hint when neither is detected. `build_launch_command(api_key: Option<&str>)` and `build_login_command(claude_config_dir)` are unit-testable; `AuthState` enum encodes the three states (ApiKey / CredentialsFile / Unknown). | SPEC-STANDALONE-MPM-03, -05 (A9) | WI-9 |
| **WI-2** | **M** | **Project-local-scope the agent/skill/output-style deploys.** Thread an explicit target through `deploy_agents_filtered`, `deploy_skills_filtered` → `repo/.claude/agents/`, `repo/.claude/skills/` (project, standard discovery; global skills/slash-commands go to the tm-global config dir per WI-9); `deploy_output_style` → `repo/.claude/settings.json` / tm-global config dir; remove the real-`home_dir()` global fallback in managed mode (fail closed). | SPEC-STANDALONE-MPM-04 (sites 1–3) | WI-9 |
| **WI-3** | **S** | **Emit the one global hooks block + trust into the tm-global config dir; stop the real-global writes.** Re-target hook composition (`remove_global_trusty_memory_hooks` + the managed hooks) to write the **single** global `hooks` block into the tm-global config dir's `settings.json` (selected by `CLAUDE_CONFIG_DIR`); re-target `preseed_workspace_trust_home` to the tm-global config dir's `.claude.json` equivalent; **stop** seeding the real `~/.claude.json`. | SPEC-STANDALONE-MPM-04 (sites 4–5), -08 | WI-9 |
| **WI-4** | **M** | **Standalone-driver registry + `register`/`ls`.** New registry file under the trusty-mpm config root; `tm register`, `tm ls`. Reuse/align with the DOC-22 project registry. | SPEC-STANDALONE-MPM-01, -07 (`ls`) | — |
| **WI-5** | **L** | **`load` + project-config generation (clone + project-local config; no per-project hook/MCP regen).** Stable alias-keyed `provision_in` clone; project-local layout (`repo/` with discoverable CLAUDE.md/project-agents/skills + `.trusty-mpm/` metadata + marker recording the tm-global `CLAUDE_CONFIG_DIR` path); idempotent refresh; `core::managed_config` module. `load` **no longer** regenerates per-project hooks/MCPs — those live in the tm-global config dir (WI-9). | SPEC-STANDALONE-MPM-02, -03 | WI-2, WI-3, WI-4, WI-9 |
| **WI-6** | **M** | **Attended `run` + `path`/`update`/`rm`.** Attended-only `tm run` launch with the **required** `CLAUDE_CONFIG_DIR=~/.trusty-mpm/claude-config` and `cwd = repo/` (optional `--settings`/`--mcp-config` override args, secondary); **no `--autonomous` flag** (autonomy belongs to Layer 2 / the session manager); the supporting verbs over the marker. | SPEC-STANDALONE-MPM-05, -06, -07 | WI-5 |
| **WI-7** | **M** | **Isolation-invariant integration tests.** Tests that run `load`/`run`/`update` against a sandboxed `$HOME` and assert **zero** writes to the real `~/.claude/` or `~/.claude.json` (all tm writes land in the tm-global config dir or `<managed-root>/<alias>`); assert global hooks/MCPs live only in the tm-global config dir and apply only when launched with `CLAUDE_CONFIG_DIR` set (and that the project-local half is discoverable in `repo/`); idempotency tests for `load`/`update` and `tm config`; marker-guard + fail-closed tests. | SPEC-STANDALONE-MPM-04 (the invariant) | WI-5, WI-6, WI-9 |
| **WI-8** | **S** | **`trusty-review` MCP wiring into the tm-global config.** Add `inject_trusty_review_mcp`; assemble the trusty triad **once** into the tm-global config dir (WI-9) — applied via `CLAUDE_CONFIG_DIR`. | SPEC-STANDALONE-MPM-08 | WI-3, WI-9 |

**Critical path:** WI-9 → (WI-2 ∥ WI-3 ∥ WI-4) → WI-5 → WI-6 → WI-7. WI-8 and WI-10 ride
alongside WI-9 (WI-10 — credential provisioning — must land before `tm run` is usable, i.e. before WI-6).
**WI-1 is off the critical path:** the `CLAUDE_CONFIG_DIR` mechanism is **CONFIRMED** (A1), so WI-1 is a
non-blocking regression-guard / version-pin + standard-doc task that runs alongside the implementation
WIs — it is **not** a prerequisite that could invalidate the design and does not gate WI-9 or anything
downstream.
**Parallelizable:** WI-2, WI-3, WI-4 after WI-9; WI-8 and WI-10 after WI-9; WI-1 anytime (independent).
**Out of scope (Layer 2):** session-manager-driven autonomy over tm-managed projects (DOC-23 /
session-manager spec) — `tm run` itself stays attended-only.

## 6. Open Questions / Future Work

1. **Managed root location.** `~/.trusty-mpm/projects/<alias>/` vs `~/trusty-mpm-projects/<alias>/`
   (more IDE-discoverable, outside a dotdir)? Pick one in WI-1; both honor the isolation invariant.
2. **`CLAUDE_CONFIG_DIR` viability — RESOLVED (CONFIRMED).** Whether `CLAUDE_CONFIG_DIR` is a viable
   isolation mechanism is **no longer an open question — it is CONFIRMED**: empirically tested on Claude
   Code 2.1.185 (macOS, 2026-06-22) **and maintainer-confirmed** to provide full user-level isolation —
   MCP + settings/hooks + the whole `~/.claude/` tree-equivalent are relocated, with the real
   `~/.claude` excluded (A1). It is the **required, primary** isolation primitive — the tm-global config
   dir (global hooks + skills/slash-commands + MCPs) is delivered through it. The **only residual** is
   that the flag stays **undocumented** in `--help`, so re-check the isolation behavior on **major
   Claude Code upgrades** (the lightweight regression guard in WI-1 — non-blocking). If the behavior
   ever regresses, the fallback is to launch Claude Code under a per-session `$HOME` shim pointing at
   the tm-global config, and/or fall back to the **secondary** `--settings`/`--mcp-config` override args
   (A8) for hooks/MCPs. **Note (A9):** because the var relocates the *entire* `~/.claude/` tree, it also
   relocates `.credentials.json` — a clean tm-global config dir is unauthenticated until credentials are
   provisioned (seed `.credentials.json` or set `ANTHROPIC_API_KEY`+`--bare`; WI-10).
3. **IDE hooks/MCP parity (by design, not a gap).** An IDE-attached or plain-`claude` session in
   `repo/` inherits the project-local half (CLAUDE.md/project-agents/skills) via discovery but
   **deliberately not** tm's global hooks/MCPs (those live in the tm-global config dir reached via
   `CLAUDE_CONFIG_DIR`, which the IDE ignores, A3 — so the IDE instead sees the real `~/.claude`
   step-on, §06). Is an IDE-friendly way to opt into tm's global hooks/MCPs wanted (e.g. a documented
   "launch via `tm`", or a helper that prints the exact `CLAUDE_CONFIG_DIR` / override-arg invocation),
   or is the tm-only contract sufficient? Decide in WI-1/WI-6.
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
- `CLAUDE_CONFIG_DIR` behavior + VS Code-extension exclusion — Claude Code GitHub issues #3833, #30538, #33430 (undocumented surface; CLI-only; the **required, load-bearing** isolation primitive — it swaps the user-level layer to the tm-global config dir and excludes the real `~/.claude`; the IDE ignores it). **CONFIRMED working** — empirically tested on CC 2.1.185 (macOS, 2026-06-22) **and maintainer-confirmed**: full user-level isolation (MCP + settings/hooks + the entire `~/.claude/` tree-equivalent relocated; real `~/.claude` excluded). Because the flag is an **undocumented surface**, re-verify on major CC upgrades (lightweight regression guard, WI-1 — non-blocking). The same test confirmed it also relocates `.credentials.json` (A9), so a clean tm-global config dir is unauthenticated until credentials are provisioned.
- **Verified Claude Code CLI flags (the *secondary* override mechanism), `claude --help`, v2.1.185:**
  `--settings <file-or-json>` ("Path to a settings JSON file … to load additional settings from"; can carry a `hooks` block);
  `--mcp-config <configs...>` ("Load MCP servers from JSON files or strings (space-separated)");
  `--strict-mcp-config` ("Only use MCP servers from `--mcp-config`, ignoring all other MCP configurations");
  `--bare` / `--safe-mode` help text confirms hooks/MCPs are otherwise standard-discovery customizations. Used for per-run overrides / replication only; the primary hooks/MCP delivery is the tm-global `CLAUDE_CONFIG_DIR`.
