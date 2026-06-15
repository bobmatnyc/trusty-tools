# DOC-12 — Bootstrap Orchestration (first-load control plane)

**Status:** Draft (owner directive 2026-06-15)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md
**Owner directive:** Bob, 2026-06-15 — "`tctl` is the first thing loaded; it
handles ALL dependent loads of the trusty stack."
**Consumes:** [DOC-1](./01-tool-contract.md), [DOC-2](./02-stack-manifest-and-versioning.md), [DOC-3](./03-scope-model.md), [DOC-4](./04-doctor-health-rollup.md), [DOC-5](./05-controller-cli.md), [DOC-8](./08-install-bootstrap.md)
**Cross-ref:** [DOC-6](./06-contract-conformance-and-mpm-adapter.md), [DOC-7](./07-controller-web-ui.md), [DOC-9](./09-upgrade-flow.md), [ADR-0008](../../../adr/0008-project-identity-convention.md), [ADR-0011](../../../adr/0011-tctl-owns-service-lifecycle.md), and issues #1104, #1220, #1221, #1222.

## Purpose

DOC-0..DOC-8 framed `tctl` as a **coordinator the user invokes on demand**
(`tctl install`, `tctl stack doctor`, …). This doc adds the owner's 2026-06-15
directive: `tctl` is the **first-loaded control plane** — the single entry point
through which the entire trusty stack comes up. It specifies the **boot
sequence** `tctl` runs (`tctl up`), the dependency-ordered staging of services,
the always-on core (memory + search), the boot-time analyze pass over the core
crates, the **opt-in** orchestrator stage (trusty-mpm), and — when the
orchestrator is opted into — ensuring **Claude Code is present and at the latest
version** for the native managed-session path.

This is a **requirements + sequence** spec. It does **not** implement the binary.
It composes the mechanics DOC-8 (install/ensure), DOC-4 (rollup), DOC-5
(dispatch), and DOC-3 (scope/ordering/idempotency) already own — bootstrap is a
**new front-door command** (`tctl up`) sequencing existing primitives, not new
install machinery.

It also **reconciles** the boot model with the `trusty-console`-as-single-HTTP
direction (#1104/#1222): `tctl` is the **headless lifecycle/control plane (CLI +
supervised process)**; `trusty-console` is the **single HTTP/UI front door**.
See §7.

---

## DESIGN

### 1. `tctl` is the first-loaded control plane

**Requirement (owner directive).** The user's single entry point to the trusty
stack is `tctl`. Everything else — the core daemons, the boot-time analysis, and
(optionally) the orchestrator and its Claude Code runtime — loads **through**
`tctl`, in a defined order, with readiness gating between stages. The user does
not start memory, search, analyze, the console, or trusty-mpm by hand; `tctl up`
brings up exactly what each stage requires.

**Boot command: `tctl up`.** A new first-class command (added to the DOC-5
command tree — §6) is the canonical boot entry point:

```
tctl up [--with-mpm] [--no-mpm] [--analyze-core <blocking|background|skip>]
        [--wait] [--yes] [--scope <project|system|all>]
```

`tctl up` is the **boot orchestrator**. It is distinct from the existing verbs:

| Existing verb | What it does | Why `up` is different |
|---|---|---|
| `tctl install` (DOC-8) | one-time, zero-knowledge **install** of members | `up` assumes installed; it **starts/ensures-running** and gates readiness. `up` may *call* `install` for a missing always-on member (§3.4). |
| `tctl start` (DOC-5) | brings a daemon up (single lifecycle verb, no gating) | `up` is the **staged, dependency-ordered, readiness-gated** sequence across the whole always-on core + analyze + optional orchestrator. |
| `tctl ensure` (DOC-8) | idempotent **per-project** auto-config pass | `up` is **system-first boot**; it *invokes* `ensure` as a later stage (§3.5) once the system core is healthy. |
| `tctl stack health` (DOC-4) | read-only liveness sweep | `up` **acts** (starts/ensures), then renders the same DOC-4 matrix as its final report. |

**Relationship to existing surfaces (no overlap):** `tctl up` is a *composition*
of `start` + `install` (only if missing) + `analyze` (passthrough) + `ensure` +
the DOC-4 rollup, sequenced with gates. It introduces **no new mutation
primitive** — every step is an existing DOC-5/DOC-8 dispatch. This keeps the
"thin coordinator / zero per-tool verb-dispatch logic" property (DOC-5 §2.2): the
boot stages are read from the manifest (`kind`, `boot_stage`, `boot_policy` —
§2), never from a compiled-in tool list.

### 2. Manifest additions (boot metadata)

Boot ordering and policy must be **manifest-driven** (DOC-5 §2.2 — no hard-coded
tool list). DOC-12 adds three optional `[[member]]` fields to DOC-2 (additive;
defaults preserve current behavior):

