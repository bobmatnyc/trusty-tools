# DOC-5 — Controller CLI Command Surface + Dispatch

**Status:** Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md
**Consumes:** [DOC-1](./01-tool-contract.md) (Accepted), [DOC-2](./02-stack-manifest-and-versioning.md) (Accepted), [DOC-3](./03-scope-model.md) (Accepted), [DOC-4](./04-doctor-health-rollup.md) (Accepted)
**Cross-ref:** [DOC-0](./00-naming-and-doc-charter.md), [DOC-6](./06-contract-conformance-and-mpm-adapter.md) (Accepted)

## Purpose

Define the controller's own CLI (the spec's example operations) plus the
manifest-driven dispatch that fans verbs out to the stack's tools.

`tctl` (DOC-0) is a **thin coordinator**: its entire command surface is sugar
over one mechanical loop — *read the manifest (DOC-2) → for each relevant member,
invoke its contract verb (DOC-1) at the right scope (DOC-3) → collect the
envelopes → roll them up / render (DOC-4)*. This document specifies that command
surface, proves the dispatch engine contains **zero tool-specific logic** (spec
§83), and pins the clap structure that implements it. It owns the **command entry
points**; the *mechanics* of the heavyweight flows live in DOC-8 (install) and
DOC-9 (upgrade), and the rollup math lives in DOC-4.

---

## DESIGN

### 1. Command surface

`tctl`'s surface has exactly **two layers**, and the first is mechanically
generated from the second:

1. **The generic passthrough** `tctl <tool> <verb> [args]` — DOC-1 D3c. Forwards
   any **advertised** verb to a manifest member and renders the returned envelope.
   This is the substrate; everything else is sugar over it.
2. **First-class commands** — ergonomic aliases that fan a verb across *all*
   relevant members and roll up the result (`tctl stack doctor`, `tctl upgrade`,
   `tctl restart`, …). Each is defined purely as "run this verb over this set of
   members at this scope, then render via DOC-4." No first-class command knows
   anything about a specific tool.

#### 1.1 The command tree

```
tctl
├── install [<member>…]          install stack (or named members) — system        → DOC-8
├── upgrade [<member>…]          upgrade stack (or named members) to BOM pins      → DOC-9
│   └── (alias: update)          `update` == `upgrade`; see §1.3 on the verb pair
├── updates                      LIST available updates + changelog headlines (read-only) → DOC-9 / DOC-2 §5
├── ensure [--scope project]     idempotent ensure-project pass (no-op if ready)   → DOC-8
├── start   [<member>…]          start daemon(s) — system
├── stop    [<member>…]          stop daemon(s) — system
├── restart [<member>…]          restart all daemons + UI services — system        → §7
├── stack
│   ├── health                   FAST liveness sweep → tools×scope matrix + verdict → DOC-4
│   └── doctor  [<member>]       DEEP diagnostic sweep → matrix + drill-down + verdict → DOC-4
├── config  [<member>…]          read-only effective merged config (redacted)       → DOC-3 §7
├── status                       one-line stack summary (verdict + version)         (sugar over stack health)
├── port                         report the controller's own UI/daemon port         → DOC-7
├── doctor  --self-check <member>   conformance self-check of one member             → DOC-6 §8
├── version                      tctl's own version + embedded stack_version + contract floor
├── ui                           open / report the controller web UI URL             → DOC-7
└── <tool> <verb> [args]         GENERIC PASSTHROUGH — any advertised verb (DOC-1)   → §2
```

Every node above resolves to the same dispatch primitive (§2). The `stack`
sub-group exists only to disambiguate the **stack-wide rollup** verbs (`stack
health`, `stack doctor`) from a per-member `health`/`doctor` reached via
passthrough — see §1.4.

#### 1.2 One-line semantics

| Command | Scope default | Does what (one line) | Owner doc |
|---|---|---|---|
| `tctl install [m…]` | system | Install missing members from the manifest `install` descriptor; idempotent (already-installed = no-op). | DOC-8 |
| `tctl upgrade [m…]` | system | Move installed members to the BOM-pinned versions; restart so the new version takes effect. | DOC-9 |
| `tctl updates` | all | **List** (read-only) which members have a newer pinned/available version, with changelog headlines between current and target. | DOC-9, DOC-2 §5 |
| `tctl ensure [--scope project]` | project | Idempotent ensure-project pass: run only if not already set up; no-op when ready. Explicit entry point for the setup automation. | DOC-8 |
| `tctl start [m…]` | system | Bring member daemon(s) up (lifecycle `start`). | DOC-1 |
| `tctl stop [m…]` | system | Take member daemon(s) down (lifecycle `stop`). | DOC-1 |
| `tctl restart [m…]` | system | Bounce all daemon members **and the controller's own UI service** (lifecycle `restart`). | §7, DOC-1 |
| `tctl stack health` | all | Fast liveness sweep → tools×scope matrix + stack verdict. | DOC-4 |
| `tctl stack doctor [m]` | all | Deep diagnostic sweep → matrix + per-check drill-down + remediation. | DOC-4 |
| `tctl config [m…]` | all | Render each member's effective merged config (read-only, secrets redacted). | DOC-3 §7 |
| `tctl status` | all | One-line "stack 2026.06-1 — verdict: ready". | DOC-4 (sugar) |
| `tctl port` | system | Print the controller's own bound port/address (clean stdout). | DOC-7 |
| `tctl doctor --self-check <m>` | n/a | Validate one member conforms to the contract (envelope, vocab, redaction). | DOC-6 §8 |
| `tctl version` | n/a | tctl version + embedded `stack_version` + `contract_floor` (`--json` → capability discovery). | DOC-1 D2/D3 |
| `tctl ui` | system | Print/open the controller web-UI URL (discovered via `port`). | DOC-7 |
| `tctl <tool> <verb> [args]` | per DOC-3 §3 | Forward an advertised verb to one member; render its envelope verbatim. | §2 |

