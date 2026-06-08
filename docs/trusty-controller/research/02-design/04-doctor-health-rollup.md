# DOC-4 — Doctor/Health Rollup Model

**Status:** Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md
**Consumes:** [DOC-1](./01-tool-contract.md) (Accepted), [DOC-2](./02-stack-manifest-and-versioning.md) (Accepted), [DOC-3](./03-scope-model.md) (Accepted)
**Cross-ref:** [DOC-6](./06-contract-conformance-and-mpm-adapter.md) (Accepted), [DOC-0](./00-naming-and-doc-charter.md)

## Purpose

Define how the controller aggregates per-tool doctor/health JSON into a stack
verdict and renders a tools × scope matrix, including a comprehensive stack
doctor.

Concretely, DOC-4 owns the **rollup function**: given the per-member contract
envelopes (DOC-1) collected across the manifest's members (DOC-2) at a requested
scope (DOC-3), how does the controller (`tctl`) combine them into

1. a single **stack verdict** (one word a human or CI can act on),
2. a **tools × scope matrix** (the primary rendered artifact of `tctl stack
   doctor` / `tctl stack health`), and
3. a machine **`--json`** rollup structure (consumed by DOC-7's UI and DOC-10's
   harness).

DOC-4 contains **zero tool-specific logic** (spec §83): every input is a generic
contract envelope, and every rule below operates on the envelope fields
(`status`, `scope`, `data.checks[].scope`, `data.deps[]`) — never on a tool's
name or internals.

---

## DESIGN

### 1. Inputs & invocation

#### 1.1 What the rollup consumes

The rollup is a pure function over **contract envelopes** (DOC-1 D3a). For each
member it gathers one or both of:

- the **`doctor` envelope** — `doctor.data.checks[]` (each
  `{id,title,scope,status,detail,remediation}`) plus `doctor.data.summary`, with
  the envelope `status` already rolled to `ok|warn|fail` per DOC-1's per-tool
  rule (any `fail`→`fail`; else any `warn`→`warn`; else `ok`;
  `pending`/`skipped` never worsen). This is the **deep** signal.
- the **`health` envelope** — envelope `status` `running|degraded|down`, with
  supporting `health.data` telemetry (`uptime_secs`, `port`, `deps[]`,
  `detail`). This is the **fast** signal.

It never inspects a repo, an index, or a daemon's internals directly: per DOC-3
Resolved Q5, *freshness and readiness are tool-reported*, not
controller-computed. The controller's only job is to **collect and combine**.

#### 1.2 How the controller gathers the data

The collection loop is manifest-driven (DOC-2 §6 discovery rule), with **zero
hard-coded tool list**:

1. **Enumerate members** from the manifest (`[[member]]` entries, honoring
   `enabled`). DOC-2 is the registry of *which members exist* and *what binary to
   invoke*.
2. **Probe capability** — for each member run `<binary> version --json` to read
   its advertised `verbs[]` and live `contract_version` (DOC-1 D3b). This is also
   the **negotiation** step (§5.2): a member below the manifest floor
   (`min_contract_version`) is recorded as *contract-incompatible* and rolled up
   as **degraded**, never hard-fail (DOC-1 floor rule, §5.2 below).
3. **Invoke the requested introspection verb** — `doctor` for `stack doctor`,
   `health` for `stack health` — at the requested `--scope` (DOC-1 D7;
   `project|system|all`), **only if the member advertises it** in `verbs[]`. A
   member that does not advertise the verb yields a **"not applicable"** cell
   (§5.3), not a failure.
4. **Route the orchestrator through the shim** — a member whose manifest `kind`
   is `orchestrator` (claude-mpm today) is non-Rust and does not emit envelopes
   itself; the controller routes its `doctor`/`health`/`version` calls through
   the `trusty_common::contract::orchestrator` shim (DOC-6 §4), which synthesizes
   the same envelopes from `mpm-doctor`/liveness probes. From the rollup's point
   of view it is just another envelope source advertising
   `["doctor","health","version"]`.
5. **Collect envelopes** into the rollup input set, tagged by member id and the
   scope each check carries.

The controller dispatches the *advertised* verb via DOC-1 D3c generic
passthrough (it is sugar over `tctl <tool> <verb> --json --scope <scope>`), so a
member needs no controller release to participate.

#### 1.3 Parallelism & per-tool timeout (a hung tool must not hang the rollup)

The rollup MUST be **resilient to a single slow/hung member**:

- **Collect in parallel.** Each member's `version`/`doctor`/`health` probe runs
  concurrently (a bounded `tokio` join set). The stack rollup latency is
  therefore ≈ the slowest single member, not the sum. This matters because
  `stack health` is meant to be a *fast* liveness sweep (§4).