| Field | Type | Default | Meaning |
|---|---|---|---|
| `boot_stage` | enum: `core` \| `analysis` \| `console` \| `orchestrator` | derived from `kind` | Which boot stage this member belongs to (§3). `kind="daemon"` two-layer members default to `core`; `kind="controller"` → `console`/control; `kind="orchestrator"` → `orchestrator`; trusty-analyze → `analysis`. |
| `boot_policy` | enum: `always_on` \| `boot_analysis` \| `opt_in` \| `on_demand` | derived from `kind` | Whether `tctl up` starts this member unconditionally (`always_on`), runs it as the boot analysis pass (`boot_analysis`), starts it only when opted in (`opt_in`), or never auto-starts it (`on_demand`). |
| `runtime` | sub-table | omitted | For an `orchestrator` member: the agent runtime it launches and how to verify/upgrade it. See §6 (Claude Code). e.g. `runtime = { kind = "claude-code", binary = "claude", min_policy = "latest", install_hint = "npm i -g @anthropic-ai/claude-code" }`. |

**Default member→stage/policy mapping (current stack):**

| Member | `kind` | `boot_stage` | `boot_policy` |
|---|---|---|---|
| `trusty-memory` | daemon | `core` | **`always_on`** |
| `trusty-search` | daemon | `core` | **`always_on`** |
| `trusty-analyze` | daemon | `analysis` | **`boot_analysis`** |
| `trusty-console` | controller/daemon | `console` | `always_on` (the single HTTP front door — §7) |
| `trusty-controller` (`tctl`) | controller | control | n/a (it *is* the orchestrator of boot) |
| `trusty-mpm` | orchestrator | `orchestrator` | **`opt_in`** |
| `claude-mpm` | orchestrator | `orchestrator` | `opt_in` (legacy orchestrator slot, DOC-0 A4) |
| `trusty-review` | cli | `on_demand` | `on_demand` |

The mapping is **data, not code**: swapping the orchestrator (claude-mpm →
trusty-mpm, DOC-6 §6) or marking a fourth daemon `always_on` is a manifest edit.

### 3. The boot sequence (`tctl up`)

The owner's ordering is a **dependency graph**:

```
        memory + search  (always-on core)
                │  gate: both system-healthy + version-ok
                ▼
        analyze over core crates  (boot analysis — blocking|background)
                │  gate: analyze daemon healthy (analysis result is non-gating)
                ▼
        console  (single HTTP front door — §7; always-on)
                │  gate: console healthy
                ▼
        [OPT-IN] trusty-mpm  ──► ensures Claude Code @ latest (native runtime)
                                  (only if user opts in — §4/§6)
```

ASCII sequence:

```
tctl up [--with-mpm]
  │
  ├─ STAGE 0  PREFLIGHT
  │     • load manifest (DOC-2 precedence: system override > embedded default)
  │     • detect platform (macOS/Linux → supervisor backend, DOC-8 §6)
  │     • acquire the system-scope advisory lock (DOC-3 §4 ensure.lock) so two
  │       concurrent `tctl up` invocations don't double-start the core
  │     • resolve project identity if in a project dir (ADR-0008) for the later
  │       ensure stage
  │
  ├─ STAGE 1  ALWAYS-ON CORE  (memory + search; boot_policy = always_on)
  │     for each always_on member, IDEMPOTENTLY ensure-running (§3.4):
  │        CHECK  `<binary> health --json`  (DOC-1) → already running+healthy?
  │        ├─ healthy + version-ok          → no-op ("trusty-search 0.24.1 — running")
  │        ├─ installed, down               → `<binary> start` (or service bootstrap)
  │        └─ not installed                 → `tctl install <member>` then start (§3.4)
  │        VERIFY `<binary> health --json` → running   (bounded retry, DOC-4 §1.3)
  │     GATE: BOTH memory AND search must reach system: healthy + version-ok.
  │           If either fails → STOP per failure policy (§5); do NOT proceed to
  │           analyze/console/mpm (you cannot analyze or serve a UI over a dead core).
  │
  ├─ STAGE 2  BOOT ANALYSIS  (trusty-analyze over the CORE CRATES; boot_analysis)
  │     • ensure the trusty-analyze daemon is running (same CHECK→act→VERIFY)
  │     • run analyze over the "core crates" set (§3.3) to produce a baseline
  │       health/quality snapshot, cached for the console + `tctl status`.
  │     • DEFAULT: BACKGROUND (non-blocking). The analysis RESULT never gates
  │       boot (a smelly core is not a down core). `--analyze-core blocking`
  │       waits for the snapshot; `--analyze-core skip` omits it.
  │     GATE: only that the analyze DAEMON is healthy (so the console's analyze
  │           tab works); the analysis CONTENT is non-gating.
  │
  ├─ STAGE 3  CONSOLE  (trusty-console; single HTTP front door — §7)
  │     • ensure-running (CHECK→act→VERIFY) like any always_on member.
  │     • console connects to memory/search/analyze over stdio-MCP (#1104) and
  │       renders their metrics natively — it is the HTTP/UI surface for the
  │       now-healthy core.
  │     GATE: console healthy → print its URL (the user's single web entry).
  │
  ├─ STAGE 4  [OPT-IN] ORCHESTRATOR  (trusty-mpm; boot_policy = opt_in)
  │     • Only if opted in (§4): --with-mpm flag, or config default_with_mpm=true,
  │       or interactive prompt "Start the session manager (trusty-mpm)? [y/N]".
  │     • ensure-running the trusty-mpm session-manager daemon (#1221/#1222).
  │     • ENSURE CLAUDE CODE @ LATEST (§6): verify `claude` present, check/upgrade
  │       to latest, confirm the native managed-session path
  │       (`env -u ANTHROPIC_API_KEY claude`) is usable.
  │     GATE: mpm healthy + Claude Code present-and-current → ready for sessions.
  │
  └─ STAGE 5  ENSURE-PROJECT  (if invoked inside a project dir, --scope all)
        • chain into the DOC-8 ensure-project pass for the resolved project id
          (index/palace/.mcp.json) once the system core is healthy (DOC-3 §6
          ordering: system → then project). Backgrounded reindex (DOC-8 §3.3).
  ────────────────────────────────────────────────────────────────────────────
  FINAL  render the DOC-4 tools×scope matrix + one-line verdict; exit code per
         DOC-4 §7 (system-track-driven; opt-out stages absent = not a failure).
```