#### 1.3 `update` vs `upgrade` — one verb, two surfaces (decision)

The spec lists both *"show available updates …"* (§90) and *"upgrade stack"*
(§90). These are **two distinct operations** that share a root word, which is a
classic CLI footgun. Decision (drafted — see Open Question 1):

- **`tctl updates`** (noun, plural) — the **read-only listing**: "what *could* I
  upgrade, and what changed?" Renders the per-member installed-vs-target diff plus
  changelog headlines (DOC-2 §5, DOC-9). Never mutates anything.
- **`tctl upgrade [m…]`** (verb) — the **mutating action**: actually move members
  to the target pins and restart (DOC-9). `tctl update` is accepted as a hidden
  alias of `upgrade` (clap `visible_alias`/`alias`) so muscle-memory `update`
  does the obvious thing, but the canonical, documented verb is `upgrade` and the
  canonical listing is `updates`. `tctl upgrade --check` (matching trusty-search's
  existing `upgrade --check`/`--yes` surface, verified) is an equivalent
  shortcut for the listing for users who think of it as "upgrade, but dry-run."

This keeps the noun/verb split unambiguous while honouring the spec's exact
phrasing.

#### 1.4 Per-tool ops: `tctl <tool> doctor`, NOT `tctl doctor --tool <x>` (decision)

For a *single* member's introspection, the surface is the **generic passthrough**
`tctl <tool> doctor` (sugar over `tctl trusty-search doctor`), **not** a
`--tool <x>` selector on a top-level `doctor`. Decision (drafted — see Open
Question 2). Rationale:

- It falls straight out of the passthrough (§2): `tctl trusty-search doctor`
  is *already* the contract call; no extra flag plumbing.
- The top-level `doctor` slot is reserved for the **controller self-check**
  (`tctl doctor --self-check <member>`, DOC-6 §8) — a controller capability, not
  a member verb.
- The **stack-wide** rollup is `tctl stack doctor` (DOC-4). So the three are
  cleanly separated: `tctl stack doctor` (whole stack), `tctl <tool> doctor`
  (one member, raw envelope), `tctl doctor --self-check <tool>` (conformance
  audit). `tctl stack doctor <member>` (DOC-4 §3.2) is the rolled-up *single
  member* view (matrix + drill-down), distinct from the raw-envelope passthrough.

#### 1.5 Global flags

Matching the repo's clap convention (verified: trusty-search declares `--json`,
`-v/--verbose` as `#[arg(long, global = true)]` on its root `Cli`):

| Flag | Type | Meaning | Grounded in |
|---|---|---|---|
| `--scope <project\|system\|all>` | enum | DOC-1 D7 wire scope; default per DOC-3 §3 (see §3). Honoured only by scope-bearing commands. | DOC-1 D7, DOC-3 |
| `--json` | bool | Machine output: emit the DOC-1 envelope (passthrough) or the DOC-4 rollup struct (stack verbs) to **stdout**. | DOC-1 D3a, DOC-4 §8.2 |
| `--timeout <secs>` | u64 | Per-tool probe deadline override (DOC-4 §1.3 defaults: 2 s health, 10 s doctor). | DOC-4 §1.3 |
| `-v` / `--verbose` | count | Drill-down detail; on stack verbs, full per-check output (DOC-4 §3.2). On daemons, raise log level on **stderr**. | DOC-4 §3.2 |
| `--yes` / `-y` | bool | Non-interactive: skip the blast-radius confirmation (§3, §5). For automation/CI. | DOC-3 §5 |
| `--manifest <path>` | path | Override manifest path (else system override → embedded default, DOC-2 §2). | DOC-2 §2 |

`--json` and `--scope` are *global* (declared once on the root, available to every
subcommand) but **inert** where they have no meaning (e.g. `--scope` on `tctl
version`); this matches clap's `global = true` semantics and the repo pattern.

### 2. Dispatch engine — the heart of the doc

Every command above is **the same mechanical pipeline**. The controller compiles
in exactly two pieces of knowledge: *how to read the manifest* (DOC-2) and *how
to parse the contract* (DOC-1, via `trusty_common::contract`). Nothing about any
specific tool is hard-coded — no tool name, no binary path, no verb list, no
output shape. This is the spec's *"controller must contain zero tool-specific
logic"* (§83) expressed as code.