- **Per-tool timeout → synthesized `down`/`unreachable` envelope.** Each probe
  has its own deadline with **defaults: 2 s for `health`, 10 s for `doctor`** —
  doctor does deeper work. These defaults are overridable via a single controller
  flag **`--timeout=<secs>`** in v1; per-member manifest timeouts are deferred.
  On timeout (or a process spawn error, a non-zero exit with no parseable
  envelope, or unparseable output) the controller **synthesizes a terminal
  envelope** for that member rather than blocking or propagating the hang.
  DOC-1 already specifies this for `health` ("the controller synthesizes a `down`
  envelope" when the tool is not answering); DOC-4 generalizes it to all
  introspection verbs and distinguishes the *reason* (§5.1: missing vs down vs
  unreachable/timeout).
- **No partial blocking.** A member that never returns is recorded as
  `unreachable` (with its timeout as the detail) and the matrix renders the rest
  of the stack immediately. The stack verdict is computed over the envelopes
  collected (real + synthesized) — there is no "still waiting" stack state.

### 2. Aggregation rules

The rollup is a three-level fold. Each level uses a **precedence/combine
function** over a status lattice, and the **scope axis** modulates how a status
contributes to the global verdict.

#### 2.0 The unified stack-verdict vocabulary

The two source vocabularies — doctor `ok|warn|fail|pending|skipped` and health
`running|degraded|down` (DOC-1 D4) — must reconcile into **one** verdict
vocabulary the matrix cells, the stack verdict, and the exit code all speak.
DOC-4 proposes a **four-value stack-verdict vocabulary**:

| Stack verdict | Meaning | Sources that map here |
|---|---|---|
| **`ready`** | Everything the stack needs is present, healthy, and version-ok. | doctor `ok`; health `running` |
| **`degraded`** | Usable but impaired — a warning, a non-fatal dependency problem, or an older-but-acceptable contract. The stack works; something wants attention. | doctor `warn`; health `degraded`; older-but-≥-floor `contract_version`; a *required dep* unreachable that the owning tool already surfaced as `degraded` |
| **`pending`** | Setup in progress / not-yet-done **but not broken** — the DOC-3 "unindexed = system-ready, project-pending, NOT broken" state. Only ever arises from **project-scope** signals. | doctor `pending` (project scope) |
| **`down`** | Broken — a system check failed or a daemon is not answering. The stack (for the affected member) is unusable. | doctor `fail`; health `down`; below-floor / contract-incompatible member (renders with `reason: "contract_incompatible"` + upgrade remediation) |

`skipped` is **not** a verdict value — a skipped check contributes nothing (it is
absorbed: it neither improves nor worsens the rollup), exactly like DOC-1's
per-tool rule where `skipped` never worsens the envelope status. It is still
*rendered* in verbose drill-down (so the user sees "N/A on this platform"), but
it is invisible to the verdict fold.

> **Why a distinct `pending` rather than folding it into `degraded`?** Because the
> spec's load-bearing rule (DOC-3 §2) is that an unindexed project on a healthy
> daemon is **not** a problem — it is normal, expected, in-progress. Collapsing it
> into `degraded` would make a fresh-but-still-indexing project look impaired,
> defeating the UUC1 "usable now, fully ready in ~N s" behavior. `pending` is a
> first-class, *positive-trajectory* state. (See Open Question 2 on whether `ready`
> should instead be named `ok` for symmetry with the doctor vocabulary.)

**Mapping function (doctor check / health envelope → stack verdict):**

The stack verdict uses **`ready`** for the all-good state (matching DOC-3's
readiness-ladder language), while individual cells may still echo the source
`ok` for envelope compatibility.

```
doctor check status → verdict:        health envelope status → verdict:
  ok      → ready                       running  → ready
  warn    → degraded                    degraded → degraded
  fail    → down                        down     → down
  pending → pending
  skipped → (absorbed; contributes nothing)
```

#### 2.1 Precedence (the combine lattice)

When folding multiple verdicts into one, the controller applies this strict
precedence (worst-wins), **modulated by scope** (§2.2):

```
down  ≻  degraded  ≻  pending  ≻  ready        (skipped: absorbed — no contribution)
```

- **`down` dominates everything.** Any contributing `down` makes the fold `down`.
- **`degraded` beats `pending` and `ready`.** An impairment outranks an
  in-progress setup.
- **`pending` beats `ready`.** A combine of an in-progress project check and an
  ok check surfaces as `pending` (there is still work to do), **but only within
  the project scope** — see the critical scope rule in §2.2; `pending` never
  promotes itself into the *system/global* verdict.
- **`ready` is the floor.** Only an all-`ready` (or `ready`+`skipped`) set folds
  to `ready`.

This is the natural generalization of DOC-1's per-tool doctor rule (any
`fail`→`fail`; else any `warn`→`warn`; else `ok`) with the two extra ranks
(`down` for liveness, `pending` for in-progress) inserted.

