# DOC-6 — Per-Tool Contract Conformance + claude-mpm Python Adapter

**Status:** Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md
**Consumes:** [DOC-1](./01-tool-contract.md) (Accepted), [DOC-2](./02-stack-manifest-and-versioning.md) (Accepted)
**Cross-ref:** [DOC-0](./00-naming-and-doc-charter.md), [DOC-3](./03-scope-model.md), [ADR-0007](../../../adr/0007-tool-contract-versioning-and-verb-model.md), [ADR-0008](../../../adr/0008-project-identity-convention.md)

## Purpose

Audit each existing tool against DOC-1, enumerate per-tool gap-closure work, and
specify how the Python-based claude-mpm satisfies the same contract.

This doc is the **conformance layer**: DOC-1 defined *the contract*, DOC-2 defined
*the registry of who must conform*; DOC-6 defines *what each member must change to
conform*, *in what order*, and *how the non-Rust orchestrator (claude-mpm)
satisfies a contract designed around Rust trait impls*. It is the bridge from the
two foundational contracts to the surfaces that consume them (DOC-4 rollup, DOC-5
dispatch) and the validation harness (DOC-10), all of which can only operate on
**conformant** members.

---

## DESIGN

### 1. Conformance model

A stack member is **conformant at `contract_version: N`** when, for every verb it
advertises, it satisfies all of the following (each clause references the DOC-1
decision it derives from):

1. **Emits the uniform envelope** (DOC-1 D3a) — every verb prints the outer JSON
   (`contract_version`, `tool`, `tool_version`, `verb`, `scope`, `status`, `data`,
   `messages`) to **stdout**, with logs on **stderr** (repo hard rule; MCP framing
   owns stdout). The CLI is the authoritative surface (DOC-1 D1); a daemon `GET
   /health` fast-path is optional.
2. **Uses the correct status vocabulary** (DOC-1 D4) — introspection
   (`doctor`/`version`), `config`, and lifecycle verbs use `ok|warn|fail`;
   `health` uses `running|degraded|down`; doctor checks use
   `ok|warn|fail|pending|skipped`. The envelope `status` mirrors the exit code
   (DOC-1 D5: `0` ok/running · `1` fail/down · `2` warn/degraded · `3`
   contract/usage error at the dispatcher boundary).
3. **Advertises `verbs[]`** (DOC-1 D3b) — `<binary> version --json` returns
   `data.verbs` listing exactly the verbs the member actually implements, plus
   `contract_version` and `tool_version`. The controller reads this at runtime and
   dispatches only advertised verbs (DOC-1 D3c generic passthrough); it never
   hard-codes a per-tool verb list (DOC-2 §6 discovery rule).
4. **Carries `contract_version`** — every envelope and the `version` payload
   advertise the integer level the member speaks (DOC-1 D2), `>=` the manifest's
   `min_contract_version` (DOC-2 §3). v1 floor `F = 1`.
5. **Redacts secrets** (DOC-1 D8) — all output (`config` effective values,
   `version.build`, `doctor` remediation hints, `health` detail) masks API keys,
   tokens, AWS credentials, and connection strings with the fixed marker
   `"***redacted***"`.
6. **Tags scope correctly** (DOC-1 D7 / DOC-3) — the `--scope project|system|all`
   flag is honored, the envelope `scope` reflects it, and each doctor/health check
   carries its own per-check `scope`. Single-layer (system-only) members simply
   never emit `project`-scope checks and report project verbs as unsupported via
   `verbs[]` graceful-degrade (DOC-3 §1, Resolved Q2).
7. **Honors the project-identity convention** (ADR-0008 / DOC-3 §8) — every
   project-scoped verb binds to the **full-path slug** of the nearest enclosing
   git root, so a single identity keys all per-project state across tools. See §7.

**Conformance is per-verb, not per-tool.** A member is conformant *for the verbs
it advertises*. A CLI-only member that advertises no lifecycle verbs is fully
conformant; the controller degrades gracefully on the missing verbs (DOC-1 D3).
This is what makes a tiered model meaningful.

#### Conformance tiers

Because verb presence is independent of `contract_version` (DOC-1 D3 critical
split), we define **tiers by which verbs are advertised**, not by contract level.
Tiers are a planning/communication aid for sequencing the retrofit and for the
self-check harness (§8); they are **not** a wire concept — the wire only ever
carries `verbs[]` and `contract_version`.

| Tier | Required verbs | Rationale |
|---|---|---|
| **T0 — Non-conformant** | none (or emits text, not the envelope) | Today's state for every tool. |
| **T1 — Introspection baseline** | `version` + `health` + `doctor` | The minimum that lets the controller *discover and diagnose* a member. `version` is mandatory (it carries `verbs[]`); without it the controller cannot negotiate at all. This is the floor every member — including claude-mpm — must reach. |
| **T2 — Lifecycle** | T1 + `start` + `stop` + `restart` | Daemon members the controller can bring up/down. CLI-only/orchestrator members may legitimately stop at T1 + `config`. |
| **T3 — Full contract** | all seven verbs incl. `config` | The complete DOC-1 surface. Target for the four Rust daemons (search/memory/analyze) and review's daemon-backed config. |