#### 2.1 The universal dispatch flow

```
tctl <command> [--scope S] [--json] [--timeout T] [--yes]
        │
        ▼
1. LOAD MANIFEST            DOC-2 §2 precedence: system override > embedded default
        │                   → Vec<Member { id, binary, kind, install, version,
        │                                  min_contract_version, depends_on, ui, … }>
        ▼
2. SELECT MEMBERS           which [[member]] entries this command targets:
        │                     • first-class stack verb → all enabled members
        │                     • `tctl <tool> <verb>`   → the one named member
        │                     • `tctl <verb> [m…]`     → the named members, else all
        │                   (honours `enabled`; never a hard-coded list)
        ▼
3. RESOLVE SCOPE            DOC-3 §3 default rule + per-verb polymorphism (§3 below)
        ▼
4. BLAST-RADIUS GATE        if any selected (member, verb, scope) is system-mutating
        │                   (DOC-3 §5) and stdin is a TTY and not --yes → confirm (§5)
        ▼
5. CAPABILITY NEGOTIATION   for each member: `<binary> version --json`  (DOC-1 D3b)
        │                     → read verbs[] + contract_version
        │                     → below floor F? mark contract_incompatible (DOC-4 §5.2)
        │                     → verb not in verbs[]? skip / render n/a (DOC-1 D3, DOC-4 §5.3)
        │                   orchestrator member (kind="orchestrator")? route via the
        │                     trusty_common::contract::orchestrator shim (DOC-6 §4)
        ▼
6. INVOKE (parallel)        for each member that advertises the verb:
        │                     spawn `<binary> <verb> --scope <S> --json`  (DOC-1 D3c)
        │                     bounded tokio join set; per-tool --timeout (DOC-4 §1.3)
        │                     timeout / spawn-fail / unparseable → synthesize down/unreachable
        ▼
7. COLLECT ENVELOPES        parse each stdout as Envelope<T> (trusty_common::contract)
        ▼
8. ROLLUP / RENDER          stack verbs → DOC-4 rollup (matrix + verdict + clusters)
        │                   passthrough  → render the single envelope verbatim
        │                   --json       → emit machine struct to stdout (§4)
        ▼
9. EXIT CODE                derive from verdict/envelope status (DOC-1 D5 / DOC-4 §7)
```

For a heavyweight first-class command (`install`, `upgrade`, `restart`) steps
6–8 are the *entry point* into the DOC-8/DOC-9 mechanics rather than a plain
envelope render — but the *shape* is identical (load manifest → per-member action
→ collect results → render). See §5.

#### 2.2 Proof of "zero tool-specific logic"

The dispatch engine is generic by construction; the proof is that **every input
to every step is either a manifest field or a contract field**:

| Step | Reads only | Never reads |
|---|---|---|
| Load manifest | the TOML registry (DOC-2) | a compiled-in tool list |
| Select members | `[[member]].id`, `.enabled` | hard-coded names |
| Resolve scope | the verb's scope-polymorphism (DOC-3 §3 table) + `--scope` | per-tool scope rules |
| Negotiate | `version.data.verbs[]`, `contract_version` (DOC-1) | a per-tool verb whitelist |
| Invoke | `[[member]].binary` + the verb string | a hard-coded binary path |
| Collect | `Envelope<T>` generic deserialization (DOC-1 D3a) | a per-tool output parser |
| Rollup | envelope `status`/`scope`/`deps[]` + manifest `depends_on`/`kind` (DOC-4) | a tool's identity |

A new member is added by **editing the manifest** (DOC-2 §3) — zero controller
code change. A new verb is usable via passthrough **without a `tctl` release**
(DOC-1 D3c) the moment a member advertises it in `verbs[]`. The orchestrator
(claude-mpm today, trusty-mpm later) is *just another member*: the controller
sees `kind = "orchestrator"`, routes its calls through the DOC-6 shim, and
otherwise treats it identically (DOC-6 §6 swap = single manifest edit). This is
the structural payoff the whole design set is organized around.

#### 2.3 Capability negotiation & graceful degrade (older-contract behaviour)

Per DOC-1 D2 and the DOC-2 §6 discovery rule, `tctl` **never infers capabilities
from the manifest**; it always probes `version --json` at dispatch time:

- **Verb not advertised** → the controller does not invoke it. For passthrough it
  errors with exit `3` ("`trusty-review` does not advertise verb `restart`"); for
  a stack verb the member's cell is `n/a` (DOC-4 §5.3) — never a failure.
- **Older but ≥-floor `contract_version`** (`[F, N)`) → invoke anyway, render only
  the fields that level guarantees, mark the cell `degraded` (DOC-4 §5.2). Never
  hard-fail. This is the canonical degradation rule DOC-1 owns and DOC-4/5
  reference.
- **Below floor** (`cv < F`) → the member is contract-incompatible: rolled up as
  `down` with `reason: "contract_incompatible"` and an **upgrade** remediation
  (DOC-9), *not* a controller-boundary exit `3` (DOC-4 §5.2 / Resolved Q4).