#### 2.2 Scope-awareness — system is GLOBAL, project is LOCAL

This is the heart of DOC-4 and the direct expression of DOC-3 §9. The combine
function is **not** scope-blind. Each input check/envelope carries a `scope`
(per-check `scope` from `doctor.data.checks[]`, or the envelope `scope` for
`health`), and the controller separates the fold into two tracks:

- **System track (GLOBAL).** Folds all `scope:"system"` checks/envelopes across
  all members. A `down` or `degraded` here means the affected tool is unusable
  (or impaired) for **every project on the box**. **The system track drives the
  global stack verdict.** A system `down` is a stack-level problem the controller
  surfaces loudly.
- **Project track (LOCAL).** Folds all `scope:"project"` checks for the *current*
  project (keyed by the DOC-3 / ADR-0008 full-path slug). A project `pending`
  here is local and in-progress; it annotates per-repo readiness but **MUST NOT
  make the stack look broken.**

**The load-bearing rule (verbatim intent from DOC-3 §2/§9):**

> A **project `pending`** check (e.g. "no index registered for this project yet —
> system is ready") **never** raises the system/global verdict above `ready`. It
> surfaces only in the project column as `pending`. An *unindexed project on a
> healthy daemon* rolls up as **system: `ready`, project: `pending`** — never
> `down`, never `degraded`.

Conversely, **a system `down`/`degraded` is global**: it dominates the stack
verdict regardless of how many projects are fine. You cannot index against a dead
daemon (DOC-3 §6), so a dead system daemon makes every project's effective
readiness `down` for that tool — but the controller represents this **once**, on
the system row, and renders the project cells as **blocked-by-system** (a derived
annotation, not a duplicated failure — see §5.4 to avoid double-counting).

**Precise per-cell fold (one member, one scope column):** apply §2.1 precedence
over exactly the checks tagged with that scope. **Precise stack verdict:** apply
§2.1 precedence over **the system-track per-member verdicts only**, then *append*
the project track as advisory annotation. The global verdict is therefore a
function of system checks; project `pending` is reported but is **never** the
reason the stack verdict is worse than `ready`. A genuine project-scope
`fail`/`down` (distinct from `pending`) **degrades only the project cell**, not
the global verdict, because its blast radius is one repo. The exit code stays
system-track-driven — a broken index in one checkout is not a machine-level
stack failure. Surface such failures prominently in the project column and
in `-v`, but do not fail a CI gate checking system health.

#### 2.3 The three folds (summary)

| Level | Input | Combine |
|---|---|---|
| **per-check → per-tool-per-scope** | the `scope:"X"` checks of one member's `doctor` (or that member's `health` envelope for column X) | §2.1 precedence over those checks |
| **per-tool-per-scope → per-tool** | a member's `{system, project}` cells | system cell drives the member verdict; project cell annotates (a member is `down` if its **system** cell is `down`, even if its project cell is `ready`) |
| **per-tool → stack** | all members' **system** verdicts | §2.1 precedence over the system track; project track appended as per-repo annotation |

### 3. The tools × scope matrix (primary rendered artifact)

`tctl stack doctor` renders a matrix: **rows = manifest members**, **columns =
`system` / `project`**, **cells = the rolled-up verdict** for that member at that
scope. A trailing line gives the **stack verdict**.

#### 3.1 Default (summary) view

```
$ tctl stack doctor                       # cwd = a project (default scope: all)

  STACK DOCTOR — stack 2026.06-1                    project: my-project
  ────────────────────────────────────────────────────────────────────
  MEMBER              SYSTEM        PROJECT
  ────────────────────────────────────────────────────────────────────
  trusty-search       ✓ ready       … pending     (indexing — usable now)
  trusty-memory       ✓ ready       ✓ ready
  trusty-analyze      ! degraded    — n/a         (trusty-search dep — see below)
  trusty-review       ✓ ready       — n/a         (system-only)
  claude-mpm          ✓ ready       — n/a         (system-only)
  ────────────────────────────────────────────────────────────────────
  STACK VERDICT: degraded   (1 degraded, 1 project-pending)
  → trusty-analyze degraded: required dependency trusty-search is reachable
    but reported degraded; run `tctl stack doctor trusty-analyze -v` for detail.
```

Cell glyph legend (proposed): `✓ ready` · `! degraded` · `… pending` · `✗ down`
· `— n/a` (verb/scope not applicable). The verdict line states the stack verdict
plus a one-line count and the **top remediation** (§6).

#### 3.2 Verbose / drill-down view

`-v` / `--verbose` (or `tctl stack doctor <member> -v`) drills into individual
`doctor.data.checks[]`, printing each check's `id`, `title`, `scope`, `status`,
`detail`, and `remediation` — i.e. the full DOC-1 doctor payload, grouped by
member then scope, with `skipped` checks shown (rendered, not folded):