#### 3.1 Why this order (dependency rationale)

- **Memory + search first** because they are the always-on substrate every other
  capability reads: the console renders *their* metrics (#1104), analyze's deep
  pass can call search (`analyzer_health.search_reachable`, #1104), and the
  orchestrator's sessions expect memory/search MCP wired in `.mcp.json`. You
  cannot meaningfully bring up analyze, console, or mpm over a dead core.
- **Analyze second** because it consumes the core (it can reach search) and its
  product is a *baseline snapshot* the console surfaces — but its *result* is
  advisory, so it is backgrounded and non-gating.
- **Console third** because it is the HTTP/UI projection of the now-healthy core
  + analysis; it depends on them, not vice versa.
- **Orchestrator last and opt-in** because a session manager is a heavier,
  user-facing capability that *consumes* the whole rest of the stack, and not
  every boot wants it (UUC: a user may just want memory+search for a Claude Code
  session without the managed-session fleet).

#### 3.2 Per-tool independence within a stage

Within the always-on stage, memory and search are **independent** (DOC-3 §6: a
slow search index does not gate memory). Both are ensured in parallel (DOC-4
§1.3 bounded join set); the **stage gate** is the AND of both reaching
`healthy + version-ok`. A single always-on member failing fails the stage (§5).

#### 3.3 "Core crates" — the boot-analysis target set (definition)

The boot analysis (STAGE 2) runs over the **core crates**: the shared libraries
and the always-on daemons that constitute the stack's own substrate — the set a
maintainer most wants a baseline health read on at boot. Defined as a
**manifest-driven set**, not a hard-coded list, via a new optional top-level
manifest key:

```toml
# stack manifest
analyze_core_targets = [
  "trusty-common",     # the shared library every crate depends on
  "trusty-search",     # always-on core daemon
  "trusty-memory",     # always-on core daemon
  "trusty-analyze",    # the analyzer itself
  "trusty-controller", # the control plane
]
```

Defaults (when the key is omitted) to **every member with `boot_stage = "core"`
plus `trusty-common`** (the shared lib, per the crate map). The set is
overridable in the system-level manifest so a maintainer can widen it (e.g. add
`trusty-mpm`) or a CI image can narrow it. Rationale for "core crates" = the
shared substrate (`trusty-common`) + the always-on daemons: those are what a
boot-time "is my stack's own code healthy?" baseline most needs, and they map to
the crate-map's "shared libraries + daemon/MCP servers" grouping.

The analysis itself is a **passthrough** to the trusty-analyze contract (`tctl
trusty-analyze analyze --targets <core-set> --json`, or the analyze daemon's
existing `analyze_quality`/`run_diagnostics` MCP tools), so DOC-12 adds no
analysis logic — it only *schedules* the pass and caches the result.

#### 3.4 Idempotency / restart-safety (the load-bearing property)

`tctl up` must be safe to run **repeatedly** — on every machine boot, after an
upgrade, or manually — and must **no-op** when the stack is already up. This
reuses DOC-3 §4's check→act→verify and DOC-8's idempotent primitives:

- Each always-on member's STAGE-1 step is **CHECK first** (`health --json`): a
  healthy, version-ok member is a **reported no-op**, never restarted. `tctl up`
  is not `tctl restart` — it never bounces a healthy daemon (so it does not
  disrupt in-flight sessions on a re-run).
- **Missing member** → `tctl up` may **install** it (DOC-8) only when its
  `boot_policy = always_on`, because an always-on core member absent on boot is
  the zero-effort-install case the spec targets (UUC2). For `opt_in`/`on_demand`
  members `up` never installs implicitly — a missing trusty-mpm on `--with-mpm`
  **guides** (points at `tctl install trusty-mpm`) rather than silently
  installing a heavy component. (Open Q1.)
- **Concurrency:** STAGE 0 takes the **system-scope advisory lock** (DOC-3 §4
  `ensure.lock`) around the system-mutating stages, so two `tctl up` invocations
  (e.g. a launchd boot trigger racing a manual run) do not double-start. The
  loser waits, re-CHECKs, and no-ops (DOC-3 §4 loser behavior).
- **Restart-safety:** because every act is idempotent and CHECK-gated, a
  crashed-and-restarted `tctl` (or a launchd `KeepAlive` re-exec) re-runs `tctl
  up` and converges to the same healthy state without duplicating daemons or
  re-indexing unnecessarily (incremental reindex skips unchanged files, DOC-8
  §2.3).

#### 3.5 Blocking vs background per stage (locked defaults)

| Stage | Default | Rationale | Override |
|---|---|---|---|
| 1 — core | **blocking** (gate before proceeding) | nothing downstream is meaningful over a dead core | — (always gates) |
| 2 — analyze daemon | **blocking** (daemon up) | console's analyze tab needs it live | — |
| 2 — analyze *content* | **background** (non-gating) | a baseline snapshot is advisory, can take time | `--analyze-core blocking` to wait; `skip` to omit |
| 3 — console | **blocking** (gate + print URL) | it is the user's web entry point | — |
| 4 — orchestrator | **blocking when opted in** | a session manager the user asked for should be confirmed ready | absent when not opted in |
| 4 — Claude Code @ latest | **blocking when opted in** (§6) | a session against a stale/absent runtime fails confusingly | `--skip-claude-upgrade` (§6) |
| 5 — ensure-project | **background reindex** | DOC-8 §3.3 progressive readiness | `--wait` to block to `fresh` |

### 4. Opt-in orchestrator stage (trusty-mpm)

After the always-on core (and console) are up, `tctl up` **offers** to start the
orchestrator. It is **opt-in**, resolved in this precedence:

1. **Explicit flag** — `--with-mpm` (force on) / `--no-mpm` (force off) wins.
2. **Config** — `~/.trusty-tools/trusty-controller/config.yaml` (§8) key
   `boot.with_mpm = true|false`.
3. **Interactive prompt** (TTY, neither flag nor config set) —
   `Start the session manager (trusty-mpm) now? [y/N]` (default **No**).
4. **Non-TTY default** — **off** (a scripted/CI `tctl up` does not silently spin
   up a session-manager fleet; it must pass `--with-mpm`). Mirrors DOC-5 §3.3's
   "fail-safe when non-interactive."

When opted in, STAGE 4 ensures the trusty-mpm session-manager daemon is running.
Per #1221/#1222, trusty-mpm's daemon stays a long-lived process speaking
MCP/stdio (the 24/7 supervisor + durable tmux fleet need persistence), and
trusty-console is the single HTTP front door surfacing its REST/fleet view — so
`tctl up`'s STAGE 4 brings up the **trusty-mpm daemon as an internal supervised
process** and relies on STAGE 3's console for any operator HTTP. `tctl` ensures
it via the orchestrator member's contract/lifecycle surface (DOC-6 shim for
claude-mpm; native for trusty-mpm when it ships), exactly like any other member
— `kind = "orchestrator"`, zero tool-specific branching.

**Why opt-in (not always-on):** the spec's lowest-effort scenario (UUC1) is a
Claude Code session with memory+search auto-wired — that needs the core, not the
managed-session fleet. The fleet is a distinct, heavier capability a user
explicitly wants. Keeping it opt-in honors "always-on = memory+search only."

### 5. Failure handling & readiness gating

Per-stage gates, with the owner's "memory or search won't start" case called out:

- **Always-on core failure (memory or search down).** This is a **hard stop** for
  the dependent stages: `tctl up` does **not** proceed to analyze-content,
  console-as-useful, or the orchestrator over a dead core (you cannot analyze,
  render metrics, or run sessions without it). Behavior:
  - Render the DOC-4 matrix with the failed member `down` + its remediation
    (DOC-8 §7: missing → install; down → start; below-floor contract → upgrade).
  - **Exit `1`** (DOC-4 §7 system-track), so a boot script fails loudly.
  - **The other core member still comes up** (independence, §3.2): if search is
    down but memory is healthy, memory stays up and is reported healthy — `tctl
    up` does not tear down a working member because its sibling failed. The
    *stage gate* fails (so downstream stages are skipped), but partial core
    availability is preserved and reported (maximize usefulness, DOC-8 §7).
  - **Console still starts** if at least one core member is healthy, because the
    console is the operator's window to *see and remediate* the failure (it
    renders the same `down` matrix). This is the one nuance to the hard-stop:
    the console is brought up *for observability* even on partial-core, but the
    **orchestrator stage is skipped** entirely on any core failure. (Open Q2.)
  - **Idempotent re-run is the recovery path** (DOC-8 §7): fix the cause, re-run
    `tctl up`; healthy members no-op, only the failed one is retried.