- **Unknown/unadvertised verb requested via passthrough, or malformed args** →
  controller-boundary **exit `3`** (DOC-1 D5), produced before any member runs.

### 3. Scope handling

`tctl` applies DOC-1's `--scope` wire flag (D7) using DOC-3's behavioural model.

#### 3.1 Default-scope rule (DOC-3 §3)

- Invoked **inside a project directory** (nearest `.git` / marker found by the
  shared `detect_project` walk, hoisted to `trusty_common` per DOC-6 §7) →
  default `--scope all`.
- Invoked **outside any project** → default `--scope system`.
- An explicit `--scope` always wins. The detected project id is the **full-path
  slug** of the git root (ADR-0008), surfaced in the matrix header (DOC-4 §3.1).

#### 3.2 Verb scope-polymorphism (DOC-3 §3) — applied at member selection

The controller derives each verb's applicable scope mechanically from the DOC-3
table, then intersects it with the requested `--scope`:

| Verb (first-class) | Applicable layer(s) | `tctl` behaviour |
|---|---|---|
| `install`, `upgrade`, `start`, `stop`, `restart` | **system only** | Always system-scoped; `--scope project` is rejected with a usage error (exit `3`). |
| `stack health`, `stack doctor`, `config` | **both** | Honour `--scope`; default per §3.1. Render system + project columns (DOC-4 §3). |
| index / reindex / palace-create (via passthrough) | **project only** | Valid only once the system layer `exists`; a bare project verb **never escalates** to a system op (DOC-3 §5). |

When a stack verb runs at `--scope all`, the controller invokes each member's
verb with `--scope all`; the member emits per-check `scope` tags and the DOC-4
rollup separates the system track (global verdict) from the project track (local
annotation).

#### 3.3 Blast-radius warn-before-system-op (DOC-3 §5)

Every selected operation carries a blast-radius tag **derived from its scope**
(DOC-3 §5), so the gate is mechanical, not per-tool:

- **System-mutating ops** (`install`, `upgrade`, `restart`, `stop`) disrupt every
  project/session on the box. Before executing, on an interactive TTY, `tctl`
  prints the radius and prompts:

  ```
  $ tctl restart
  ⚠  This will restart 3 system daemons (trusty-search, trusty-memory,
     trusty-analyze) and the controller UI service.
     All active projects and sessions on this machine will be interrupted.
  Continue? [y/N]
  ```

- **`--yes` / `-y` bypasses the prompt** for automation/CI (and is implied when
  stdin is not a TTY only if `--yes` is set — a non-TTY without `--yes` for a
  system-mutating op **aborts with exit `3`** rather than silently proceeding, so
  a scripted `tctl restart` that forgot `--yes` fails loudly instead of nuking a
  shared box). See Open Question 3 on the exact non-TTY default.
- **Project ops never trigger a system op implicitly** (DOC-3 §5): if a project
  verb finds the system layer not ready, it **reports the unmet precondition and
  stops** — no silent daemon restart.

### 4. Output & exit codes

`tctl` follows the repo's Unix-philosophy rule (DOC-1 D1, CLAUDE.md): **stdout
carries data, stderr carries logs/progress/prompts.**

#### 4.1 Human (default)

- **Passthrough** (`tctl <tool> <verb>`): render the single DOC-1 envelope as
  readable text (status line + `data` summary + `messages[]`).
- **Stack verbs** (`stack health`/`stack doctor`): the DOC-4 tools×scope matrix +
  one verdict line + the top remediation; `-v` adds the per-check drill-down
  (DOC-4 §3.2).
- **Long ops** (`install`/`upgrade`/`restart`): a per-member progress stream on
  **stderr** (so piping stdout stays clean); a final summary on stdout.

#### 4.2 `--json` (machine)

- **Passthrough** → the raw `Envelope<T>` (DOC-1 D3a) on stdout, byte-identical
  to what the member emitted (the controller is a transparent pipe for a single
  verb — it does not re-wrap).
- **Stack verbs** → the full DOC-4 rollup struct (DOC-4 §8.2: `verdict`,
  `exit_code`, `summary`, per-member `cells`, `clusters[]`, `remediations[]`),
  which DOC-7's UI renders identically.
- **`tctl version --json`** → capability discovery for the controller itself:
  `{ tool: "trusty-controller", tool_version, contract_floor: F,
  contract_target: N, stack_version }`. (The controller does not advertise a
  member-style `verbs[]`; its commands are documented in `--help`.)

#### 4.3 Exit codes (DOC-1 D5 / DOC-4 §7)

| Code | Meaning | Source |
|---|---|---|
| `0` | ok / running / ready (incl. project-`pending`, DOC-4 §7) | verdict / envelope status |
| `1` | fail / down — the stack (or member) is broken | verdict / envelope status |
| `2` | degraded / warn — usable but impaired | verdict / envelope status |
| `3` | controller/usage error — bad flag, unknown member, unadvertised verb, unreadable manifest, system-mutating op refused (non-TTY, no `--yes`) | **controller boundary**, not an envelope status |

