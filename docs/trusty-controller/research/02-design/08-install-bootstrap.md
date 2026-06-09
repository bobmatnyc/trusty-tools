# DOC-8 — Install/Bootstrap Flow (UUC1, UUC2)

**Status:** Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md
**Consumes:** [DOC-2](./02-stack-manifest-and-versioning.md) (Accepted), [DOC-3](./03-scope-model.md) (Accepted), [DOC-5](./05-controller-cli.md) (Accepted)
**Cross-ref:** [DOC-4](./04-doctor-health-rollup.md) (Accepted), [DOC-6](./06-contract-conformance-and-mpm-adapter.md) (Accepted), [DOC-9](./09-upgrade-flow.md), [DOC-10](./10-isolation-testing-harness.md), [DOC-0](./00-naming-and-doc-charter.md), [ADR-0008](../../../adr/0008-project-identity-convention.md)

## Purpose

Specify the zero-knowledge install (UUC2) and per-project auto-config on
claude-mpm launch (UUC1), including the Rust-toolchain hard dependency and a
progressive-readiness UX.

This doc owns the **mechanics** of two flows whose **command entry points** DOC-5
already pinned:

- **`tctl install`** (system scope) — the one-time, zero-knowledge system install
  (UUC2): from a vanilla machine to every stack member installed, supervised, and
  contract-verified.
- **`tctl ensure [--scope project]`** (the idempotent ensure-project pass) — the
  per-project auto-config that fires on every claude-mpm launch (UUC1) and
  no-ops when the project is already `ready`.

DOC-8 is a **thin coordinator**, exactly like the rest of `tctl`: it composes
*existing, already-grounded primitives* (`cargo install … --locked`,
`uv tool install`, `trusty_common::launchd`, `trusty_common::claude_config`, the
trusty-search `POST /indexes` + SSE reindex stream) per the manifest (DOC-2) and
the scope model (DOC-3). It contains **zero tool-specific logic** — every member's
install source, binary, and contract verbs come from the manifest + the contract
(DOC-1), never from a compiled-in tool list.

---

## DESIGN

### 1. UUC2 — zero-knowledge system install (`tctl install`)

`tctl install` is the spec's *"simple method to install and configure the stack"*
(spec §156) for a user *"with zero knowledge … of … setup requirements"*. It runs
at **system scope** (DOC-3 §3: `install` is system-only) and is **idempotent**
(DOC-3 §4: a second run is a reported no-op). It is the *entry point* DOC-5 §1.2
names; the mechanics below are DOC-8's.

#### 1.1 The end-to-end flow (vanilla machine → ready stack)

```
tctl install [<member>…]   [--yes]                     # system scope (forced)
        │
        ▼
0. PREFLIGHT — hard dependencies (§5)
   • Rust toolchain + cargo present?  →  if absent: GUIDE + ABORT (§5)
   • uv present (needed for the orchestrator member)?  →  if absent: GUIDE + ABORT (§5)
   • platform detect (macOS vs Linux — §6): selects the supervisor backend
        │
        ▼
1. LOAD MANIFEST                    DOC-2 §2 precedence: system override > embedded default
        │                          → Vec<Member { id, binary, kind, install, version,
        │                                         min_contract_version, depends_on, ui, enabled }>
        ▼
2. SELECT + ORDER MEMBERS          named members, else all `enabled` members.
        │                          Order by `depends_on` (DOC-2 §3) topologically so a
        │                          dependency is installed before its dependent
        │                          (search → analyze → review). System-before-project
        │                          is implicit: install is system-only (§4 / DOC-3 §6).
        ▼
3. PER-MEMBER INSTALL  (sequential; progress on stderr — §7)
        │   for each member, dispatch on `install.source`:
        │     • source = "cargo"   → cargo install <crate> --locked
        │                            (SKIP_UI_BUILD=1 for UI-embedding crates — §1.3)
        │     • source = "python"  → uv tool install <package>   (the orchestrator — §1.4)
        │   idempotent: already-installed & version-ok → reported no-op (§1.5)
        ▼
4. SUPERVISE DAEMON MEMBERS         for kind = "daemon": install + load the launch agent
        │                           (macOS: launchd; Linux: systemd-user / foreground — §6)
        │                           via each member's own `service install` contract surface.
        │                           **Include the controller's own service-install** (the
        │                           trusty-controller daemon itself, restarted last in any
        │                           restart sweep for DOC-7 UI continuity — DOC-5 §7).
        ▼
5. VERIFY EACH MEMBER               run the member's contract introspection verbs (DOC-4):
        │                             `<binary> version --json`  → contract_version ≥ floor (DOC-2)
        │                             `<binary> health  --json`  → running (daemons)
        │                             `<binary> doctor  --json`  → no system-scope `fail`
        │                           orchestrator routed via the DOC-6 shim.
        ▼
6. ROLLUP + REPORT                  render a DOC-4 system-track matrix + verdict.
        │                           partial failure → continue, report, remediate (§7)
        ▼
7. (optional) ENSURE PROJECT        if invoked inside a project dir and --scope all,
                                    chain into the §2 ensure-project pass once the
                                    system layer is ready (DOC-3 §6 ordering).
```

The whole flow is **manifest-driven**: step 3 reads only `install`, step 4 reads
only `kind`, step 5 reads only the contract — there is no `if member == "trusty-search"`
anywhere.