```
  trusty-search  (system: ready · project: pending)
    [system]  ✓ daemon_running       Daemon running at 127.0.0.1:7879 (v0.24.1)
    [system]  ✓ model_cache          all-MiniLM-L6-v2 present (84 MB)
    [system]  ! log_rotation         stderr.log has no rotation policy
              ↳ fix: trusty-search doctor --fix
    [project] … project_index        No index registered for this project yet
              ↳ fix: trusty-search index        (system is ready — usable now)
    [system]  ○ coreml_probe         skipped (not Apple Silicon)
```

The drill-down is a faithful re-print of each member's own checks (zero
re-interpretation), so a member's remediation hints reach the user verbatim
(§6).

### 4. Health vs doctor — two depths, one verdict vocabulary

DOC-1 defines two introspection verbs with two different vocabularies; DOC-4
defines two stack commands over them and reconciles both into the single
§2.0 verdict vocabulary.

| | `tctl stack health` | `tctl stack doctor` |
|---|---|---|
| Underlying verb | `health` (DOC-1) | `doctor` (DOC-1) |
| Depth | **fast** liveness sweep — "is the stack up?" | **comprehensive** — every check, scope-tagged, with remediation |
| Source vocab | `running|degraded|down` | `ok|warn|fail|pending|skipped` |
| Per-tool timeout | short (≈2 s) | longer (≈10 s) |
| Project layer | reports per-project *state* status where the verb is project-aware | full project-scope checks (`pending` etc.) |
| Use | the cheap pre-flight / "should I route traffic / start a session?" | the diagnostic / "what's wrong and how do I fix it?" |