Stack-command exit codes are **system-track-driven** (DOC-4 §7): a project
`pending` or even a project-scope `fail` never by itself fails a CI gate, so
`tctl stack doctor` is a sound "is the *machine's* stack healthy?" gate. The
`--json` `exit_code` field mirrors the process exit code for scriptability.

### 5. Interactivity & long-running ops

`install`, `upgrade`, and `restart` take time and touch the system. DOC-5 owns
their **command entry points**; the *mechanics* are owned downstream (DOC-8
install, DOC-9 upgrade). The shared UX contract:

1. **Confirmation** — system-mutating ops gate on the blast-radius prompt (§3.3)
   unless `--yes`.
2. **Progress** — long ops stream per-member progress to **stderr** (e.g.
   "installing trusty-search … cargo install trusty-search --locked …",
   "upgrading 2/4 …", "draining in-flight requests …"). DOC-3 §2/§6's
   *progressive readiness* applies: after install/upgrade, a freshly-installed
   daemon may report `system: ready, project: pending` — `tctl` surfaces
   "usable now, indexing in progress" rather than blocking (UUC1).
3. **Idempotency** — `install` on an already-installed member is a reported no-op
   (DOC-3 §4); `upgrade` on an up-to-date member is a no-op.
4. **Take-effect** — `upgrade` restarts the affected daemons so the new version
   takes effect (UUC3), via the DOC-9 flow (`trusty_common::update::
   upgrade_and_restart`, which already respects launchd supervision —
   `is_launchd_supervised`).

**`tctl updates` (the listing)** is the read-only sibling: it reuses
`trusty_common::update::check_crates_io` per member to compute installed-vs-target
diffs and renders changelog headlines parsed best-effort from each member's
`changelog` descriptor (DOC-2 §5). It is the spec's *"show available updates …
plus changelog headlines"* (§90). It never mutates and needs no confirmation.

**`tctl ensure`** is the idempotent entry point for the ensure-project pass.
It runs only if the project is not already set up, and is a no-op when ready.
DOC-8 owns the launch-hook that automatically calls this pass on claude-mpm
startup (so the user does not need to run it manually for typical workflows);
`tctl ensure` is the explicit manual escape hatch and is also invoked explicitly
during system setup. DOC-5 defines the command surface and scope behaviour;
DOC-8 owns the mechanics and the auto-launch wiring.

Relationship to the owning docs (kept crisp to avoid overlap):

- **DOC-5 defines** the command *names*, flags, scope/confirmation UX, and that
  each entry point dispatches via the §2 engine. **DOC-5 performs no hidden
  mutation** (e.g. on read commands like `stack doctor`). Explicit `tctl ensure`
  is the named entry point for the setup pass.
- **DOC-8 owns** the install/bootstrap *mechanics* (zero-knowledge install,
  ensure-project auto-config, ordering, and the launch-hook that auto-calls
  `tctl ensure` on startup).
- **DOC-9 owns** the upgrade *mechanics* (update detection, changelog extraction,
  upgrade + take-effect restart).
- **DOC-4 owns** the rollup the `stack` verbs render.

### 6. clap structure (implementation target)

Derive-based, matching the repo convention (verified against
`crates/trusty-search/src/main.rs`: `#[command(propagate_version,
subcommand_required, arg_required_else_help)]` on the root, global flags via
`#[arg(long, global = true)]`). Lives in **`crates/trusty-controller/src/`**
(`main.rs` defines `Cli`/`Commands`; `dispatch/` houses the §2 engine;
`render/` the human/JSON renderers). Edition 2021 (no let-chains required), per
the workspace default for non-mpm/agents crates.