- **Analyze daemon down** → STAGE 2 reports analyze `down` (console's analyze tab
  shows it), but this is **not** a core failure: boot continues (analyze is not
  always-on substrate for sessions). Analysis content simply absent.
- **Console down** → reported `down`; the CLI still functioned (the user can
  drive `tctl` headlessly), so this is **degraded, exit `2`**, not a hard `1` —
  the stack is usable via CLI without the web UI.
- **Orchestrator / Claude Code failure (opted in)** → §6: if Claude Code is
  absent or can't be upgraded, STAGE 4 **guides and (for a hard runtime miss)
  marks the orchestrator stage failed** but does **not** fail the whole boot
  (core + console are up and useful); exit reflects DOC-4 rollup (degraded).

### 6. Native latest Claude Code (orchestrator runtime)

When the orchestrator stage is opted in (§4) **and** the selected orchestrator
launches Claude Code natively (the current managed-session path:
`env -u ANTHROPIC_API_KEY claude`, verified in
`crates/trusty-mpm/src/runtime/claude_code.rs`), `tctl up` ensures **Claude Code
is present and at the latest version** before declaring the orchestrator ready.

**Grounding.** trusty-mpm's `ClaudeCodeAdapter::spawn` already does a
**presence check** (`which claude`) and returns `BinaryNotFound("claude binary
not found on PATH — install Claude Code first")` if absent. DOC-12 lifts that
presence check to **boot time** and adds a **version/latest check + upgrade**, so
the failure surfaces *before* a session is attempted, not mid-session.