**Mandatory floor:** every member MUST reach **T1**. `version` is non-negotiable —
it is how the controller learns everything else (DOC-2 §6). A member that cannot
emit `version --json` cannot participate in the stack at all.

**Per-member target tiers (recommended):**

| Member | Target tier | Note |
|---|---|---|
| trusty-search | T3 | richest existing surface; the pattern-setter |
| trusty-memory | T3 | daemon; `GET /health` already speaks `degraded` |
| trusty-analyze | T3 | daemon; dependency-aware health already exists |
| trusty-review | T2 (T3 if `config` lands) | system-only daemon (DOC-3); the laggard — net-new CLI verbs |
| claude-mpm (orchestrator) | **T1 + partial** | `doctor/health/version` synthesized by the adapter; lifecycle/`config` degraded or unsupported (§4) |

### 2. Per-tool gap audit + retrofit plan

The audit below **re-confirms** DOC-1's conformance snapshot against the source
tree (2026-06-08; clap command enums + axum health handlers + `trusty_common`
re-checked). The headline holds: **the controller-facing contract is net-new
across the board** — the *commands* often exist but emit human text, not the
envelope; no tool advertises `verbs[]` or emits `contract_version`. A `⚠️` cell
means "there is existing behaviour to wrap," not "almost done."

Cells: `✅` conformant today · `⚠️` exists but text-only / different shape /
partial · `❌` missing. "Lands in" names the concrete file/module the retrofit
touches.

#### 2.1 trusty-search (richest — the pattern-setter)

Sources: `crates/trusty-search/src/main.rs`, `src/commands/` (`doctor.rs`,
`status.rs`, `start.rs`, `stop.rs`, `config.rs`, `serve.rs`, `port.rs`),
`src/service/server/health.rs` (`HealthResponse`).

| Verb | Current state | Required change | Lands in |
|---|---|---|---|
| `doctor` | `⚠️` `doctor [--fix]` — 6 checks as `CheckResult::{Ok,Warn,Error}(String)`, message-only (no `id`/`scope`/`remediation`) | Map each check into `DoctorCheck{id,title,scope,status,detail,remediation}`; add `--json`; roll up via `Dispatcher::doctor_status` | `commands/doctor.rs`, `commands/doctor_checks.rs` |
| `health` | `⚠️` `status` (alias `health`), `status --json`, `GET /health` (`HealthResponse`, status literal `"ok"`) | Map `HealthResponse` → `HealthData`; set envelope `status` to `running|degraded|down`; add `--json` to the CLI verb | `commands/status.rs`, `service/server/health.rs` |
| `version` | `⚠️` clap `--version` flag only; no `verbs[]` | Add a real `version` subcommand emitting `VersionData` with `verbs[]` + `contract_version` | `main.rs` (new `Version` variant), `commands/` |
| `start`/`stop` | `⚠️` `start [...]`/`stop` — text | Emit `LifecycleData{action,previous_state,new_state,...}`; `--json` | `commands/start.rs`, `commands/stop.rs` |
| `restart` | `❌` absent (operators use launchd `bootout`/`bootstrap`) | Net-new: compose stop→start (or `LaunchdConfig::bootout`+`bootstrap`); emit `LifecycleData` | new `commands/restart.rs` |
| `config` | `⚠️` `config get\|set <key> [val]` — **live memory-limit mutation**, NOT effective merged config | Make the read-only effective-config verb (DOC-1 `config.data`) the **default `config`**; move the existing mutation behind `config set`/`config tune` (and `config get <key>` for a single value) — see §2.6 | `commands/config.rs` |
| `version.verbs[]` | `❌` | advertised by the new `version` verb | as above |
| envelope/`contract_version` | `❌` | provided by `trusty_common::contract::Dispatcher` | shared module (§3) |

Reusable today: `port [--json]` (UI discovery, DOC-2), `service install|uninstall`
(launchd), `upgrade [--check|--yes]` (wraps `trusty_common::update`). The richest
`GET /health` body (indexes, embedder, rss, update_available) is the template for
the envelope's introspection payload (DOC-1 grounding).

#### 2.2 trusty-memory

Sources: `crates/trusty-memory/src/main.rs`, `src/commands/doctor.rs`,
`src/web.rs` (`HealthResponse`).