```rust
// crates/trusty-controller/src/main.rs  (design sketch — not committed source)

use clap::{Parser, Subcommand, Args, ValueEnum};
use trusty_common::contract::Scope;   // DOC-1 D7 wire enum, reused verbatim

/// trusty-controller (`tctl`) — thin control plane for the claude-mpm stack.
#[derive(Parser)]
#[command(
    name = "tctl",
    version,
    propagate_version = true,
    subcommand_required = true,
    arg_required_else_help = true,
)]
struct Cli {
    /// Scope to act on. Default: `all` inside a project dir, else `system` (DOC-3 §3).
    #[arg(long, value_enum, global = true)]
    scope: Option<ScopeArg>,

    /// Machine-readable JSON to stdout (envelope for passthrough; rollup for stack verbs).
    #[arg(long, global = true)]
    json: bool,

    /// Per-tool probe deadline in seconds (DOC-4 §1.3: 2s health / 10s doctor default).
    #[arg(long, global = true)]
    timeout: Option<u64>,

    /// Non-interactive: skip the blast-radius confirmation (automation/CI).
    #[arg(long, short = 'y', global = true)]
    yes: bool,

    /// Override the manifest path (else system override > embedded default, DOC-2 §2).
    #[arg(long, global = true)]
    manifest: Option<std::path::PathBuf>,

    /// Increase detail (stack drill-down; daemon log level on stderr).
    #[arg(short, long, global = true, action = clap::ArgAction::Count)]
    verbose: u8,

    #[command(subcommand)]
    command: Commands,
}

/// CLI mirror of `trusty_common::contract::Scope` (DOC-1 D7). Default applied in §3.1.
#[derive(Copy, Clone, ValueEnum)]
enum ScopeArg { Project, System, All }

#[derive(Subcommand)]
enum Commands {
    /// Install the stack (or named members) — system. → DOC-8
    Install { members: Vec<String> },

    /// Upgrade the stack (or named members) to the BOM pins, then restart. → DOC-9
    #[command(visible_alias = "update")]
    Upgrade {
        members: Vec<String>,
        /// List what would change without upgrading (equivalent to `tctl updates`).
        #[arg(long)]
        check: bool,
    },

    /// List available updates + changelog headlines (read-only). → DOC-9 / DOC-2 §5
    Updates,

    /// Idempotent ensure-project pass; no-op when already set up. → DOC-8
    Ensure {
        /// Scope for ensure. Default: project (inside project dir) or system (outside).
        /// Explicit --scope all runs both layers.
        scope: Option<String>,
    },

    /// Start member daemon(s) — system.
    Start { members: Vec<String> },
    /// Stop member daemon(s) — system.
    Stop { members: Vec<String> },
    /// Restart all daemons + the controller UI service — system. → §7
    Restart { members: Vec<String> },

    /// Stack-wide rollup verbs (DOC-4).
    #[command(subcommand)]
    Stack(StackCmd),

    /// Read-only effective merged config (redacted) for each member. → DOC-3 §7
    Config { members: Vec<String> },

    /// One-line stack summary (verdict + stack version).
    Status,

    /// Print the controller's own bound port/address (clean stdout). → DOC-7
    Port {
        /// Emit host:port instead of the bare port.
        #[arg(long)]
        addr: bool,
    },

    /// Controller-side conformance self-check of a member (DOC-6 §8).
    Doctor {
        #[arg(long)]
        self_check: bool,
        member: Option<String>,
    },

    /// Print/open the controller web-UI URL. → DOC-7
    Ui {
        /// Just print the URL; do not launch a browser.
        #[arg(long)]
        print: bool,
    },

    /// GENERIC PASSTHROUGH: `tctl <tool> <verb> [args]` — any advertised verb (DOC-1 D3c).
    ///
    /// Captured as a trailing var-arg so the controller never hard-codes a
    /// per-tool verb list; the first token is the member id, the rest is the
    /// verb + its args forwarded to `<binary> <verb> …`.
    #[command(external_subcommand)]
    Passthrough(Vec<String>),
}

#[derive(Subcommand)]
enum StackCmd {
    /// Fast liveness sweep → tools×scope matrix + verdict (DOC-4).
    Health,
    /// Deep diagnostic sweep → matrix + drill-down + remediation (DOC-4).
    Doctor { member: Option<String> },
}
```

Notes:

- **`#[command(external_subcommand)]`** is the clap idiom that makes the generic
  passthrough (§2) first-class without enumerating tools or verbs — exactly the
  "zero tool-specific logic" requirement. The dispatcher validates the first
  token against the manifest member ids and the second against the member's
  advertised `verbs[]` before invoking.
- `Scope` is **reused from `trusty_common::contract`** (DOC-1 D6) rather than
  redefined, keeping one wire vocabulary across the stack; `ScopeArg` is only the
  thin clap-facing mirror.
- Global flags mirror trusty-search's verified root `Cli` (`--json`, `-v`,
  plus the controller-specific `--scope`/`--timeout`/`--yes`/`--manifest`).

### 7. Daemonless vs UI services — what `restart` covers

The spec requires `restart` to bounce *"all demonized tools **and UI services**"*
(§93). The controller derives the target set from the manifest, never a hard list:

- **Member daemons** (`kind = "daemon"`: trusty-search, trusty-memory,
  trusty-analyze) → restarted via each member's **`restart` contract verb**
  (DOC-1 lifecycle), which on macOS composes launchd `bootout` + `bootstrap`
  (graceful SIGTERM drain per CLAUDE.md #534 / `trusty_common::launchd` +
  `shutdown`). Each member's UI is *embedded in its own daemon* (DOC-2 `ui` =
  `/ui` on the daemon's port), so restarting the daemon restarts its UI — there
  is no separate UI process to bounce.
- **CLI-only / serve-only members** (`kind = "cli"`: trusty-review) → no
  long-lived daemon to restart unless run in `serve` mode; if they advertise
  `restart` in `verbs[]` it is dispatched, otherwise the cell is `n/a` (graceful
  degrade, DOC-1 D3).
- **The orchestrator** (`kind = "orchestrator"`: claude-mpm) → **does not
  advertise `restart`** in v1 (DOC-6 §4 / Resolved Q3): it is user-session-owned,
  not daemon-supervised, so `tctl restart` **skips it** and notes "claude-mpm is
  session-managed — restart your claude-mpm session manually." When trusty-mpm
  (Rust, supervised) replaces it (DOC-6 §6) it advertises `restart` and is bounced
  like any other daemon — no `tctl` change.