#### 1.2 Ordering guarantee (DOC-3 §6: system → then project)

`install` only ever touches the **system** layer, so the cross-layer ordering
("ensure system → then project") is satisfied by construction: a project resource
(index, palace) is **never** created by `install`. *Within* the system layer,
members are installed in `depends_on` topological order so a dependent never
verifies before its dependency exists (trusty-analyze hard-depends on
trusty-search; trusty-review depends on both — DOC-2 §3 worked example). Cross-tool
the order is otherwise free (independent crates).

#### 1.3 cargo members — the common case (`source = "cargo"`)

Grounding (root `CLAUDE.md` release section; confirmed in-tree): every Rust member
installs via **`cargo install <crate> --locked`**. The manifest's `install`
sub-table already encodes this (DOC-2 §3:
`install = { source = "cargo", crate = "trusty-search" }`), and DOC-9 reuses the
*same* command through `trusty_common::update::perform_upgrade` (verified: it
shells `cargo install <name> --locked`). So `install` and `upgrade` compose the
identical primitive; the only difference is install runs once and upgrade moves a
version pin.

Two CLAUDE.md-grounded subtleties the install flow MUST honor:

1. **UI-embedding crates need `SKIP_UI_BUILD=1`.** trusty-search and trusty-memory
   (and the controller itself, DOC-7) embed a Svelte UI via a `build.rs` that
   invokes `pnpm`. The published crate ships a committed `ui-dist/` bundle, so a
   `cargo install` from crates.io does **not** need pnpm — but the controller
   sets `SKIP_UI_BUILD=1` in the install subprocess environment for these members
   defensively, so a host without pnpm never has `cargo install` fail in the UI
   build step. Which members need it is read from the manifest `ui` sub-table
   (`ui.available = true` ⇒ set `SKIP_UI_BUILD=1`), not hard-coded. (Resolved
   Decision 1: locked for v1.)

2. **macOS cdhash / code-signing caveat — never `cp`, always `cargo install`.**
   The root CLAUDE.md is emphatic: on macOS a plain
   `cp target/release/<bin> ~/.cargo/bin/<bin>` over an existing on-PATH binary
   can leave the kernel's code-signing cache keyed on a stale `cdhash`, so the
   next exec is SIGKILL'd (`EXC_CRASH / CODESIGNING`) before any code runs — it
   looks exactly like an OOM kill but is not. `cargo install` writes to a temp
   path and **renames atomically**, which keeps the cache consistent. **DOC-8's
   rule: the controller installs every cargo member exclusively via
   `cargo install <crate> --locked` and never copies a binary into place.** This
   also means a re-install (or DOC-9 upgrade) that replaces an on-PATH binary is
   inherently cdhash-safe. (The controller's own `tctl` binary is bootstrapped by
   the user's first `cargo install trusty-controller`; `tctl install` installs the
   *other* members.)

#### 1.4 The orchestrator — `claude-mpm` via `uv` (`source = "python"`)

claude-mpm is the **one non-cargo install path** (DOC-2 §3; DOC-6 §5, owner-approved).
It is installed and upgraded with **`uv tool install claude-mpm`** /
`uv tool upgrade claude-mpm` (DOC-6 Resolved Decision 5 — `uv` is the single tool;
no pipx default, no uvx override). The manifest records this as
`install = { source = "python", tool = "uv", package = "claude-mpm" }` and the
controller composes `uv tool install <package>` from that descriptor. `uv` is a
**preflight hard dependency** alongside cargo (§5): if `uv` is absent and the
orchestrator member is in the selection, the controller guides-and-aborts the same
way it does for cargo (§5).

The orchestrator's *contract surface* is synthesized by the DOC-6 shim
(`trusty_common::contract::orchestrator`), so step-5 verification of claude-mpm
routes through the shim (`doctor`/`health`/`version` only — its advertised `verbs[]`).
DOC-8 performs an unpinned `uv tool install claude-mpm` and verifies via the shim;
the concrete pinned version is owned by DOC-6/DOC-10. (Resolved Decision 2: locked for v1.)

#### 1.5 Idempotency (DOC-3 §4: install runs once)

`tctl install` wraps each member's install with a **presence + version check** so a
second run is a reported no-op, never a destructive reinstall:

- **Already installed & version-ok** (the member's `version --json` reports a
  version that satisfies the BOM pin / floor) → skip with
  `"trusty-search 0.24.1 — already installed (BOM-pinned)"`.
- **Installed but below the BOM pin** → this is an *upgrade*, owned by DOC-9, not
  `install`. `tctl install` reports it and points to `tctl upgrade <member>`
  rather than silently moving the version (install vs upgrade boundary — §8).
- **Not installed** → install via §1.3 / §1.4.

`cargo install --locked` is itself near-idempotent (it rebuilds/reinstalls), but
the controller's pre-check avoids the needless rebuild and gives the user a clean
"nothing to do" signal — the spec's lowest-effort goal.

#### 1.6 Verification (ties to DOC-4 / DOC-6)