**Mechanism (`runtime` manifest sub-table, §2):**

```
STAGE 4b — ensure Claude Code @ latest  (orchestrator opted in, runtime.kind = claude-code)
  1. PRESENCE   `which claude` (reuse the adapter's check).
                absent → per runtime.min_policy:
                  • GUIDE + fail the orchestrator stage (default): print the
                    install hint (`runtime.install_hint`, e.g.
                    `npm i -g @anthropic-ai/claude-code`) and a docs link;
                    do NOT auto-run a remote installer (same trust stance as
                    DOC-8 §5 guide-and-abort for rustup/uv).
  2. VERSION    `claude --version` → installed version.
  3. LATEST     determine the latest available version:
                  • prefer Claude Code's own self-update path if present
                    (e.g. `claude update` / `claude upgrade` if the CLI exposes
                    one — probe, do not assume);
                  • else compare against the npm-published latest
                    (`npm view @anthropic-ai/claude-code version`) when npm is
                    the install channel (runtime.install_hint encodes the
                    channel).
  4. UPGRADE    installed < latest → upgrade per policy:
                  • runtime.min_policy = "latest" (default for the managed path):
                    run the runtime's own update command if it has one, else the
                    channel upgrade (`npm i -g @anthropic-ai/claude-code@latest`),
                    streaming progress to stderr.
                  • --skip-claude-upgrade or min_policy = "present": skip the
                    upgrade, accept the installed version, warn if stale.
  5. CONFIRM    re-run `claude --version`; confirm the native managed-session
                command (`env -u ANTHROPIC_API_KEY claude`) resolves (the same
                `ClaudeCodeAdapter` presence check passes). Report the version in
                the DOC-4 matrix (orchestrator row, runtime sub-cell).
```

**Decisions / open points:**