- **The controller's own UI service** (DOC-7) → `tctl restart` also bounces
  *itself*: the controller daemon hosting the DOC-7 web UI is restarted last (so
  it does not kill itself mid-sweep). This is the only member where the controller
  acts on its own process; DOC-7 owns the controller-UI lifecycle, DOC-5 only
  names the entry point.

So `restart` = "every supervised daemon I can reach via the contract + my own UI
service," computed from `kind` + advertised `verbs[]` — zero tool-specific
branching.

---

## Dependencies

### Consumes (inputs)
- **DOC-1** (Accepted) — the contract `tctl` dispatches to: the uniform envelope,
  the seven verbs, `version --json` `verbs[]` capability discovery, the generic
  passthrough (`tctl <tool> <verb>`), exit codes `0/1/2/3` (D5), the older-contract
  graceful-degrade rule (D2), and the `trusty_common::contract` types/Dispatcher
  the controller reuses to parse envelopes.
- **DOC-2** (Accepted) — the manifest registry the dispatch loop iterates:
  `[[member]]` enumeration, `binary`, `install`, `version`, `min_contract_version`,
  `kind`, `depends_on`, `ui`, and the embedded-default → system-override precedence.
- **DOC-3** (Accepted) — the scope model: `--scope project|system|all`, the
  default-scope rule, verb scope-polymorphism, and blast-radius warn-before-system-op.
- **DOC-4** (Accepted) — the rollup the `stack health`/`stack doctor` commands
  render (verdict, tools×scope matrix, `--json` struct, `--timeout`, exit codes).

### Produces (consumed by)
- **DOC-7** — the web UI is the GUI front-end of these same dispatch commands and
  consumes the `--json` rollup/envelope shapes; `tctl ui`/`tctl port` expose its URL.
- **DOC-8** — the `install` command is the entry point into the install/bootstrap
  mechanics.
- **DOC-9** — the `upgrade`/`updates` commands are the entry points into the
  upgrade mechanics.
- **DOC-10** — the isolation harness drives stack health/install/upgrade through
  these CLI entry points and asserts on their `--json`/exit codes.

> These edges match the README dependency graph (DOC-5 consumes DOC-1 + DOC-2 +
> DOC-3 + DOC-4; produces into DOC-7, DOC-8, DOC-9, DOC-10).

## Grounding (exists vs. net-new)

Source-first audit (2026-06-08): clap conventions and the dispatch primitives the
controller reuses are confirmed against the tree.

- **Consistent clap template.** All tools use **clap 4 derive** with the same root
  shape — verified in `crates/trusty-search/src/main.rs`:
  `#[command(propagate_version = true, subcommand_required = true,
  arg_required_else_help = true)]`, global flags declared as
  `#[arg(long, global = true)]` (`--json`, `-v/--verbose`, `-i/--index`). DOC-5's
  `Cli`/`Commands` sketch matches this verbatim; the `toolchains-rust-cli-clap`
  skill is the supplementary reference.