Verification reuses the contract introspection verbs, not bespoke checks: after
installing a member the controller runs `version`/`health`/`doctor` (DOC-4 §1.2
collection loop) and rolls the system track up via the DOC-4 verdict. A member
that installs but fails `version --json` (cannot speak the contract) is
**contract-incompatible** (DOC-4 §5.2 → `down` + upgrade remediation). A daemon
that installs but won't come up is **down** (remediation: `start`). This makes
`tctl install` end with the *same* matrix the user later gets from
`tctl stack doctor`, so "did the install work?" and "is the stack healthy?" share
one answer shape. DOC-6 §8's `tctl doctor --self-check <member>` is the deeper
conformance gate the DOC-10 harness runs in a VM.

### 2. UUC1 — per-project auto-config (`tctl ensure --scope project`)

UUC1 is the *"lowest possible effort config"* scenario: when claude-mpm launches
in a project directory, *"all relevant tools are auto-configured and usable"*
(spec §147–150). The mechanism is the **idempotent ensure-project pass** — DOC-5
named the entry point (`tctl ensure [--scope project]`, Resolved Decision 5 — no
hidden mutation, explicit entry point); DOC-8 owns the mechanics and the launch
hook (§4) that fires it automatically.

#### 2.1 The check → act → verify shape (DOC-3 §4)

The ensure pass is the literal implementation of DOC-3 §4's ensure shape, run per
member at **project** scope (system layer assumed already installed by §1; if not,
it reports the unmet system precondition and stops — DOC-3 §5, blast radius):

```
tctl ensure --scope project                            # default in a project dir
        │
        ▼
0. RESOLVE PROJECT IDENTITY (§2.2)   detect_project() → nearest .git root → full-path slug
        │                            (ADR-0008): the single id keying index + palace + .mcp.json
        ▼
1. CHECK (read-only; no mutation)    for each two-layer member, read project readiness via the
        │                            contract: configured? exists? fresh?  (DOC-3 §2 project ladder,
        │                            tool-reported per DOC-3 Resolved Q5 — controller never inspects
        │                            the repo/index itself).
        ▼
2. ACT (only the missing steps; each step independently idempotent)
        │   • not configured → patch .mcp.json (claude_config::patch_mcp_server) +
        │                      register project with the daemon (POST /indexes — search;
        │                      palace-create — memory)
        │   • not exists     → create the per-project resource (index / palace)
        │   • not fresh      → trigger an incremental reindex (skips unchanged files)
        ▼
3. VERIFY                            re-read readiness: did the ladder climb, or is it now
                                     `pending` and progressing (indexing — §3)?  Report per-member.
```

Single-layer members (trusty-review, claude-mpm — DOC-3 Q2 system-only) have **no
project layer**; the ensure pass reports their project column `n/a` (DOC-4 §5.3)
and does nothing for them. So "ensure project" only ever touches the two-layer
daemons that have per-project state (search, memory).

#### 2.2 Project identity (ADR-0008 / DOC-3 §8)

The ensure pass keys every per-project resource on the **canonical project id =
full-path slug of the nearest enclosing git root** (`id_from_path`, e.g.
`Users_mac_workspace_my-project`). This is the single identity DOC-3 §8 / DOC-6 §7
mandate so search's `IndexId`, memory's palace id, and the `.mcp.json` entry all
agree. The controller obtains it from the **hoisted `trusty_common::detect_project`
+ `id_from_path`** (DOC-6 Resolved Decision 9 — the canonical slug implementation
both `detect.rs` and `fs_discovery.rs` are reconciled onto). Detection precedence
(DOC-3 §8): `.git` root → tool marker (`.claude/`, `CLAUDE.md`) → cwd fallback
(warn, never refuse). Worktrees get their own id; monorepo subdirs share the
enclosing git root's id.

#### 2.3 Composing existing idempotent primitives (grounding)

The ensure pass adds **no mutation state of its own**; it composes primitives that
are *already* idempotent in the tree (source-first audit, 2026-06-08):

| Step | Grounded primitive | Idempotency proof |
|---|---|---|
| Patch `.mcp.json` | `trusty_common::claude_config::patch_mcp_server(path, key, entry)` (writes via `write_json_atomic`) | **Verified:** returns `Ok(false)` (no write) when the existing entry equals the desired one (`patch_mcp_server_is_idempotent` test); preserves sibling keys (`patch_mcp_server_preserves_other_keys`). |
| Build the entry | `trusty_common::claude_config::mcp_server_entry(command, args)` | Pure constructor (`{command, args}`); deterministic for given inputs. |
| Register the index | trusty-search `POST /indexes {id, root_path}` | **Verified (CLAUDE.md API + `index.rs`):** *"Idempotent: re-registering an existing id returns `created: false` rather than an error."* The CLI `index_one` already treats `created:false` as success. |
| Create / refresh the index | trusty-search `POST /indexes/:id/reindex {force:false}` + the SSE stream | Incremental reindex **skips unchanged files** via sha2 content fingerprints (DOC-3 §2 `fresh` rung; CLAUDE.md "Incremental reindex skip"). `force:false` ⇒ a no-op-ish pass when nothing changed. |
| Create the palace | trusty-memory project-scoped palace-create (its contract `config`/project op) | Palace-per-project model (DOC-3 §1); re-create is a no-op when the palace exists. |