| Verb | Current state | Required change | Lands in |
|---|---|---|---|
| `doctor` | `⚠️` `doctor` — `CheckStatus::{Pass,Warn,Fail}` + `label` + optional detail (richer than search; still no `id`/`scope`/`remediation`); plus a separate palace-audit (`PalaceAuditStatus`) | Map checks + palace audit into `DoctorCheck[]` (palace checks are `scope:"project"`); `--json` | `commands/doctor.rs` |
| `health` | `⚠️` no CLI verb (HTTP only); `GET /health` already `status:"ok"\|"degraded"` + `detail`/`uptime`/`addr` | Add a `health` CLI verb that maps the HTTP body → `HealthData`; this body is the **closest existing match** to the target split | new `commands/health.rs`, `web.rs` |
| `version` | `⚠️` clap `--version` only | Add `version` subcommand with `verbs[]` | `main.rs`, `commands/` |
| `start`/`stop` | `⚠️` `start`/`stop` — text | `LifecycleData`; `--json` | `commands/start.rs`, `commands/stop.rs` |
| `restart` | `❌` | Net-new (graceful drain already implemented per #534) | new `commands/restart.rs` |
| `config` | `❌` | Net-new read-only effective-config verb | new `commands/config.rs` |
| `verbs[]` / `contract_version` / envelope | `❌` | shared module + new `version` | §3 |

Reusable: `port [--json]`, `service`, `upgrade`, graceful shutdown (#534).

#### 2.3 trusty-analyze

Sources: `crates/trusty-analyze/src/main.rs`, `src/commands/`, `src/service/`
(`HealthResponse{status, version, search_reachable}`). Hard runtime dep on
trusty-search.

| Verb | Current state | Required change | Lands in |
|---|---|---|---|
| `doctor` | `⚠️` `doctor [--port]` text; also an MCP `run_diagnostics` tool | Map to `DoctorCheck[]`; `--json`; reuse `run_diagnostics` logic | `commands/` doctor module |
| `health` | `⚠️` `health` CLI verb **already exists** + `GET /health` (`status:"ok"\|"degraded"`, `search_reachable`, 200/503) | Map `search_reachable` → `HealthData.deps[]` (`{id:"trusty-search",required:true,reachable}`); `degraded` ⇔ dep down — a natural model for dependency-aware health | `service/`, `commands/` health |
| `version` | `⚠️` clap `--version` only | Add `version` with `verbs[]` | `main.rs`, `commands/` |
| `start`/`stop` | `⚠️` `start [...]`/`stop [...]` — text | `LifecycleData`; `--json` | `commands/` |
| `restart` | `❌` | Net-new | `commands/` |
| `config` | `❌` | Net-new read-only effective-config (surface `TRUSTY_LLM_MODEL`, region, redacted credentials) | `commands/` |
| `verbs[]` / `contract_version` / envelope | `❌` | shared module + new `version` | §3 |

Reusable: `service install|uninstall|status|logs`, `status`, the dep-aware health
shape (best prior art for `health.data.deps`).

#### 2.4 trusty-review (the laggard — heaviest retrofit)

Sources: `crates/trusty-review/src/main.rs` (clap `Commands` = `Run` / `Compare` /
`Serve` / `Profile` only), `src/service/handlers.rs`
(`HealthResponse{status, version, dry_run, reviewer_model, inference, deps{...}}`
with `compute_status()`; `GET /status{in_flight,last_error?}`).

| Verb | Current state | Required change | Lands in |
|---|---|---|---|
| `doctor` | `❌` no CLI doctor at all | Net-new: synthesize checks (inference reachable? required deps reachable? config present?) from the existing `compute_status()` dep graph | new `commands/doctor.rs` |
| `health` | `⚠️` `GET /health` JSON is the **best existing match** to the target `status` semantics (already `ok\|degraded` + a real `deps{}` block) — but **no CLI verb** | Add a `health` CLI verb mapping `compute_status()` + `deps{}` → `HealthData` | new `commands/health.rs`, reuse `service/handlers.rs` |
| `version` | `❌` clap `--version` flag only | Net-new `version` subcommand + `verbs[]` | `main.rs`, new `commands/` |
| `start`/`stop` | `❌` (only `Serve`; no daemon lifecycle verbs) | Net-new: model `Serve` as the daemon; add `start`/`stop`/`restart` (system-only per DOC-3) | new `commands/` |
| `restart` | `❌` | Net-new | new `commands/` |
| `config` | `❌` | Net-new read-only effective-config (reviewer_model, dry_run, inference endpoint, **redacted** GitHub token / API key) | new `commands/config.rs` |
| `verbs[]` / `contract_version` / envelope | `❌` | shared module + new `version` | §3 |

**trusty-review needs the most work** — it implements *none* of the seven verbs at
the CLI today. Its only contract-relevant surface is the daemon's `GET /health` +
`GET /status`, which is, ironically, the closest existing match to the target
envelope `status` semantics (and `compute_status()` is the prototype for the
DOC-4 rollup). Recommended target: **T2** (it is a system-only member per DOC-3
Q2 — no project layer). `config` (→ T3) is a nice-to-have once the others land.

#### 2.5 claude-mpm (orchestrator, external Python)

Not in this repo. Audited from what is knowable (spec §82; the
`trusty-agents-common` brand adapter): a human-oriented `mpm-doctor`-style
capability and MCP integration exist, but **no machine-JSON contract** — no
envelope, no `contract_version`, no `verbs[]`, no `--json`. Full adapter design in
§4.

| Verb | Current state | Required change |
|---|---|---|
| `doctor` | `⚠️` `mpm-doctor` text (assumed) | Wrap into `doctor.data.checks[]` via the adapter (§4) |
| `health` | `⚠️` process liveness only (assumed) | Map liveness → `running\|down` via the adapter |
| `version` | `❌` | Synthesize `version` envelope with a hand-maintained `verbs[]` |
| `start`/`stop`/`restart` | `⚠️`/`❌` depends on run mode | Degraded/unsupported in v1 — omit from `verbs[]` (§4) |
| `config` | `❌` | Degraded/unsupported in v1 — omit from `verbs[]` |

#### 2.6 The `config` semantics change (call-out)

DOC-1's `config` verb is the **read-only effective merged config** (system +
project per D7), with secrets redacted (D8); editing is an explicit spec non-goal
(DOC-3 §7, Resolved Q3 — the controller's `config` is read/report-only).

trusty-search already has a `config` subcommand — but it is a **live-mutating
`get`/`set`** for daemon memory limits, a *different verb with the same name*.
The retrofit MUST NOT overload it. **Decision (owner-approved):** the contract
`config` (read-only effective merged view) becomes the **default `config` verb**;
trusty-search's existing live-mutating behaviour moves behind explicit
`config set` / `config tune` subcommands, with `config get <key>` reading a single
value. The contract `config` (no subcommand / `--json`) returns `ConfigData`;
`config get`/`config set`/`config tune` remain tool-internal subcommands the
controller *may dispatch* but never treats as the contract verb. This is the only
place the contract verb name collides with existing behaviour, and this
disambiguation resolves it.

### 3. Shared-vs-per-crate retrofit strategy

**Recommendation (strong): put the envelope, types, trait, and dispatcher in a
new `trusty_common::contract` module** (DOC-1 D6), sibling to the existing
`mcp` / `rpc` / `launchd` / `shutdown` / `update` modules (verified present in
`crates/trusty-common/src/`). Build the shared module **first**; per-tool
retrofits then reduce to a mechanical, repeatable shape.

**Why shared, not per-crate:**

- DOC-1 D6 already mandates a single `trusty_common` contract module so all Rust
  tools serialize **byte-identically** — divergent per-crate envelopes would
  defeat the controller's "parse generically" guarantee (DOC-1 D3a).
- The hard plumbing the retrofit needs **already lives in `trusty_common`**
  (verified): `update` (`check_crates_io`, `perform_upgrade`,
  `upgrade_and_restart`, `is_launchd_supervised`), `launchd`
  (`LaunchdConfig::{install,bootstrap,bootout}`), `shutdown` (`shutdown_signal`).
  `restart` and lifecycle data can be composed from these — no new per-crate
  primitive.
- The dispatcher centralizes the easy-to-get-wrong parts: envelope assembly,
  doctor rollup, exit-code mapping, unknown-verb rejection (exit `3`), and
  stdout-only JSON emission (logs on stderr).

**What is net-new vs. reuse:**

- **Net-new:** the entire `trusty_common::contract` module (envelope + per-verb
  `data` structs + `EnvelopeStatus`/`Scope`/`CheckStatus` enums + the
  `ContractTool` trait + `Dispatcher` + a `redact_value` helper — none exists in
  `trusty_common` today). The DOC-1 "API sketch" is the build target.
- **Reuse:** `update`, `launchd`, `shutdown` (above); each tool's existing
  `doctor` checks, `GET /health` structs, `start`/`stop` logic — these supply the
  *data* the typed structs wrap; the retrofit is mostly a *mapping* exercise.

**Minimal per-crate work once the shared module exists** (per Rust tool):

1. `impl trusty_common::contract::ContractTool for <Tool>` — implement
   `tool_id`/`tool_version`/`supported_verbs` (the literal `verbs[]` it advertises)
   and the per-verb async methods, mapping existing data into the typed structs.
2. Add the contract subcommands to the clap enum (`version`, `health`, `doctor`,
   `restart`, `config`; `start`/`stop` already exist — add `--json`), wiring each
   to `Dispatcher::emit`.
3. Map the tool's existing `doctor` `CheckResult`/`CheckStatus` and `GET /health`
   `HealthResponse` into `DoctorCheck`/`HealthData` (the only genuinely
   tool-specific code).
4. Add per-check `scope` tags and the project-identity binding (§7).

**Sequence (recommended):**

1. **Land `trusty_common::contract` + `Dispatcher`** (with unit tests for the
   envelope round-trip, doctor rollup, exit-code mapping). Bump `trusty-common`.
2. **Retrofit trusty-search** — richest existing surface → fastest conformance;
   sets the canonical pattern other tools copy. Target T3.
3. **Retrofit trusty-memory & trusty-analyze** — their `GET /health` already
   speaks the `degraded` vocabulary (memory has `detail`/`uptime`/`addr`; analyze
   has `search_reachable` → `deps[]`). Target T3.
4. **Retrofit trusty-review** — net-new CLI verbs (the laggard). Target T2.
5. **Build the claude-mpm adapter** (§4) — synthesizes the whole surface. Target
   T1 + partial.

This matches DOC-1's "highest-leverage work, in order" and respects the
parallel-worktree discipline: each tool retrofit is an independent worktree once
the shared module is published.

### 4. claude-mpm Python adapter (the core net-new design)

claude-mpm is **external Python**, pluggable per ADR-0006 / DOC-0 A4, with **no
machine-JSON contract** today. The contract is designed around a Rust trait, so
claude-mpm cannot `impl ContractTool` — it must emit the same **JSON shapes** some
other way. Three options:

| Option | What it is | Pros | Cons |
|---|---|---|---|
| **(a) Native** | claude-mpm grows `claude-mpm <verb> --json` in its own repo | Cleanest long-term; no extra process; same binary the manifest already names | Cross-repo work on an external Python project; release-coupled to claude-mpm's cadence; we don't own that repo |
| **(b) Controller-side shim** | A small adapter the controller invokes that maps claude-mpm's existing `mpm-doctor`/CLI output into the contract envelope | No claude-mpm changes; lives in *this* repo (we control it); retired cleanly when trusty-mpm lands | Brittle (parses human output); an extra hop; must track claude-mpm CLI drift |
| **(c) Hybrid** | Native where cheap (`version`/`health`), shim for the rest (`doctor`) | Pragmatic balance; native verbs are stable, shim only where output is messy | Two mechanisms to maintain; split ownership |

**Decision: (b) the controller-side shim for v1, retired wholesale when trusty-mpm
lands** (no native-verb upstreaming — Resolved Decision 4). Rationale:

- **Ownership & velocity.** The shim lives in this monorepo, so DOC-6 can land it
  without a cross-repo dependency on the external claude-mpm release cycle. We
  control its correctness and its tests (DOC-10).
- **It is provably temporary.** The orchestrator is swappable (claude-mpm now →
  trusty-mpm later, DOC-0 A4 / DOC-2 §7). When trusty-mpm ships it `impl`s
  `ContractTool` natively and the shim is *deleted*. Investing in native
  claude-mpm verbs (option a) means improving a component we are actively
  planning to replace.
- **Low surface.** The adapter only needs **T1 + partial** (`doctor/health/
  version`); lifecycle/`config` are degraded. A shim for three read-only verbs is
  small and testable.

**Where the shim lives & how it is invoked.** **Decision (owner-approved):** the
claude-mpm adapter/shim lives in **`trusty_common::contract::orchestrator`** — a
shared submodule shipped in the published `trusty-common` crate, sibling to the
rest of the contract types. The manifest `claude-mpm` entry is
`kind = "orchestrator"`, `binary = "claude-mpm"` (DOC-2 §3). The controller, on
seeing `kind = "orchestrator"` for a non-Rust member, routes its contract calls
through the shim instead of expecting the binary itself to emit envelopes. The shim
shells out to claude-mpm's existing commands (`mpm-doctor`, a version probe, a
liveness probe), parses their output, and assembles the DOC-1 envelope in Rust
using the *same* `trusty_common::contract` types — so the wire output is
byte-identical to a native Rust tool's.

**`verbs[]` advertisement (resolves DOC-1's deferral).** The adapter advertises a
**partial, hand-maintained set**:

```json
{ "tool": "claude-mpm", "tool_version": "<probed>", "contract_version": 1,
  "verbs": ["doctor", "health", "version"] }
```

- `doctor` — wrap `mpm-doctor` output into `doctor.data.checks[]` (each check
  `scope:"system"`; `status` mapped from mpm-doctor's pass/warn/fail).
- `health` — map process liveness to envelope `status` (`running` if the
  orchestrator process answers, `down` otherwise; `degraded` if a probed
  dependency such as the configured MCP servers is unreachable).
- `version` — probe claude-mpm's reported version, emit the `verbs[]` set above.
- `start`/`stop`/`restart`/`config` — **NOT advertised** in v1 (owner-approved).
  claude-mpm's lifecycle is owned by however the user launches it (terminal
  session, not a supervised daemon), so the adapter cannot reliably bounce it;
  `config` is the orchestrator's own concern. The controller degrades gracefully
  (DOC-1 D3) — it simply never offers these verbs for the orchestrator member. The
  adapter therefore advertises exactly `["doctor","health","version"]`.

**Scope mapping.** Per DOC-3 §10, the orchestrator is a **system-layer member**.
The adapter's `doctor`/`health` checks are `scope:"system"`. claude-mpm *may* carry
a thin project layer (per-project `.mcp.json` wiring — the "ensure project"
surface, DOC-3 §10); if the adapter later surfaces project-scoped checks, they use
`scope:"project"` and the full-path-slug identity (§7). v1 keeps the adapter
system-only.

**Install path.** Per DOC-2 §3 / DOC-8 §38 the install source is
`source = "python"` with `tool = "uv"` — claude-mpm is installed and upgraded via
`uv tool install claude-mpm` / `uv tool upgrade claude-mpm`. See §5 for the pin
resolution.

**Cross-repo ownership & sequencing.** This is the one piece of work that touches
(or is constrained by) a repo outside the monorepo:

- The **shim itself** is fully in-repo (no cross-repo dependency) — this is the
  decisive advantage of option (b). It can be authored, tested, and shipped in the
  same PR sequence as the Rust retrofits.
- The shim's **correctness depends on claude-mpm's CLI surface staying stable**
  (the format of `mpm-doctor`, the version probe). The claude-mpm version is pinned
  in the BOM at implementation time (§5) so the shim is tested against a known
  output format, and claude-mpm CLI drift is a shim-maintenance task gated by
  DOC-10's isolation tests.
- Native-verb upstreaming (option a / hybrid) is **deferred (owner-approved)**: no
  cross-repo upstream work is pursued. The shim suffices until trusty-mpm retires
  it (§6), so there is no upstream-ownership question to resolve in v1.

### 5. Resolving DOC-2 Q6 (claude-mpm package/version/changelog pins)

DOC-2 deferred Q6 (the canonical claude-mpm package name, pinned version, and
authoritative `CHANGELOG.md` URL) to DOC-6. All three are now resolved
(owner-approved).

- **Install/upgrade tool — `uv`.** claude-mpm is installed and upgraded via
  `uv tool install claude-mpm` / `uv tool upgrade claude-mpm`. `uv` is the single
  tool — there is no pipx default or uvx override. This is the install path the
  manifest's `install` sub-table records for the orchestrator member
  (`source = "python"`, `tool = "uv"`).

- **Canonical package name — `claude-mpm`.** The manifest `binary`/`package` use
  `claude-mpm`, installed and upgraded via `uv` per the bullet above
  (`uv tool install claude-mpm`).

- **Pinned version — pin at implementation.** DOC-2's worked example uses the
  placeholder `version = "0.0.0"`; the design keeps a placeholder for the manifest
  orchestrator entry. The **concrete version is pinned when the shim is built and
  tested against a specific claude-mpm release** (the shim's parsing is
  version-coupled, §4). When the shim lands, set the BOM pin to whatever claude-mpm
  version DOC-10's isolation test installs and validates, and bump it in lockstep
  with shim updates.

- **Authoritative `CHANGELOG.md` URL —**
  `https://raw.githubusercontent.com/bobmatnyc/claude-mpm/main/CHANGELOG.md`. This
  literal is used as the orchestrator's changelog source (parsed best-effort as
  keepachangelog per DOC-2 §5, degrading gracefully if absent).

### 6. Forward-compatibility (claude-mpm → trusty-mpm)

When `trusty-mpm` (the in-house Rust orchestrator, not yet ready — DOC-0) ships,
conformance becomes native and the adapter retires:

- trusty-mpm `impl`s `trusty_common::contract::ContractTool` **directly** (like any
  other Rust tool) and wires the contract subcommands to `Dispatcher::emit`. It can
  reach **T3** (full lifecycle + config) because it is a supervised Rust daemon.
- The **shim is deleted** — the only orchestrator-specific glue in the system
  disappears; the controller talks to trusty-mpm over the plain contract.
- The swap is a **single manifest edit** (DOC-2 §7): replace the `claude-mpm`
  `[[member]]` with a `trusty-mpm` entry — `kind = "orchestrator"` unchanged,
  `install = { source = "cargo", crate = "trusty-mpm" }`, a `git_tag` changelog,
  and a real `version`/`min_contract_version`. **No controller code changes.**
- Because `kind = "orchestrator"` is continuous, DOC-3's system-layer model and
  DOC-4's rollup treat the new entry identically.

This is the structural payoff of routing orchestrator calls through the contract:
the orchestrator is just another conformant member, and "now Python, later Rust"
is absorbed entirely by the manifest entry (DOC-2) + the swappable adapter (DOC-6).

### 7. Project-identity conformance (ADR-0008 / DOC-3 §8)

Every member's project-scoped verbs MUST bind to the **canonical project id =
full-path slug of the nearest enclosing git root** (`id_from_path`, e.g.
`Users_mac_workspace_my-project`), so one identity keys all per-project state
across tools. The git-root **basename is a display-only alias**.

**The reconciliation work DOC-3 flagged.** The codebase has **two divergent id
schemes** today (both currently registered in the live daemon for the same root):

- `crates/trusty-search/src/detect.rs` — uses the **basename** of the root
  (`my-project`): short, human-friendly, but **collides** across repos that share
  a basename.
- `crates/trusty-search/src/service/fs_discovery.rs::id_from_path` — uses the
  **full-path slug**: collision-free, stable.

ADR-0008 resolved that the **slug scheme wins**; the basename is a display alias.
**Conformance requirement:** as part of trusty-search's retrofit, `detect.rs` and
`fs_discovery.rs` must be **reconciled to a single canonical id** (slug) so that
project-scoped `doctor`/`health` checks and the controller's "ensure project" pass
(DOC-8) key on a consistent identity. trusty-memory's palace id must use the same
slug. Single-layer members (trusty-review) have no project layer and are exempt.

This is a per-tool conformance clause (not just a search bug): every tool that
emits a `scope:"project"` check must derive that project's id via the shared rule,
not its own scheme. **Decision (owner-approved):** hoist project-identity into
`trusty_common`. Expose **`id_from_path` + `detect_project`** from `trusty_common`
as the **single canonical implementation** (full-path slug, per ADR-0008), and have
every tool — and the adapter — consume the shared helper rather than its own. This
eliminates the `detect.rs` basename vs `fs_discovery.rs` slug divergence at the
source: the slug scheme becomes the one shared implementation, and the basename is
retained only as a display alias. This is a **decided retrofit item** — part of the
trusty-search retrofit and a precondition for any tool emitting `scope:"project"`
checks: `detect.rs` and `fs_discovery.rs` are reconciled by replacing both with the
hoisted `trusty_common` helpers.

### 8. Conformance verification

How the controller / test harness checks a member is conformant (feeds DOC-10):

1. **`tctl doctor --self-check <member>`** (recommended) — the controller, for a
   named member, runs `<binary> version --json`, reads `verbs[]` + `contract_version`,
   and then **invokes each advertised verb** and validates:
   - the envelope deserializes into `Envelope<T>` (all required fields present);
   - `contract_version >= min_contract_version` (DOC-2 floor);
   - `status` uses the verb-appropriate vocabulary (D4) and the **exit code
     mirrors it** (D5);
   - `doctor.data.summary` counts match `checks[]`; each check has a valid
     per-check `scope`;
   - no secret-shaped value appears unredacted (a redaction lint on known key
     patterns — `*_api_key`, `*token*`, `AWS_*`, connection strings — asserting
     `***redacted***`);
   - unadvertised/unknown verbs are rejected with exit code `3`.
2. **Round-trip serde test in `trusty_common::contract`** — the shared module
   ships golden-JSON fixtures (DOC-1's worked examples) asserting every type
   round-trips, so a retrofit that drifts the shape fails CI in the shared crate.
3. **Per-tool conformance test** — each retrofitted tool gets a test invoking its
   own contract subcommands and asserting the envelope (the per-crate half of the
   self-check).
4. **claude-mpm adapter test** — DOC-10's isolation harness installs the pinned
   claude-mpm (§5) and runs the shim, asserting the synthesized envelope is valid
   and `verbs[]` is exactly `["doctor","health","version"]`.

`tctl doctor --self-check` doubles as the DOC-10 acceptance gate: a member that
passes it is, by construction, conformant for its advertised verbs.

---

## Dependencies

### Consumes (inputs)
- **DOC-1** (Accepted) — the contract to conform to: the envelope, the seven
  verbs, status enums, exit codes, `version --json` `verbs[]` advertisement, and
  the `trusty_common::contract` module API sketch (trait + types + Dispatcher).
- **DOC-2** (Accepted) — the manifest registry enumerating which members to audit,
  `min_contract_version` pins, the `kind = "orchestrator"` slot, and the deferred
  Q6 (claude-mpm package/version/changelog pins) which DOC-6 now owns (§5).
- **DOC-0** — orchestrator forward-compat (claude-mpm now → trusty-mpm later) and
  `trusty_common` as the home for shared contract code.
- **DOC-3** — system vs project layers; CLI-only/serve-only tools (trusty-review)
  are system-only members; the project-identity convention (ADR-0008) each tool
  must honor (§7). (bidirectional with DOC-1.)

### Produces (consumed by)
- **DOC-4** and **DOC-5** — which can only roll up / dispatch to **conformant**
  tools (the gap audit + tiers define what is dispatchable).
- **Gates DOC-10** — isolation testing can only validate conformant tools; the
  self-check harness (§8) is the acceptance gate.

> These edges match the README dependency graph (DOC-6 consumes DOC-1 + DOC-2;
> produces into DOC-4, DOC-5; gates DOC-10).

## Grounding (exists vs. net-new)

Source-first re-audit, 2026-06-08 (clap command enums + axum health handlers +
`trusty_common` modules re-confirmed against the tree).

- **Confirmed (matches DOC-1's snapshot):**
  - trusty-search is richest: `doctor [--fix]`, `status`/`health`, `start`/`stop`,
    `config get/set` (live mutation), `serve`, `port [--json]`, `service`,
    `upgrade`; `GET /health` `HealthResponse` is the richest body.
  - trusty-memory: `doctor` (`Pass/Warn/Fail` + `label` + palace audit), `start`/
    `stop`, no CLI `health`; `GET /health` already `ok|degraded` + `detail`/
    `uptime`/`addr`.
  - trusty-analyze: `doctor`, **`health` CLI verb already exists**, `start`/`stop`;
    `GET /health` `{status, version, search_reachable}` (200/503); hard dep on
    trusty-search.
  - **trusty-review is the laggard** — clap `Commands` = `Run`/`Compare`/`Serve`/
    `Profile` only; **none** of the seven verbs at the CLI. Its `GET /health`
    (`compute_status()` + `deps{}`) is the closest match to the target `status`
    semantics and the prototype for the DOC-4 rollup.
  - `trusty_common` already provides `update`, `launchd`, `shutdown`, `mcp`, `rpc`
    — but **no `contract` module** (verified `ls crates/trusty-common/src/`).
- **Net-new:**
  - The entire `trusty_common::contract` module (envelope + data types + trait +
    Dispatcher + redaction helper) — the DOC-1 sketch is the build target.
  - A `version` subcommand (with `verbs[]`) on every tool — none exists today.
  - `restart` everywhere (operators use launchd `bootout`/`bootstrap`).
  - The read-only effective-`config` verb everywhere (search's `config` is
    live-mutating; the others have none).
  - All of trusty-review's CLI contract verbs.
  - The claude-mpm Python contract adapter (synthesizes everything) — no machine
    contract exists today.
  - Hoisting the canonical `id_from_path` + `detect_project` into `trusty_common`
    (the single shared slug implementation, ADR-0008) and reconciling the two
    project-id schemes (`detect.rs` basename vs `fs_discovery.rs` slug) onto it.

## Cross-cutting notes

- **Project-identity:** each tool must honor the shared identity convention
  (DOC-3 §8 / ADR-0008 — full-path slug) so project-scoped verbs bind to the right
  cwd; the `detect.rs`/`fs_discovery.rs` divergence is a conformance task (§7).
- **Security / secrets:** no secrets in any contract output — `doctor` remediation
  hints, `health` detail, `version.build`, and `config` dumps must redact with the
  fixed marker `***redacted***` (DOC-1 D8); the self-check harness lints for this
  (§8).
- **Contract-versioning behavior:** the controller accepts any member at
  `contract_version >= min_contract_version` and degrades gracefully on missing
  verbs (DOC-1 D2/D3); the tier model (§1) is a planning aid, never a wire concept.

## Remaining work

**Design (complete):**

- [x] Re-confirm the per-tool current-state audit against the source tree (clap
      enums + health handlers + `trusty_common` modules)
- [x] Define the conformance model + tiers (T0–T3; mandatory T1 floor)
- [x] Per-tool gap audit + retrofit plan (file/module landing sites) for the four
      Rust tools, incl. trusty-review as laggard and the `config` semantics change
- [x] Shared-vs-per-crate strategy (`trusty_common::contract`) + sequence
- [x] claude-mpm adapter design (controller-side shim in
      `trusty_common::contract::orchestrator`) + `verbs[]`
- [x] Resolve DOC-2 Q6 (install tool = `uv`, package = `claude-mpm`, version
      pinned at implementation, changelog URL) — see §5
- [x] Forward-compat (trusty-mpm replaces shim) + project-identity conformance
- [x] Conformance verification design (`tctl doctor --self-check`; ties to DOC-10)
- [x] Owner: resolve the open questions (all 9 approved — see Resolved Decisions)

**Implementation-time (remaining):**

- [ ] *(implementation-time)* Build `trusty_common::contract` + Dispatcher (envelope
      + data structs + enums + `ContractTool` trait + `redact_value`), and hoist the
      canonical `id_from_path` + `detect_project` into `trusty_common` (Q9). Bump
      `trusty-common`.
- [ ] *(implementation-time)* Per-tool retrofits, in order: trusty-search (incl.
      reconciling `detect.rs`/`fs_discovery.rs` onto the hoisted helpers and the
      `config` default-verb split) → trusty-memory & trusty-analyze → trusty-review.
- [ ] *(implementation-time)* Build the claude-mpm shim in
      `trusty_common::contract::orchestrator`; pin the concrete claude-mpm version it
      is tested against (§5) and wire `uv tool install`/`upgrade` into the install
      path.
- [ ] *(DOC-10-owned)* Wire `--self-check` into the isolation harness as the
      conformance acceptance gate.

## Resolved Decisions

All nine questions below were **approved by the owner** (2026-06-08). They are
recorded here for traceability; the DESIGN body above has been updated to read as
decided throughout.

1. **`config` verb disambiguation (trusty-search).** The contract `config`
   (read-only effective merged config) becomes the **default `config` verb**;
   trusty-search's existing live-mutating behaviour moves behind `config set` /
   `config tune`, with `config get <key>` reading a single value. See §2.6 and §2.1.

2. **claude-mpm shim location.** The adapter/shim lives in
   **`trusty_common::contract::orchestrator`** — a shared submodule shipped in the
   published `trusty-common` crate. See §4.

3. **claude-mpm v1 verbs.** The adapter advertises exactly
   **`["doctor","health","version"]`**; lifecycle (`start`/`stop`/`restart`) and
   `config` are **unsupported in v1** (claude-mpm is user-session-owned, not
   daemon-supervised). See §4.

4. **Cross-repo upstreaming ownership.** **Deferred** — no native-verb upstream work
   is pursued; the shim suffices until trusty-mpm retires it (§6). See §4.

5. **Install/upgrade tool — `uv`.** claude-mpm is installed and upgraded via
   `uv tool install claude-mpm` / `uv tool upgrade claude-mpm`. `uv` is the single
   tool (no pipx default, no uvx override) and is the orchestrator install path the
   manifest's `install` sub-table records. See §4 and §5.

6. **Canonical package name — `claude-mpm`**, installed/upgraded via `uv` per #5.
   See §5.

7. **First-BOM pinned version — pin at implementation.** The manifest orchestrator
   entry uses a placeholder version in the design; the concrete version is pinned
   when the shim is built and tested against a specific claude-mpm release. See §5.

8. **Authoritative `CHANGELOG.md` URL —**
   `https://raw.githubusercontent.com/bobmatnyc/claude-mpm/main/CHANGELOG.md`, used
   as the orchestrator's changelog source. See §5.

9. **Hoist project-identity into `trusty_common`.** Expose `id_from_path` +
   `detect_project` from `trusty_common` as the single canonical implementation
   (full-path slug, per ADR-0008), eliminating the `detect.rs` basename vs
   `fs_discovery.rs` slug divergence; each tool consumes the shared helper. Recorded
   as a decided retrofit item in §7.