- **Reusable dispatch primitives** (net-new wiring, not net-new code):
  - **Invoke a member binary** — resolved from the manifest `binary` (DOC-2); the
    controller `Command::new(binary).args([verb, "--json", "--scope", s])`. Net-new
    glue; the binaries already exist.
  - **`<tool> port [--json]` discovery** — verified present on trusty-search /
    trusty-memory; reused for UI link-out (DOC-7) and the controller's own
    `tctl port`.
  - **Upgrade machinery** — `trusty_common::update` already exposes
    `check_crates_io` (verified `crates/trusty-common/src/update/mod.rs:213`),
    `perform_upgrade`, `upgrade_and_restart`, and `is_launchd_supervised`
    (verified `…/update/upgrade.rs:142`). `tctl updates`/`tctl upgrade` reuse these
    directly (DOC-9 owns the flow).
  - **Restart machinery** — `trusty_common::launchd`
    (`LaunchdConfig::{install,bootstrap,bootout}`) + `shutdown` (graceful drain,
    #534) compose the lifecycle `restart` verb; no new restart primitive.
  - **Envelope parsing** — `trusty_common::contract` (DOC-1 D6, **net-new** per
    DOC-6 §3) supplies `Envelope<T>` + `Dispatcher`; the controller deserializes
    generically.
- **Net-new:**
  - The **controller crate itself** (`crates/trusty-controller/`, DOC-0) — the
    `Cli`/`Commands` surface, the §2 dispatch engine, the §3 scope/blast-radius
    gate, and the human/JSON renderers.
  - The **generic-passthrough dispatcher** (`external_subcommand` → manifest +
    `verbs[]` validation → spawn). No existing tool fans a verb across the stack.
  - `tctl restart` bouncing **all daemons + the controller UI** from one command
    (operators today use per-tool launchd `bootout`/`bootstrap`).
  - `tctl updates` as a **cross-tool** changelog-headline listing (each tool only
    knows its own update today, via `upgrade --check`).

## Cross-cutting notes

- **Contract-versioning behavior (older-contract dispatch).** `tctl` always probes
  `version --json` and negotiates per DOC-1 D2 (§2.3): a member on an older
  (≥-floor) contract is dispatched-to with only the guaranteed fields and rendered
  `degraded`; a below-floor member rolls up `down`/`contract_incompatible` with an
  upgrade remediation; an unadvertised verb is `n/a` (stack) or exit `3`
  (passthrough). The controller **never hard-fails** on an older-but-≥-floor
  member — it degrades, matching DOC-1/DOC-4.
- **Zero tool-specific logic (spec §83).** §2.2 is the explicit proof: every
  dispatch input is a manifest or contract field; no tool name, binary, verb, or
  output shape is compiled in. Orchestrator swap (claude-mpm → trusty-mpm) is a
  single manifest edit (DOC-2 §7 / DOC-6 §6).
- **Unix philosophy / composability.** stdout = data/JSON, stderr =
  logs/progress/prompts (DOC-1 D1, CLAUDE.md); `--json` makes every command a
  composable building block; exit codes are scriptable (DOC-1 D5).
- **Security / secrets.** Passthrough renders tool envelopes verbatim, which are
  already redacted at the source (DOC-1 D8); the controller introduces no output
  that could leak secrets and never reads a tool's raw config.

## Remaining work

**Design:**

- [x] Lock the command surface (install / updates / upgrade / ensure / restart / stack
      health / stack doctor / start / stop / config / status / port / ui /
      self-check / passthrough) with one-line semantics (§1)
- [x] Resolve `update` vs `upgrade` (noun listing vs verb action) (§1.3)
- [x] Decide per-tool ops surface (`tctl <tool> doctor`, not `--tool`) (§1.4)
- [x] Specify the manifest-driven dispatch engine + the zero-tool-specific-logic
      proof (§2)
- [x] Specify capability negotiation + older-contract graceful degrade (§2.3)
- [x] Specify scope handling (default rule, polymorphism, blast-radius gate, `--yes`) (§3)
- [x] Specify output + exit codes (human/JSON; stdout=data, stderr=logs) (§4)
- [x] Specify interactivity / long-running-op UX + the DOC-8/DOC-9 hand-off (§5)
- [x] Pin the clap derive structure as an implementation target (§6)
- [x] Specify what `restart` covers (daemons + UI services + controller self) (§7)
- [x] Owner approved: all 5 open questions resolved (§ Resolved Decisions)
- [ ] Team review → Accepted

**Implementation-time (out of design scope):**

- [ ] Build `crates/trusty-controller/src/{main.rs,dispatch/,render/}` against the
      §6 sketch once `trusty_common::contract` lands (DOC-6 §3)
- [ ] Wire the passthrough validator (manifest ids + `verbs[]`) and the parallel
      probe/timeout collector (DOC-4 §1.3)

## Resolved Decisions

1. **`update` vs `upgrade` naming (Owner-approved).** The split is **KEPT**:
   - `tctl updates` (read-only listing, plural noun) — shows available upgrades + changelog headlines.
   - `tctl upgrade` (mutating verb) — moves members to BOM pins and restarts.
   - `tctl update` is a visible alias of `upgrade` (muscle-memory compatibility).
   - `tctl upgrade --check` is a dry-run equivalent of `tctl updates`.

   This is unambiguous and matches the spec's exact phrasing (§90, §1.3).

2. **Per-tool introspection surface (Owner-approved).** The passthrough form is **KEPT**:
   - `tctl <tool> doctor` (e.g. `tctl trusty-search doctor`) — raw envelope, generic passthrough.
   - `tctl doctor --self-check <member>` — conformance audit (controller capability).
   - `tctl stack doctor [m]` — rolled-up stack-wide view (DOC-4 matrix + drill-down).

   Three cleanly separated doctor surfaces, no extra flag plumbing (§1.4).

3. **Non-TTY default for system-mutating ops without `--yes` (Owner-approved).** **Abort loud**:
   - System-mutating ops (`install`/`upgrade`/`restart`/`stop`) on **non-TTY stdin without `--yes`** → **exit `3`** (fail loud).
   - Prevents a scripted command from interrupting every session by omission (§3.3, §4.3).

4. **Does `tctl restart` bounce the controller's own UI service by default? (Owner-approved).** **YES, include the controller**:
   - `tctl restart` bounces all member daemons **AND** the controller's own DOC-7 UI service.
   - Controller UI restarted **last** to avoid self-kill mid-sweep.
   - Future `--exclude-self` escape hatch noted but not required in v1 (§7, §1.2).
   - Covers "all daemons + UI services" per spec (§93).

5. **"Ensure project" boundary — explicit entry point required (Owner-approved).** **Add `tctl ensure`**:
   - DOC-5 defines **no hidden mutation** on read commands (`stack doctor`, `config`, etc.).
   - Ensure-project pass is **explicit-only**: entry point is `tctl ensure [--scope project]`.
   - DOC-8 owns the launch-hook that calls this pass on claude-mpm launch (auto-run on startup).
   - `tctl ensure` is idempotent: no-op when already set up (§5, §1.2).
   - `tctl ensure` now appears in the command tree (§1.1) and one-line semantics (§1.2).