- **Version-check + upgrade is real but channel-dependent.** Claude Code's
  install/update channel is not Rust/cargo; DOC-12 keys it off
  `runtime.install_hint`/`runtime.channel` in the manifest (npm today). The
  controller probes for a native `claude update` command first (so it uses the
  tool's own upgrade path when available) and only falls back to the package
  manager. (Open Q3 — confirm the exact latest-version source and self-update
  command with Bob / the Claude Code CLI surface.)
- **Absent Claude Code = guide, don't auto-install** (default), consistent with
  DOC-8 §5's stance on not running remote installers as a side effect. A future
  `tctl up --bootstrap-claude` opt-in (explicit consent) could run the installer,
  deferred beyond v1.
- **"Native" means the OAuth managed-session path**, not the direct-API `tcode`
  runtime (`runtime/tcode.rs`, which keeps `ANTHROPIC_API_KEY`). DOC-12's
  Claude-Code-@-latest stage applies to the `claude-code` runtime only; a
  `tcode` orchestrator runtime skips it.
- **Forward-compat (DOC-0 A4 / DOC-3 §10):** the orchestrator is pluggable. The
  `runtime` sub-table is per-orchestrator, so trusty-mpm-with-claude-code,
  claude-mpm-with-claude-code, or a future orchestrator with a different runtime
  each declare their own runtime ensure-policy — no controller code change.

### 7. Reconciliation: `tctl` vs `trusty-console` (the boundary)

DOC-7 specified a controller-embedded web UI; #1104/#1222 (later, owner-directed)
mandate **HTTP lives ONLY in trusty-console**. DOC-12 reconciles these so the
design set is consistent rather than carrying two HTTP UIs:

| Concern | **`tctl` (trusty-controller)** | **`trusty-console`** |
|---|---|---|
| Role | **Headless lifecycle / control plane**: boot orchestration (`tctl up`), install/upgrade/restart, doctor/health rollup, ensure-project, the manifest/contract dispatch engine. | **Single HTTP/UI front door** (#1104): a Svelte SPA + the only HTTP server; a stdio-MCP client that renders each service's metrics natively. |
| Surface | **CLI** (DOC-5) + a **supervised control process** (boot trigger, lock holder). No browser UI of its own. | **HTTP + web UI**, including the controller/stack view (versions, health, upgrade indicators, doctor results — the DOC-7 content **moves here**). |
| Who serves the stack dashboard | Produces the DOC-4 rollup as **`--json`** (the data plane). | **Consumes** that `--json` (over stdio-MCP — `tctl` gains a `console_metrics`/stack-rollup MCP tool per #1104's shared contract) and renders it natively. |
| Boot relationship | `tctl up` **starts** trusty-console as the `boot_stage = "console"` always-on member (STAGE 3). | Is started **by** `tctl up`; depends on the core `tctl` already ensured. |

**Net effect — DOC-7 is superseded in surface, retained in content.** The
"out-of-the-box stack control plane UI" the spec asked for (§44–§56) is
**delivered by trusty-console**, not a second HTTP server inside `tctl`. `tctl`
becomes **headless** (CLI + control process); it exposes its stack rollup as
**JSON / an MCP `console_metrics`-style tool** that the console renders. This:

- honors #1104's "HTTP exactly once, in trusty-console" principle (no per-service
  HTTP, including none in the controller);
- removes the DOC-7 embedded-UI / `SKIP_UI_BUILD` / loopback-auth surface from
  `tctl` (the controller crate no longer ships a Svelte bundle — simplifying its
  publish, DOC-0 A5);
- keeps DOC-7's *control-plane semantics* (link-out to each tool's view, upgrade
  indicators, doctor results) — they relocate into the console's controller tab,
  fed by `tctl --json`.

This is an architectural decision (DOC-7 surface → trusty-console) recorded as
**[ADR-0011](../../../adr/0011-tctl-owns-service-lifecycle.md)**: *tctl owns
service lifecycle (headless control plane); trusty-console owns the single HTTP
surface.* (Open Q4 — confirm DOC-7 is formally re-statused **Superseded-by-#1104
/ ADR-0011**, and whether any controller-only view must remain CLI-only vs
console-rendered.)

**`tctl`'s own process under #1104.** The controller still needs a small
**supervised process** for boot triggering and lock-holding (the `kind =
"controller"` daemon, DOC-2). Under #1104 that process is **HTTP-less** — it is a
launchd/systemd-supervised boot+lifecycle agent that exposes its state via
stdio-MCP (`console_metrics`) to the console, not via its own HTTP port. The
DOC-5 `tctl port`/`tctl ui` commands change meaning: `tctl ui` now prints/open
**the trusty-console URL** (discovered via the console's `port --json`), not a
controller-served page. (Open Q5.)

### 8. Configuration (`~/.trusty-tools/trusty-controller/config.yaml`, #1220)

`tctl` reads its config from the cross-crate convention #1220 establishes:
**`~/.trusty-tools/<crate>/config.yaml`** → for the controller,
**`~/.trusty-tools/trusty-controller/config.yaml`**. Boot-relevant keys:

```yaml
# ~/.trusty-tools/trusty-controller/config.yaml
boot:
  with_mpm: false            # §4 opt-in default for `tctl up` (flag overrides)
  analyze_core: background   # blocking | background | skip  (§3.5)
  claude_code:
    policy: latest           # latest | present  (§6 min_policy default)
    skip_upgrade: false      # §6 --skip-claude-upgrade default
analyze_core_targets: []     # override the §3.3 core-crate set (empty = default)
manifest_override: null      # optional path; else system override > embedded (DOC-2)
```

This composes with DOC-2's manifest precedence (the manifest is the *registry*;
this config is *boot policy*). Per #1220 the same key is also editable via
trusty-console's config UI (the console is the config front door for all trusty
crates), so a user toggles `with_mpm` / `claude_code.policy` from the console and
`tctl up` honors it. Config precedence follows DOC-3 §7 (project > system >
default) for any project-scoped override; boot policy is **system-scoped** (boot
is a system act).