Because each underlying op is idempotent, the *whole* ensure pass is safe to run on
**every** claude-mpm launch — which is exactly what the launch hook (§4) does. When
the project is already `ready`, the CHECK step finds nothing missing, ACT does
nothing, and the pass returns fast (the spec's "no-op when set up" — DOC-3 §4).

There is also a **per-tool precedent worth reusing**: each daemon tool today has a
`setup`/`integrate` one-shot (verified: `trusty-search integrate`/`setup`,
`trusty-memory setup` with `patch_one` → `patch_mcp_server` + `merge_prompt_context_hook`).
The ensure pass is the **cross-tool generalization** of those per-tool setups, fanned
out via the manifest instead of each tool wiring itself independently — which is
precisely the anti-pattern the spec calls out (spec §140: *"user needs to … configure …
each tool on a per project basis"*).

### 3. Progressive readiness UX (the spec's explicit question)

The spec asks directly (spec §150–151): *"trusty-search can be setup immediately
but needs time to index a project. What does waiting for completion look like?"*

#### 3.1 The model: ready-to-use ≠ fully-indexed

DOC-3 §2 already defines the answer's backbone: an unindexed project on a healthy
daemon is **`system: ready, project: pending`** — *not broken*, just in progress.
`pending` is a first-class, positive-trajectory state (DOC-4 §2.0), distinct from
`degraded`/`down`. So "waiting" is **never** an error screen; it is a progress
indicator on a stack that is already usable for everything except the
not-yet-built index.

Two distinct readiness moments the UX names explicitly:

- **Usable now** — the daemon is up, the project is `configured` + `exists`
  (`.mcp.json` patched, index registered, palace created). The user can start
  their claude-mpm session immediately; search just returns fewer/no hits until
  the index fills.
- **Fully ready** — the index has finished its first build (`fresh`), so search is
  at full recall. This is the moment `project: pending → ready`.

#### 3.2 How `tctl` reports progress (reuse the SSE reindex stream)

Grounding (verified in-tree): trusty-search already emits a **real progress
stream** the controller reuses verbatim — no new progress protocol:

- `POST /indexes/:id/reindex` returns immediately with a `stream_url`
  (`/indexes/:id/reindex/stream`), then the daemon streams **SSE**
  (`text/event-stream`) events: `start {total_files}` → `progress {indexed, total,
  current_file}` → `complete {indexed, elapsed_ms}` (or `error {message}`). The
  handler **replays buffered events** to late subscribers, so a hook that connects
  after `start` still sees it.
- `trusty_common::monitor::search_client::ReindexEvent`
  (`Started/Progress/Complete/Failed`) is the **already-shipped typed client** for
  this stream; the monitor TUI renders `indexing: {indexed}/{total_files} ({pct}%)`
  from it (`apply_reindex_event`, verified). The controller reuses this client and
  the same percentage rendering.
- Cheap polling alternative: `GET /indexes/:id/status` returns `chunk_count`, and
  `doctor`/`health` (DOC-4) report the project rung (`pending` vs `ready`) — the
  controller's contract-level signal for "is it done yet" without holding the SSE
  stream open.

#### 3.3 Block vs stream vs background (locked behavior)

The ensure pass kicks off the reindex but **does not block** on full completion by
default. The locked behavior (Resolved Decision 3):

- **Interactive `tctl ensure` (TTY):** kick off the reindex, **stream** the SSE
  progress bar to **stderr** (matching DOC-5 §4.1 / DOC-4: data on stdout, progress
  on stderr; reusing the `indicatif` multi-bar trusty-search already drives in
  `reindex_engine.rs`), and return `0` when the project reaches **usable-now**
  (configured + exists), annotating *"indexing in progress — usable now, fully
  ready in ~N s"*. The user is not held hostage to a multi-minute first index.
- **Launch-hook / non-TTY (§4):** **background** the reindex entirely. The hook
  must be fast and non-blocking (§4.2), so it fires `reindex {force:false}` and
  returns without consuming the stream; progress is observable later via
  `tctl stack doctor` (project `pending`) or the trusty-search UI (DOC-7 link-out).
- **Opt-in blocking:** a `--wait` flag makes `tctl ensure --wait` consume the SSE
  stream to `complete` and only then return — for CI / DOC-10 harness use that wants
  "fully ready" as a gate. Mirrors DOC-4's deferred `--fail-on-pending` idea.

Exit-code consistency: a project `pending` is **exit 0** (DOC-4 §7 Resolved
Decision 5 — project-pending is not broken), so a launch hook that leaves the index
building never reports failure.

### 4. The launch hook (DOC-5 left the hook to DOC-8)

DOC-5 §5 explicitly deferred *"the launch-hook that auto-calls `tctl ensure` on
startup"* to DOC-8. This is the wiring that makes UUC1 zero-effort: the user never
types `tctl ensure` for normal workflows — it fires when claude-mpm launches in a
project dir.

#### 4.1 Mechanism (locked; Resolved Decision 4)

claude-mpm is **external** (a different repo; DOC-6 §4 cross-repo note), so the
hook must not require modifying claude-mpm's core. There is **strong in-tree
precedent**: the trusty-memory/trusty-search `setup` already installs a
**claude-mpm/Claude-Code startup hook** into the settings file
(`merge_prompt_context_hook`, verified in `claude_config` + `setup.rs`) that runs
on session start.

- **Primary: a claude-mpm / Claude Code startup hook** that invokes
  `tctl ensure --scope project --yes` (non-interactive, backgrounded reindex per
  §3.3). The hook is installed **idempotently into the project `.mcp.json` /
  settings both during `tctl install`'s optional ensure step (§1.1 step 7) AND on
  the first manual `tctl ensure`**, reusing the same `claude_config` atomic-write +
  hook-merge primitives the per-tool `setup` already uses (idempotent either way).
  This makes the hook a *one-line shell-out to `tctl`*, so all the real logic stays
  in the controller (single source of truth) and the hook itself never needs to change.
- **Fallback: a thin wrapper / alias.** Where a startup hook is unavailable, a
  `claude-mpm` launch wrapper (or shell alias) that runs `tctl ensure` first, then
  `exec`s the real orchestrator, achieves the same effect. This is the
  lowest-coupling option and works regardless of claude-mpm internals.
- **Not for v1: MCP-triggered ensure.** Triggering the pass from an MCP server call
  (e.g. on first tool use) couples auto-config to a specific tool's MCP lifecycle
  and muddies the "no hidden mutation" line DOC-5 drew. Keep ensure an explicit
  (hook-invoked) command, not a side effect of a search/memory call.

#### 4.2 Idempotency + fast no-op cost (the load-bearing property)

Because the hook fires on **every** launch, its no-op cost must be negligible:

- The CHECK step (§2.1) is **read-only contract calls** (`doctor`/`health`
  `--json` at project scope) — a sub-second probe per two-layer member, run in
  parallel (DOC-4 §1.3). When everything is `ready`, ACT does nothing and the pass
  returns in well under the DOC-4 timeout budget.
- The hook **backgrounds** any reindex (§3.3) so even a *stale* project (needs an
  incremental reindex) does not delay session start — the launch proceeds while the
  index refreshes; `fresh` is reached asynchronously.
- It is **safe to run concurrently / repeatedly** because every ACT primitive is
  idempotent (§2.3): two overlapping launches racing the same `patch_mcp_server` /
  `POST /indexes` both converge (the second is a `created:false` / no-write no-op).

#### 4.3 Cross-repo aspect

The hook *content* is a one-line `tctl ensure` shell-out, so the only claude-mpm
dependency is "it supports a startup hook (or can be wrapped)." All install/ensure
logic lives in `tctl` (this repo). When the orchestrator swaps claude-mpm →
trusty-mpm (DOC-2 §7 / DOC-6 §6), the hook mechanism may change (trusty-mpm, being
in-tree, could call `tctl ensure` natively), but the ensure-pass mechanics (§2)
are unchanged — they are orchestrator-agnostic.

### 5. Rust toolchain hard dependency (detection + absent UX)

The spec lists the Rust toolchain as a **hard dependency** (spec §70–72: *"Rust
tool-chain is installed and functioning correctly. e.g. we need 'cargo' to
bootstrap"*). The controller itself was installed via `cargo install
trusty-controller`, so by the time `tctl` runs, cargo was present *at least once* —
but it may have been removed, or `uv` may be absent for the orchestrator.

**Detection (preflight, §1.1 step 0):**

- **cargo / rustup:** probe `cargo --version` (and `rustc --version`) on PATH. This
  is required for every `source = "cargo"` member.
- **uv:** probe `uv --version` on PATH. Required only when the orchestrator member
  (`source = "python"`) is in the selection.

**When a hard dependency is absent — GUIDE and ABORT (Resolved Decision 5):**

The controller **does not attempt to auto-install** a toolchain in v1. Rationale:
auto-installing rustup/cargo (or `uv`) means running a remote install script
(`https://sh.rustup.rs`, `astral.sh/uv`) as a side effect of `tctl install`, which
is a trust/security surface (arbitrary remote code) inconsistent with the
manifest's "no code execution from data" stance (DOC-2 §8) and surprising for a
"thin coordinator." Instead, `tctl install` **fails fast with an actionable,
copy-pasteable remediation** and a non-zero exit:

```
$ tctl install
✗ Rust toolchain not found (cargo is required to install stack members).
  Install it, then re-run `tctl install`:
    curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh
  (see https://rustup.rs).  trusty-controller does not install toolchains for you.
```

(Same shape for a missing `uv`: point at `https://astral.sh/uv`.) This is the
guide-and-abort approach for v1. A future opt-in `tctl install --bootstrap-toolchain`
(which *would* run the official installer with an explicit consent prompt) is
deferred beyond v1. Exit code is the controller-boundary **`3`** (DOC-5 §4.3 — a
precondition/usage failure produced before any member runs), distinct from a
member `down`.

### 6. Platform specifics (MUC2: macOS primary, Linux secondary)

The spec (spec §172–173) makes **macOS the primary** install target and **Linux
secondary but important**. The install flow diverges only at the
**daemon-supervision** step (§1.1 step 4); install (§3) and ensure (§2) are
platform-identical.

| Concern | macOS (primary) | Linux (secondary) |
|---|---|---|
| Daemon supervisor | **launchd** — `trusty_common::launchd::LaunchdConfig` (`install` writes the `~/Library/LaunchAgents/<label>.plist`, `bootstrap`/`bootout` load/unload into `gui/$(id -u)`). Verified: each tool's `service install` composes exactly this. | **No launchd.** Today the tools' `service install` prints *"Skipping launchd install (not macOS) — use your distro's service manager"* (verified, trusty-memory `setup.rs`). Recommended v1: install a **systemd user unit** (`systemctl --user`) where present, else fall back to documenting `daemonless / foreground` (`<binary> serve` / `start` run by the user or a process manager). |
| Binary install | `cargo install … --locked` (atomic rename — cdhash-safe, §1.3) | `cargo install … --locked` (same; the cdhash caveat is macOS-only). |
| cdhash / code-signing caveat | **Applies** — never `cp` over an on-PATH binary (§1.3); `cargo install` is mandatory. | N/A (no kernel cdhash cache). |
| Restart convention | `launchctl bootout` (SIGTERM, graceful drain #534) then `bootstrap` (CLAUDE.md / DOC-5 §7). | systemd `restart`, or stop+start the foreground process. |

DOC-8 keeps the platform branch **thin and at the supervisor layer only**: the
controller selects the supervisor backend from the detected OS (§1.1 step 0) and
otherwise dispatches each member's own `service`/lifecycle contract verb, which is
where the per-OS knowledge already lives. **The deep platform matrix (exact
systemd unit content, Linux port/data-dir conventions, container nuances) is
deferred to DOC-10** (isolation harness, MUC1/MUC2), which must exercise both
OSes; DOC-8 only fixes the *shape* of the divergence. (Resolved Decision 6: Linux v1
target is systemd-user where available, foreground fallback.)

### 7. Failure / partial-install handling (Resolved Decision 7)

A member can fail to install (cargo build error, network, missing toolchain mid-run)
or fail to verify (installs but won't start / can't speak the contract). The locked
flow behavior:

- **Continue, don't abort, by default.** A failure on one member does **not** abort
  the remaining members (they may be independent — a failed trusty-review should not
  block a working trusty-search). The controller records the failure, continues, and
  reports a **DOC-4 system-track matrix** at the end with the failed member as `down`
  (or `contract_incompatible`) and its remediation. This maximizes the
  zero-knowledge user's chance of a mostly-working stack from one command.
  - *Exception — dependency ordering:* if a member's **hard dependency** failed
    (DOC-2 `depends_on`), its dependents are reported `blocked-by` the root (DOC-4
    §5.4 dependency clustering, "represent once") rather than each independently
    re-attempted-and-failed. E.g. a failed trusty-search install surfaces
    trusty-analyze/review as blocked, with the single remediation "fix trusty-search."
- **Non-zero exit on any system failure.** `tctl install` exits `1` (DOC-5 §4.3) if
  any member's final system verdict is `down`, so a scripted / DOC-10 install fails
  loudly. A pure project-`pending` (index still building) is **exit 0** (§3.3).
- **Remediation ties to DOC-4 / DOC-9.** Each failed cell carries the DOC-4
  synthesized remediation: *missing* → `tctl install <member>`; *down* →
  `<binary> start`; *older/below-floor contract* → `tctl upgrade <member>` (DOC-9).
- **Idempotent re-run is the recovery path.** Because install is idempotent (§1.5)
  and ensure is idempotent (§2.3), the remediation for almost any partial failure is
  simply **re-run the same command** — already-good members are no-ops, and only the
  previously-failed member is retried. This is the spec's low-effort recovery and the
  property DOC-10 leans on to test install convergence in a clean VM.

### 8. Relationship to DOC-9 (install vs upgrade boundary)

DOC-8 (install) and DOC-9 (upgrade) share the **same primitive**
(`cargo install <crate> --locked` / `uv tool install` — DOC-2 §3,
`trusty_common::update`) but own **disjoint situations**, and the boundary is sharp:

| | **DOC-8 — install** | **DOC-9 — upgrade** |
|---|---|---|
| Situation | member **not present** (or present and version-ok) | member **present but below the target BOM pin** |
| Trigger | `tctl install` (one-time, UUC2) + `tctl ensure` (per-project, UUC1) | `tctl upgrade` / `tctl updates` (UUC3) |
| Version motion | installs the BOM-pinned version; never moves an existing pin | moves installed → target pin; restarts so the new version takes effect |
| Take-effect restart | install brings the daemon up the first time (§1.1 step 4) | upgrade **restarts** the daemon (DOC-9, `upgrade_and_restart`) |
| Idempotent no-op | already-installed & version-ok (§1.5) | already-at-target (DOC-9) |

The clean rule: **`tctl install` never performs a version transition.** If
`install` finds a member installed-but-stale, it reports the gap and defers to
`tctl upgrade` (§1.5) rather than silently upgrading — so "I asked to install" never
surprises the user with a version bump and a daemon bounce. DOC-4's remediation
surfacing routes stale/older-contract cells straight to the DOC-9 flow.

---

## Dependencies

### Consumes (inputs)
- **DOC-2** (Accepted) — the manifest/BOM the install flow reads: per-member
  `install` descriptor (`source = "cargo"|"python"`, `crate`/`tool`/`package`),
  `kind` (which members get a launch agent), `version`/`min_contract_version`
  (idempotency + verify), `depends_on` (install ordering + failure clustering),
  `ui` (the `SKIP_UI_BUILD` signal), and the embedded-default → system-override
  precedence. The embedded default BOM is what makes a fresh
  `cargo install trusty-controller` immediately able to `tctl install` with no
  network fetch (DOC-2 §2).
- **DOC-3** (Accepted) — the scope model: ensure-system-then-project ordering (§6),
  the idempotency contract (install once / ensure every launch, check→act→verify),
  blast radius (project ops never escalate to system), the project ladder +
  `pending` semantic (progressive readiness), and the project-identity convention
  (ADR-0008 full-path slug) the ensure pass keys on.
- **DOC-5** (Accepted) — the command entry points DOC-8 implements: `tctl install`
  (system), `tctl ensure [--scope project]` (the named idempotent pass),
  `--yes`/non-TTY behavior, the blast-radius warn, and stdout=data / stderr=progress.

### Produces (consumed by)
- **DOC-10** — the isolation testing harness drives `tctl install` (and the
  ensure pass) in a vanilla container/VM and asserts on the verification matrix /
  exit codes; the `--wait` ensure variant (§3.3) is the harness's "fully ready"
  gate. DOC-8's "runnable non-interactively, side-effect-scoped" requirement is the
  contract DOC-10 exercises.

> These edges match the README dependency graph (DOC-8 consumes DOC-2 + DOC-3 +
> DOC-5; produces into DOC-10).

## Grounding (exists vs. net-new)

Source-first audit, 2026-06-08 (trusty-search MCP search + Read against the tree).

| Area | Reality today | Install/bootstrap implication |
|---|---|---|
| **cargo install** | Root CLAUDE.md release section: `cargo install --path … --locked` / `cargo install <crate> --locked`; UI crates need `SKIP_UI_BUILD=1`; the macOS cdhash/codesign caveat (atomic install, never `cp`). `trusty_common::update::perform_upgrade` already shells `cargo install <name> --locked`. | §1.3: the install primitive **exists** and is shared with DOC-9; DOC-8 only composes it per-manifest + sets `SKIP_UI_BUILD`. |
| **launchd** | `trusty_common::launchd::LaunchdConfig` (`install` writes plist, `bootstrap`/`bootout` into `gui/$(id -u)`); each tool's `service install` (verified: search/memory/analyze) composes it; graceful-drain restart (#534). | §1.1 step 4 / §6: daemon supervision **exists** per-tool; DOC-8 dispatches each member's `service install` verb. |
| **`.mcp.json` wiring** | `trusty_common::claude_config::{mcp_server_entry, patch_mcp_server, write_json_atomic}` — `patch_mcp_server` is **verified idempotent** (no-write on identical entry; preserves siblings). `trusty-search integrate`/`setup`, `trusty-memory setup` (`patch_one` + `merge_prompt_context_hook`) already patch project config. | §2.3: the ensure-project config primitive **exists** and is idempotent; the ensure pass generalizes the per-tool `setup` cross-tool. |
| **Project register (idempotent)** | trusty-search `POST /indexes` returns `created:false` on re-register (verified CLAUDE.md API + `index_one`); incremental reindex skips unchanged files (sha2). | §2.3: index creation + freshness are idempotent; the ensure pass is safe to fire on every launch. |
| **Progressive-readiness signal** | trusty-search `POST /indexes/:id/reindex` → `stream_url`; SSE `start/progress/complete/error` with **replay buffer**; typed client `trusty_common::monitor::search_client::ReindexEvent`; TUI renders `indexed/total (pct%)`. `GET /indexes/:id/status` → `chunk_count`. `<binary> port [--json]` / `port.lock` for discovery. | §3: the "waiting" UX **exists** as a real stream; DOC-8 reuses the client + `indicatif` bars; no new progress protocol. |
| **uv (orchestrator install)** | DOC-6 §5 (owner-approved): `uv tool install claude-mpm` is the single Python install path. | §1.4: the one non-cargo path; `uv` is a preflight hard dep. |
| **Startup hook precedent** | `merge_prompt_context_hook` (verified) installs a claude-mpm/Claude-Code startup hook into settings via `claude_config`. | §4: the launch-hook mechanism **has in-tree precedent**; DOC-8 makes it a one-line `tctl ensure` shell-out. |
| **The controller install/ensure orchestration** | **Net-new.** No `tctl`, no cross-tool install loop, no manifest-driven ensure pass, no launch-hook-calls-`tctl-ensure` wiring exists today. | This document. |

## Cross-cutting notes

- **Isolation-testability (DOC-10):** the whole flow must be runnable
  **non-interactively** (`--yes`, backgrounded reindex, `--wait` for the gate) and
  **side-effect-scoped** (per-project state keyed on the path-slug id; system state
  under the OS data dirs) so DOC-10 can exercise it in a clean VM/container without
  contaminating a host. Guide-and-abort on a missing toolchain (§5) keeps the flow
  deterministic in a minimal image (the harness pre-provisions cargo/uv).
- **Security / no remote code from data:** the controller composes install commands
  from the fixed `source` templates (`cargo install …`, `uv tool install …`) and
  never executes a free-form command string from the manifest (DOC-2 §8). It does
  **not** auto-run remote toolchain installers in v1 (§5).
- **Zero tool-specific logic (spec §83):** every install/ensure step reads a manifest
  field (`install`, `kind`, `ui`, `depends_on`) or a contract verb (`version`/
  `health`/`doctor`); no member name is compiled in. Orchestrator swap (claude-mpm →
  trusty-mpm) changes only the manifest entry (`source` cargo vs python) and the hook
  flavor — not the flow.
- **macOS cdhash caveat is load-bearing:** §1.3's "always `cargo install`, never `cp`"
  is not a style preference — a `cp` can produce a SIGKILL that mimics an OOM and
  silently breaks the installed stack. Surfaced here so the implementation never
  introduces a copy-into-PATH shortcut.

## Remaining work

- [x] Specify the UUC2 zero-knowledge system-install flow (`tctl install`):
      preflight → manifest → ordered per-member install (cargo / uv) → supervise →
      verify → rollup (§1)
- [x] Address the cargo mechanics: `--locked`, `SKIP_UI_BUILD`, the macOS
      cdhash/codesign caveat (atomic install, never `cp`) (§1.3)
- [x] Specify the orchestrator install via `uv tool install claude-mpm` (§1.4)
- [x] Specify the UUC1 ensure-project pass (`tctl ensure --scope project`):
      identity → check → act → verify, composing idempotent primitives (§2)
- [x] Specify the progressive-readiness UX (usable-now vs fully-ready; reuse the
      SSE reindex stream; block/stream/background) (§3)
- [x] Recommend the launch-hook mechanism (startup hook → `tctl ensure`; wrapper
      fallback) + fast idempotent no-op + cross-repo aspect (§4)
- [x] Specify the Rust-toolchain hard-dep detection + guide-and-abort UX (§5)
- [x] Specify platform divergence (macOS launchd primary / Linux secondary),
      deferring the deep matrix to DOC-10 (§6)
- [x] Specify failure / partial-install handling + idempotent re-run recovery (§7)
- [x] Fix the install-vs-upgrade boundary with DOC-9 (§8)
- [x] **Owner: resolve all 7 open questions** → **Resolved Decisions (all owner-approved)**
- [ ] Team review → Ready
- [ ] *(implementation-time)* build the install/ensure orchestration in
      `crates/trusty-controller/src/` (dispatch the `install`/`service`/contract
      verbs per manifest; reuse `trusty_common::{claude_config, launchd, update,
      monitor::search_client, detect_project}`)
- [ ] *(DOC-10-owned)* wire `tctl install` + `tctl ensure --wait` into the
      isolation harness as the install acceptance gate

---

## Resolved Decisions

1. **`SKIP_UI_BUILD` signal — derived from `ui.available` (§1.3).** (Owner-approved)
   For v1, the controller derives "set `SKIP_UI_BUILD=1` for this member" from the
   manifest `ui` sub-table (`ui.available = true`). This is correct today — every
   UI-embedding crate (trusty-search, trusty-memory, trusty-controller) is exactly
   the set that needs the flag. An explicit `install.skip_ui_build` field may be
   added to DOC-2's `install` sub-table in a future version only if a member's
   UI-presence diverges from its install-time build need. **The controller's own
   service-install is included in step 4 of the UUC2 flow (§1.1), restarted last
   in any stack restart to maintain DOC-7 UI continuity (DOC-5 §7).** Locked for v1.

2. **claude-mpm pinned version for install/verify — unpinned in DOC-8 (§1.4).** (Owner-approved)
   DOC-8 install treats the orchestrator as "install latest via `uv tool install
   claude-mpm`, then verify via the shim" in v1. The concrete pinned version is
   owned by DOC-6/DOC-10, not DOC-8. When DOC-10 pins a tested release, that
   version flows into the manifest's `version` field; DOC-8 does not carry a separate
   pin. Locked for v1.

3. **Default blocking behavior of `tctl ensure` — return at usable-now + `--wait` opt-in (§3.3).** (Owner-approved)
   Interactive `tctl ensure` (TTY) returns at *usable-now* (project configured +
   exists) while streaming reindex progress to stderr; the launch hook backgrounds
   the reindex entirely (non-TTY, fast no-op cost). An opt-in `--wait` flag makes
   `tctl ensure --wait` block until the index reaches `fresh` (for CI / DOC-10 use).
   Project-`pending` always exits 0. Locked for v1.

4. **Launch-hook mechanism — primary startup hook + idempotent install timing (§4).** (Owner-approved)
   Primary = a claude-mpm / Claude-Code **startup hook** that shells out to
   `tctl ensure --scope project --yes` (installed idempotently via the existing
   `claude_config` hook-merge primitive, reusing the `merge_prompt_context_hook`
   precedent); fallback = a launch wrapper/alias. NOT MCP-triggered in v1. The hook
   is installed **both during `tctl install`'s optional ensure step AND on first
   `tctl ensure`** (idempotent either way). Locked for v1.

5. **Missing Rust toolchain / `uv` — guide-and-abort (§5).** (Owner-approved)
   v1 = **guide and abort** (fail fast with copy-paste install commands +
   non-zero exit `3`); the controller does NOT run remote toolchain installers as
   a side effect of `tctl install`. A future opt-in `tctl install --bootstrap-toolchain`
   (explicit consent) may run the official rustup/uv installer, but is deferred.
   Locked for v1.

6. **Linux daemon supervision target for v1 — systemd user (§6).** (Owner-approved)
   macOS uses launchd (grounded). Linux v1 installs a **systemd user unit** where
   `systemctl --user` is available, falling back to documented foreground/daemonless
   (`<binary> serve`) otherwise. The exact unit content and Linux path/port
   conventions are deferred to DOC-10 (isolation harness). Locked for v1.

7. **Partial-install behavior — continue and report (§7).** (Owner-approved)
   **Continue** on member failure (install all selected members, report failures in a
   DOC-4 matrix, exit `1` on any system `down`), with dependency-aware clustering so
   a failed dependency surfaces its dependents as `blocked-by` rather than
   independently failing. Idempotent re-run is the recovery path. Locked for v1.