**Reconciliation.** Both commands fold into the §2.0 vocabulary using the §2.0
mapping. `stack health` can only ever produce `ready`/`degraded`/`down` (health
has no `pending` — liveness is binary-ish), while `stack doctor` can additionally
produce `pending` (it sees project-scope `pending` checks). This is intentional:
*liveness has no notion of "setup in progress"; only the deep doctor does.* So a
freshly-installed daemon with no project index shows `stack health: ready` (the
daemon is up) and `stack doctor: project pending` (the project isn't indexed yet)
— both correct, at their respective depths.

**Consistency guarantee.** For a given member, `stack health` and `stack doctor`
must not contradict on the **system** track: if `health` says `down`, `doctor`'s
system column must also be `down` (a dead daemon fails its system checks).
DOC-10's harness asserts this (the self-check, DOC-6 §8). The two may legitimately
*differ in depth* (doctor surfaces `degraded` for a non-fatal warning that
liveness doesn't probe), but never in direction.

### 5. Degenerate / edge states (each has a defined rollup treatment)

Every member ends up in exactly one of these buckets per probe; the table fixes
the verdict and the matrix rendering for each.

| Edge state | How detected | System-cell verdict | Rendering / note |
|---|---|---|---|
| **Missing** — in manifest, not installed | `version --json` spawn fails (binary not on PATH) | `down` | `✗ down — not installed`; remediation = the manifest `install` descriptor (DOC-8): "run `tctl install trusty-search`". Distinct from "installed but down". |
| **Down** — installed but daemon not answering | binary runs, but `health` → no response / connection refused; controller synthesizes a `down` envelope (DOC-1) | `down` | `✗ down — daemon not running`; remediation = `start` (DOC-1 lifecycle): "run `trusty-search start`". |
| **Unreachable / timeout** — process hangs past deadline | per-tool timeout fires (§1.3) | `down` (sub-reason `unreachable`) | `✗ down — unreachable (timed out after 2s)`; never blocks the rest of the rollup. |
| **Older contract** — `contract_version` below target `N` but ≥ floor `F` | `version --json` reports `contract_version` in `[F, N)` | `degraded` | `! degraded — older contract (cv=1, target=2)`; render only the fields that level guarantees; **never hard-fail** (DOC-1 D2). |
| **Below floor / contract-incompatible** — `contract_version < F` | `version --json` reports `cv < F` | `down` with `reason: "contract_incompatible"` | `down` for the member's row + a distinct `contract-incompatible` sub-reason with an upgrade remediation (DOC-9). Does NOT raise the controller-level exit `3` — the stack verdict carries `down`/exit `1` instead. The member cannot be trusted to speak the contract. |
| **Verb not advertised** — member lacks the verb (e.g. claude-mpm has no `config`/lifecycle; a project verb on a system-only tool) | verb absent from `verbs[]` | `n/a` (absorbed) | `— n/a`; **not a failure** (DOC-1 D3 / DOC-3 Q2 graceful-degrade). A system-only tool's *project* column is always `n/a`. |
| **Cross-tool dependency failure** — tool degraded *because* a dep is down | the owning tool's `health` reports `degraded` with `deps[].reachable=false` | `degraded` on the **dependent**; root cause owns the `down` | de-duplicated — see §5.4. |

#### 5.1 Missing vs down vs unreachable (three distinct "not ok"s)

All three roll up to `down`, but the **sub-reason** (and therefore the
**remediation**) differs, so the controller records a `reason` discriminant:

- `missing` → fix is **install** (DOC-8).
- `down` → fix is **start** (DOC-1 lifecycle / launchd).
- `unreachable` → fix is **investigate** (the daemon is up enough to spawn but
  not answering — check logs / restart).

Collapsing these into one "down" loses the actionable distinction the spec wants;
the rollup keeps the sub-reason in the `--json` structure (§8) and the drill-down,
even though the matrix glyph is the same `✗`.

#### 5.2 Older contract → degraded, never hard-fail

This is DOC-1's canonical degradation rule, applied at the rollup. A member whose
advertised `contract_version` is **below the controller's target `N` but at or
above the manifest floor `F`** is fully usable for the fields its level
guarantees; the controller renders its row **`degraded`** with a note ("speaks
cv=1, controller targets cv=2 — some fields unavailable") and rolls only the
fields that level guarantees. It is **never** the reason the stack verdict goes
`down`. A below-floor member (`cv < F`) is the harder case (Open Question 4).

#### 5.3 Verb / scope not applicable → `n/a`, never a failure

Two sub-cases, both → `n/a` (absorbed, contributes nothing to the fold):

- **Verb not advertised.** claude-mpm advertises only
  `["doctor","health","version"]` (DOC-6 §4), so its `config`/lifecycle columns
  are `n/a`. The rollup never penalizes a member for a verb it legitimately does
  not implement (DOC-1 D3 graceful-degrade).
- **Scope not applicable.** A **system-only** member (DOC-3 Q2: trusty-review,
  claude-mpm — `kind = "cli"`/`"orchestrator"`) has **no project layer**, so its
  *project* column is always `— n/a`. Asking a project-scoped verb of it returns
  "unsupported," which the rollup renders as `n/a`, not `down`.

`n/a` is visually distinct from `pending`: `n/a` = "this doesn't apply here";
`pending` = "this applies and is in progress."

#### 5.4 Cross-tool dependency failures — represent once, no cascading

The dependency edges are real and already surface as `degraded` today (grounding
§): trusty-analyze `degraded` ⇔ trusty-search unreachable; trusty-review
`degraded` ⇔ a *required* dep (search) unreachable (its `compute_status()`).
DOC-2 also records static `depends_on` edges (analyze→search;
review→[search,analyze]).

A naive rollup of "search is down" would paint **three** scary failures (search
`down`, analyze `degraded`, review `degraded`). DOC-4 avoids this:

- **Attribute the root cause once.** The tool that is itself `down`/`fail` owns
  the **`down`** verdict (the *root*). trusty-search shows `✗ down`.
- **Dependents render `degraded` with a `because` pointer, not an independent
  failure.** trusty-analyze and trusty-review show `! degraded — blocked by
  trusty-search` (derived from their own `health.data.deps[]` +
  the manifest `depends_on`), and the controller groups them under the root in
  the verdict summary:

  ```
  STACK VERDICT: down
  ROOT CAUSE: trusty-search ✗ down (daemon not running)
    → trusty-analyze degraded (required dep trusty-search unreachable)
    → trusty-review   degraded (required dep trusty-search unreachable)
  → fix the root: run `trusty-search start`   (resolves 2 dependent degradations)
  ```

- **Single remediation.** The remediation surfaced for the cluster is the
  **root's** fix (start trusty-search), not three separate "fix the dependent"
  hints. This is the anti-double-counting rule: *N dependents of one dead root
  produce one root failure + N annotated degradations, never N+1 failures.*

The rollup builds the dependency cluster from two sources, both contract/manifest
data (no tool-specific logic): the manifest `depends_on` (static edges, DOC-2 §3)
and each tool's runtime `health.data.deps[]` (DOC-1; the `{id,required,reachable}`
nodes). When a dependent reports a required dep unreachable AND that dep's own row
is `down`, the controller collapses the dependent into the root's cluster.

### 6. Remediation surfacing

Every non-`ready` cell can carry an actionable fix; the rollup bubbles these up
so the controller output tells the user *what to run*.

- **Source.** DOC-1 `doctor.data.checks[].remediation` (the per-check fix hint;
  `null` when none). The drill-down (§3.2) prints these verbatim under each
  failing/warning check.
- **Synthesized remediation for edge states** (which have no tool-emitted check
  because the tool never ran): the controller fills the gap from the manifest /
  contract:
  - *missing* → the manifest `install` descriptor (DOC-2 §3) → "run `tctl install
    <member>`" (DOC-8).
  - *down* → the DOC-1 `start` lifecycle verb → "run `<binary> start`" (or `tctl
    <member> start`).
  - *older / below-floor contract* → **upgrade** → "run `tctl upgrade <member>`"
    (DOC-9). Upgrade is the single most common cross-cutting remediation, so the
    rollup ties contract-version and out-of-date-version cells straight to the
    DOC-9 flow.
- **Top remediation in the summary.** The default view surfaces the **single
  highest-leverage fix** (the root cause of the worst cluster, §5.4) on the
  verdict line; `-v` lists them all. Ordering: fix `down` roots first (they unblock
  dependents), then `degraded`, then `pending` (often "just wait / index").
- **Redaction.** Remediation strings are contract output and MUST already be
  redacted by the emitting tool (DOC-1 D8); the controller passes them through
  unchanged and never re-introduces secrets.

Ties: DOC-9 (upgrade) is the remediation for stale-version / older-contract
cells; DOC-8 (install/bootstrap) is the remediation for *missing* members and for
project `pending` ("ensure project" — run the auto-config index).

### 7. Exit-code semantics

`tctl stack doctor` / `tctl stack health` are scriptable; their exit codes derive
from the **stack verdict**, consistent with DOC-1 D5 (`0` ok · `1` fail/down ·
`2` degraded/warn · `3` contract/usage error) and the system-global vs
project-local distinction.

| Stack verdict | Exit code | Rationale |
|---|---|---|
| `ready` | **0** | nothing to do |
| `pending` (project-only; system all `ready`) | **0** (default) | DOC-3: project-pending is **not broken** — usable now. CI that just provisioned a box and hasn't indexed yet should not see a non-zero "failure." An opt-in `--fail-on-pending` flag may be added later for polling use cases, but v1 defaults to 0. |
| `degraded` | **2** | mirrors DOC-1 D5 `2 = degraded/warn`; usable but impaired |
| `down` | **1** | mirrors DOC-1 D5 `1 = fail/down`; the stack is broken |
| controller/usage error (bad flag, unknown member, manifest unreadable) | **3** | DOC-1 D5 `3` — produced at the controller boundary, not from a verdict |

**The exit code reflects the SYSTEM track**, by the same logic as §2.2: a project
`pending` (or even a project-scope `fail` per Open Question 3) does not by itself
make a CI gate fail, because its blast radius is one repo, while a system `down`
must. This makes `tctl stack doctor` a sound CI gate for "is the *machine's* stack
healthy" without false-failing on a not-yet-indexed checkout.

### 8. Output formats

#### 8.1 Human (default)

The matrix + drill-down of §3 — a compact summary by default, full per-check
detail under `-v`. Designed to be skimmable: one glance at the cells, one verdict
line, one top remediation.

#### 8.2 `--json` (machine — the rollup structure)

`tctl stack doctor --json` (and `stack health --json`) emit the **full rollup
structure** so DOC-7's UI renders the *same* rollup and DOC-10's harness asserts
on it. Proposed shape (illustrative; the exact serde struct is implementation-time
and lives with the controller, reusing `trusty_common::contract` types for the
embedded per-member envelopes):

```json
{
  "stack_version": "2026.06-1",
  "scope": "all",
  "project": { "id": "Users_mac_workspace_my-project", "display": "my-project" },
  "verdict": "degraded",
  "exit_code": 2,
  "summary": { "ready": 2, "degraded": 1, "pending": 1, "down": 0, "na": 2 },
  "note": "`na` is included to distinguish intentionally-blank cells from missing data; it does not participate in the verdict fold"
  "members": [
    {
      "id": "trusty-search",
      "kind": "daemon",
      "contract_version": 1,
      "cells": {
        "system":  { "verdict": "ready",   "reason": null },
        "project": { "verdict": "pending", "reason": "no_index",
                     "remediation": "trusty-search index" }
      },
      "checks": [ /* full doctor.data.checks[] verbatim, scope-tagged */ ],
      "envelope_status": "warn"
    },
    {
      "id": "trusty-analyze",
      "kind": "daemon",
      "contract_version": 1,
      "cells": {
        "system":  { "verdict": "degraded", "reason": "dep_degraded",
                     "because": "trusty-search", "remediation": "trusty-search start" },
        "project": { "verdict": "na" }
      },
      "health": { "deps": [ { "id": "trusty-search", "required": true, "reachable": false } ] }
    }
  ],
  "clusters": [
    { "root": "trusty-search", "root_verdict": "down",
      "dependents": ["trusty-analyze", "trusty-review"],
      "remediation": "trusty-search start" }
  ],
  "remediations": [
    { "member": "trusty-search", "scope": "project", "run": "trusty-search index",
      "kind": "index" }
  ]
}
```

Key machine fields: `verdict` + `exit_code` (the headline), `summary` counts,
per-member `cells.{system,project}.{verdict,reason,because,remediation}`, the
embedded per-member `checks[]`/`health` (verbatim DOC-1 payloads), and the
`clusters[]` block that encodes the de-duplicated dependency root-cause grouping
(§5.4) so the UI can render "fix the root" without re-deriving the graph.

DOC-7 renders this identically to the CLI matrix (it is a thin link-out control
plane — spec §56 — so it consumes the rollup rather than recomputing it), and
DOC-10's isolation harness asserts on `verdict`/`exit_code`/`summary` to validate
end-to-end stack health in a clean VM.

---

## Dependencies

### Consumes (inputs)
- **DOC-1** (Accepted) — the per-verb JSON schemas: the uniform envelope, the
  doctor/health `data` shapes, the `ok|warn|fail|pending|skipped` and
  `running|degraded|down` vocabularies, the exit-code mirror (D5), the
  `version --json` `verbs[]` advertisement, and the older-contract degrade rule
  (D2). The rollup is a pure fold over these envelopes.
- **DOC-2** (Accepted) — the manifest registry the rollup iterates: which members
  exist, their `binary`, `kind`, `min_contract_version`, `depends_on`, and the
  `install` descriptor used for *missing*-member remediation.
- **DOC-3** (Accepted) — the scope model: system-global vs project-local,
  CLI-only/orchestrator tools as system-only (no project layer), and the §9
  rollup-interplay rules (system failures dominate; project `pending` is local /
  not broken) that §2.2 makes concrete.

### Produces (consumed by)
- **DOC-5** — the controller CLI surfaces `tctl stack doctor` / `tctl stack
  health`, which are the rendered front-ends of this rollup.
- **DOC-7** — the web UI renders the same `--json` rollup structure (§8.2).
- **DOC-10** — the isolation harness asserts on the rollup verdict/exit code/
  summary to validate stack health end-to-end.

> These edges match the README dependency graph (DOC-4 consumes DOC-1 + DOC-2 +
> DOC-3; produces into DOC-5, DOC-7, DOC-10).

## Grounding (exists vs. net-new)

Source-first audit (2026-06-08): the rollup's *inputs* are partially grounded in
existing daemon `GET /health` bodies — and, critically, the **status semantics**
the rollup needs already exist as prior art in trusty-review's `compute_status()`.

- **Best existing prototype — trusty-review `GET /health`
  (`compute_status()` + `deps{}`).** Confirmed in
  `crates/trusty-review/src/service/handlers.rs`: `HealthResponse` carries
  `status:"ok"|"degraded"`, `inference`, and a real `deps:{ trusty_search:{
  required, reachable }, trusty_analyze:{ required, reachable } }` block.
  `compute_status(inference, &deps)` returns `"degraded"` when inference is bad
  **or any *required* dep is unreachable**, and `"ok"` otherwise — **optional
  deps never degrade status** (`health_optional_dep_down_stays_ok`). This is
  exactly the cross-tool dependency-aware rollup semantics DOC-4 §5.4 generalizes
  to the stack: a dependent goes `degraded` when its *required* dep is down, and
  the `required` flag governs severity. DOC-6 names this the closest match to the
  target envelope `status` semantics and the **prototype for the DOC-4 rollup**.
- **trusty-analyze `GET /health` (`crates/trusty-analyze/src/service/mod.rs`).**
  `HealthResponse{ status, version, search_reachable }`; `status` is `"degraded"`
  ⇔ `search_reachable == false` (200 vs 503). A minimal, single-dependency
  instance of the same rule: degraded ⇔ a required dependency (trusty-search) is
  unreachable. Maps onto `health.data.deps[]` (DOC-1) and feeds §5.4's "represent
  the dependency failure once" rule.
- **trusty-memory `GET /health` (`crates/trusty-memory/src/web.rs`).**
  `HealthResponse{ status:"ok"|"degraded", detail?, version, uptime_secs, ... }`;
  `detail` is populated only when degraded (a store/recall round-trip probe, #71),
  and `uptime_secs` since `started_at`. Closest to the target `health.data` split
  (status in the envelope, telemetry in `data`, a triage `detail` phrase when
  degraded).
- **Net-new:** the **cross-tool aggregation** itself — the manifest-driven
  collection loop, the unified four-value stack-verdict vocabulary (§2.0), the
  scope-aware system-global/project-local fold (§2.2), the tools × scope matrix
  artifact (§3), the de-duplicated dependency root-cause clustering (§5.4), and the
  stack-level exit-code mapping (§7). No tool produces a *stack-wide* rollup today;
  each only reports its own health. The per-tool inputs are largely grounded; the
  rollup over them is new (and thin once DOC-1 lands and tools emit envelopes —
  DOC-6).

## Cross-cutting notes

- **Contract-versioning behavior:** DOC-4 applies DOC-1's canonical rule at the
  rollup — a member on an older (but ≥-floor) `contract_version` renders its row
  **`degraded`**, never a hard `down`, and the controller rolls only the fields
  that level guarantees (§5.2). A below-floor member is the one open edge (Open
  Question 4).
- **Security / secrets:** the rollup carries through tool-emitted `detail` and
  `remediation` strings verbatim; these are already redacted at the source
  (DOC-1 D8). The controller introduces no new output that could leak secrets and
  never re-derives a tool's config.
- **Zero tool-specific logic:** every rule above operates on generic envelope
  fields (`status`, `scope`, `deps[]`) and manifest fields (`kind`, `depends_on`,
  `install`) — never on a member's identity. Swapping claude-mpm → trusty-mpm
  (DOC-2 §7 / DOC-6 §6) requires no rollup change.

## Remaining work

- [x] Define the unified stack-verdict vocabulary (`ready|degraded|pending|down`;
      `skipped` absorbed) and the source→verdict mapping (§2.0)
- [x] Define the combine/precedence lattice (`down ≻ degraded ≻ pending ≻ ready`)
      and the three folds (§2.1, §2.3)
- [x] Make scope-awareness concrete: system-global drives the verdict; project
      `pending` is local and never breaks the stack (§2.2)
- [x] Specify inputs, manifest-driven collection, parallelism, and per-tool
      timeout / hung-tool resilience (§1)
- [x] Specify the tools × scope matrix + verbose drill-down artifact (§3)
- [x] Reconcile `stack health` (fast) vs `stack doctor` (deep) into one verdict
      vocabulary (§4)
- [x] Define every degenerate/edge state's rollup treatment, incl. the
      de-duplicated cross-tool dependency clustering (§5)
- [x] Define remediation surfacing + ties to DOC-8 (install) / DOC-9 (upgrade) (§6)
- [x] Define exit-code semantics from the verdict, system-track-driven (§7)
- [x] Define human + `--json` output formats; the `--json` rollup structure DOC-7
      renders (§8)
- [x] **Owner: resolve all 6 design decisions** (completed 2026-06-08)
- [ ] Team review (pending)
- [ ] *(implementation-time)* finalize the controller-side rollup serde struct
      (reusing `trusty_common::contract` types) and the matrix renderer

## Resolved Decisions

**All decisions below are owner-approved (2026-06-08).**

1. **Per-tool timeout defaults & override (APPROVED):** Ship **2 s for `health`,
   10 s for `doctor`** per member. Parallel collection with hung tools
   synthesized as `unreachable`. Override via a single controller flag
   **`--timeout=<secs>`** in v1. Per-member manifest timeouts deferred to later
   release. *Implemented in §1.3.*

2. **Verdict value name: `ready` vs `ok` (APPROVED):** Stack-level verdict uses
   **`ready`** (matching DOC-3's readiness-ladder language and reading naturally
   on the matrix). Individual cells may still echo the source `ok` for envelope
   compatibility. Purely a naming choice with no behavioral effect. *Implemented
   in §2.0 and mapping function.*

3. **Project-scope `fail`/`down` isolation (APPROVED):** A genuine project-scope
   `fail`/`down` (distinct from `pending`) **degrades only the project cell**, 
   not the global verdict, because its blast radius is one repo. The exit code
   stays **system-track-driven** — a broken index in one checkout is not a
   machine-level stack failure. Surface such failures prominently in the project
   column and in `-v`, but do not fail a CI gate checking system health.
   *Implemented in §2.2 and §7.*

4. **Below-floor / contract-incompatible member (APPROVED):** Render as **`down`**
   with a distinct `reason: "contract_incompatible"` sub-reason and an
   **upgrade** remediation (DOC-9). The member cannot be trusted to speak the
   contract. Does **NOT** raise the controller-level exit `3` (reserved for
   controller usage errors per DOC-1) — the stack verdict carries `down`/exit `1`
   instead. Distinct from a ≥-floor older contract, which is `degraded`.
   *Implemented in §5 edge-states table and §6 remediation.*

5. **Project-`pending` exit code (APPROVED):** Default to **`pending → exit 0`**
   (so the common "is the machine healthy" gate never false-fails). An opt-in
   `--fail-on-pending` flag may be added later if a polling use case materializes;
   do not reserve a new exit code in v1. *Implemented in §7 exit-code table.*

6. **`n/a` in `--json` summary (APPROVED):** Include **`na` count** in the
   `summary` so the UI distinguishes intentionally-blank cells from missing data.
   Keep `na` **OUT of the precedence fold** — it never affects the verdict.
   *Implemented in §8.2 JSON structure and cross-referenced in the note.*