**Workspace-root note (#1220).** trusty-mpm's default managed-session workspace
root (`~/trusty-mpm-projects/<owner>/<repo>`) and its
`~/.trusty-tools/trusty-mpm/config.yaml` are **trusty-mpm's** config, not the
controller's — `tctl up`'s STAGE 4 starts the trusty-mpm daemon, which reads its
*own* config. The controller only decides *whether* to start it (§4); it does not
own trusty-mpm's workspace-root knob. (Cross-ref #1220/#1222.)

### 9. MCP-service direction (#1221) interplay

#1221 moves the session manager (and, by #1104, every local service) toward a
**JSON-RPC/stdio MCP** surface. `tctl up`'s STAGE-1/2/4 readiness checks are
**already** contract probes (`<binary> health --json`, DOC-1) that compose
cleanly with an MCP frontend: as services gain `serve --stdio` MCP, `tctl` can
health-check them either via the contract CLI (today) or via the MCP tool
(future) — the DOC-5 dispatch engine treats both uniformly (it reads `verbs[]`,
not a transport). DOC-12 adds no new transport assumption; it gates on the
contract status enum (DOC-1 D4) however the member reports it. When trusty-mpm
ships its MCP service (#1221), STAGE 4's "ensure-running" becomes an MCP
health/metrics probe rather than a CLI parse — a manifest/transport detail, not a
boot-sequence change.

---

## Acceptance criteria

1. **`tctl up` exists** as a first-class command (DOC-5 tree) and is the single
   documented boot entry point; `tctl install`/`start`/`ensure` remain distinct
   (§1 table).
2. **Always-on core gating:** `tctl up` ensures memory **and** search to
   `system: healthy + version-ok` before any later stage; if either fails the
   stack hard-stops the dependent stages and exits `1`, while preserving and
   reporting any healthy sibling (§3.2/§5).
3. **Boot analysis:** trusty-analyze runs over the manifest-defined core-crate set
   (§3.3); default background + non-gating; `--analyze-core blocking|skip`
   honored; the analyze *daemon* health gates only the console's analyze tab.
4. **Opt-in orchestrator:** trusty-mpm starts only via `--with-mpm` / config /
   interactive-yes; non-TTY default off (§4).
5. **Claude Code @ latest:** when the orchestrator is opted in and uses the
   native `claude-code` runtime, `tctl up` verifies `claude` presence, checks +
   (per policy) upgrades to latest, and confirms `env -u ANTHROPIC_API_KEY claude`
   resolves before declaring the orchestrator ready; absent → guide, don't
   auto-install (§6).
6. **Idempotency / restart-safety:** a second `tctl up` on a healthy stack is an
   all-no-op (no daemon bounced); concurrent invocations serialize via the
   system advisory lock; a supervised re-exec converges (§3.4).
7. **tctl ⟂ console boundary:** `tctl` is headless (CLI + control process,
   no HTTP); trusty-console is the single HTTP/UI surface and renders the
   controller's `--json` stack rollup; DOC-7's embedded UI is superseded by the
   console (ADR-0011) (§7).
8. **Config:** boot policy read from
   `~/.trusty-tools/trusty-controller/config.yaml` (#1220) and editable via the
   console; flags override config (§8).
9. **Zero per-tool verb-dispatch logic preserved:** every boot stage is selected
   from manifest `boot_stage`/`boot_policy`/`kind`, never a hard-coded tool list
   (§2, DOC-5 §2.2).

## Phased rollout

- **Phase 0 — manifest + sequence skeleton.** Add `boot_stage`/`boot_policy`/
  `runtime` to DOC-2 and `analyze_core_targets`/`boot` config (§2/§8). Implement
  `tctl up` STAGE 0–1 (preflight + always-on core ensure with gating) reusing
  DOC-8 ensure + DOC-4 rollup. Acceptance: `tctl up` brings memory+search to
  healthy idempotently and gates correctly.
- **Phase 1 — analyze + console stages.** STAGE 2 (boot analysis over core
  crates, background) + STAGE 3 (start console, print URL). Acceptance: a clean
  `tctl up` yields healthy core + a console URL + a cached core-analysis snapshot.
- **Phase 2 — opt-in orchestrator + Claude Code @ latest.** STAGE 4/4b
  (trusty-mpm opt-in, Claude Code presence/version/upgrade). Acceptance:
  `tctl up --with-mpm` starts the session manager and confirms `claude` @ latest;
  absent `claude` guides.
- **Phase 3 — console reconciliation (ADR-0011).** Move DOC-7 content into the
  console; expose `tctl`'s stack rollup as a `console_metrics`/stdio-MCP tool
  (#1104); re-status DOC-7 Superseded. Make `tctl ui` print the console URL.
- **Phase 4 — supervised boot trigger.** Wire `tctl up` to run at machine
  login/boot (launchd/systemd) so the always-on core comes up automatically; the
  user's first interaction is an already-warm stack.

## Dependencies

### Consumes (inputs)
- **DOC-8** — install/ensure mechanics + progressive readiness + guide-and-abort
  the boot stages compose.
- **DOC-4** — the rollup `tctl up` renders + the timeout/exit-code model the
  gates use.
- **DOC-5** — the dispatch engine + command tree `tctl up` extends.
- **DOC-3** — ordering (system→project), idempotency (check→act→verify), the
  advisory lock, scope/blast-radius.
- **DOC-1/DOC-2** — contract probes (`health --json`) + manifest fields
  (`kind`, plus the new boot fields).
- **DOC-6** — the orchestrator shim (claude-mpm) / native (trusty-mpm) the
  STAGE-4 ensure routes through.

### Produces (consumed by)
- **trusty-console (#1104/#1222)** — consumes `tctl`'s stack-rollup `--json` /
  MCP tool to render the controller/stack view.
- **DOC-7** — superseded in surface by §7/ADR-0011 (content relocates to the
  console).
- **DOC-10** — the isolation harness drives `tctl up` (and `--with-mpm`,
  `--analyze-core blocking`, `--wait`) as the boot acceptance gate in a clean VM.

## Grounding (exists vs. net-new)

| Area | Reality today | Bootstrap implication |
|---|---|---|
| Managed Claude Code launch | `ClaudeCodeAdapter` (`crates/trusty-mpm/src/runtime/claude_code.rs`): `which claude` presence check + `env -u ANTHROPIC_API_KEY claude` spawn. | §6 lifts presence to boot + adds version/latest/upgrade; the spawn command is unchanged. |
| Per-member health/start | each daemon has `start` + `health`/`doctor` contract verbs (DOC-1; verified search/memory/analyze). | §3 STAGE ensure = CHECK `health` → act `start` → VERIFY `health`. Net-new: the *sequencing/gating*, not the verbs. |
| Idempotent ensure + lock | DOC-8 §2.3 idempotent primitives; DOC-3 §4 `ensure.lock` (fs4/flock). | §3.4 restart-safety/concurrency reuse these directly. |
| Console as single HTTP | #1104 (CLOSED epic): console = only HTTP, stdio-MCP client, native render. trusty-console crate exists (`crates/trusty-console/`). | §7 boundary + ADR-0011; `tctl` stays headless. |
| Config convention | #1220: `~/.trusty-tools/<crate>/config.yaml`. | §8 controller config path. |
| Session-manager MCP | #1221 (open): trusty-mpm `serve --stdio` MCP frontend. | §9: STAGE-4 probe composes with the future MCP surface. |
| The `tctl up` boot orchestrator | **Net-new.** No staged boot command, no boot metadata, no Claude-Code-@-latest gate exists today. | This document. |

## Open questions (for Bob)

1. **Q1 — install-on-boot for always-on members?** Should `tctl up` *install* a
   missing always-on core member (memory/search) automatically (treating boot as
   the UUC2 zero-effort path), or only *start* an already-installed one and guide
   otherwise? DOC-12 proposes: install always-on members, guide for
   opt-in/on-demand (§3.4). Confirm.
2. **Q2 — console on partial-core.** On a core failure (e.g. search down, memory
   up), should the console still start *for observability* (proposed yes, §5) so
   the user can see/remediate via the web UI, or hard-stop everything after the
   core gate?
3. **Q3 — Claude Code latest-version source + self-update.** What is the
   authoritative "latest" source and upgrade command for Claude Code (a native
   `claude update`? npm `@anthropic-ai/claude-code@latest`? something else)?
   §6 probes for a native update path first, else npm — confirm the channel and
   whether `tctl` should ever auto-run it vs always guide.
4. **Q4 — DOC-7 disposition.** Confirm DOC-7 (controller embedded web UI) is
   formally **Superseded** by trusty-console (#1104) per ADR-0011, and that the
   controller crate drops its Svelte/`SKIP_UI_BUILD` surface (§7).
5. **Q5 — `tctl` process shape under #1104.** Is the `kind=controller` process a
   genuinely HTTP-less launchd/systemd boot agent exposing state via stdio-MCP
   (proposed, §7), and does `tctl ui` redirect to the console URL?
6. **Q6 — always-on set.** Is the always-on core exactly **memory + search**
   (per the directive), with analyze as boot-analysis and console as always-on
   front door — or should analyze/console also be `always_on` proper? DOC-12
   models analyze as `boot_analysis` and console as `always_on` (§2 table);
   confirm.

## Remaining work

- [ ] Owner review of §3 sequence + §5 gating + §6 Claude-Code policy
- [ ] Resolve Open Questions Q1–Q6 with Bob
- [ ] Land ADR-0011 (tctl headless / console single-HTTP) — drafted alongside
- [ ] Re-status DOC-7 → Superseded once Q4 confirmed
- [ ] (impl-time) build `tctl up` staging in `crates/trusty-controller/src/`
      composing DOC-8/DOC-4/DOC-5 primitives + the §6 Claude Code ensure
- [ ] (DOC-10-owned) wire `tctl up [--with-mpm] [--analyze-core blocking]
      [--wait]` into the isolation harness as the boot acceptance gate
