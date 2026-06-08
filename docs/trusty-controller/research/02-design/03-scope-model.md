# DOC-3 — Scope Model (System vs Project)

**Status:** Accepted (owner-approved)
**Source spec:** ../01-spec/trusty-end-to-end-setup.md

## Purpose

Specify the **behavioral model** behind the scope axis: the two layers (system
vs project), their layered readiness ladders, verb scope-polymorphism,
idempotency, blast radius, ensure-system-then-project ordering, config
precedence, and the shared project-identity convention.

This doc owns the **model**; DOC-1 owns the **wire format** (the
`--scope project|system|all` flag and the per-check `scope` field, recorded as
DOC-1 D7). Where this doc names enum values (`pending`, `ok`, `running`, …) it
uses DOC-1's vocabularies (D3 envelope, D4 status enums) verbatim and does not
redefine them.

---

## DESIGN

### 1. The two layers

Some stack members are **two-layer**: a singleton **system** resource (a daemon)
plus per-project **state** that singleton serves. Scope names which layer a verb
or check addresses. This is not invented — it already exists in the codebase:
trusty-search runs **one machine-wide daemon** whose `IndexRegistry`
(`DashMap<IndexId, Arc<IndexHandle>>`) holds **many named per-project indexes**;
trusty-memory runs one daemon serving **per-project palaces**.

| Aspect | **System** layer | **Project** layer |
|---|---|---|
| Cardinality | one per machine | one per repo / cwd |
| Owns | daemon process, listening port, runtime/model, installed binary version | index, palace, project `.mcp.json` entry, per-project config overrides |
| Lifecycle owner | `cargo install` + daemon supervisor (launchd / systemd) | per-launch auto-config ("ensure project", UUC1) |
| Failure meaning | global — affects every project/session on the box | local — affects only this repo |
| Examples (trusty-search) | the daemon, its port lock, embedder model, binary version | the project's `IndexId`, its on-disk redb corpus, freshness vs HEAD |
| Examples (trusty-memory) | the daemon, its port | the project's palace |
| Single-layer members | — | — |

Not every tool has both layers. **CLI-only / serve-only tools** (e.g.
trusty-review, which today implements only `serve`) are effectively
**system-only**: they have no per-project state to ensure. The model must degrade
to "this tool has no project layer" rather than assuming every tool is
two-layer. (RESOLVED — Resolved Decisions Q2: such tools are modeled as
**system-only** members with no project layer; project-scoped verbs report
"unsupported" via DOC-1's `verbs[]` graceful-degrade.)

**Status is composite.** A daemon can be `running`/`healthy` (system-ok) while a
given project is `pending` (unindexed). The two layers are reported and
remediated independently; a healthy system with a pending project is a normal,
expected state — **not** a failure.

### 2. Layered readiness ladders

Readiness is **two stacked ladders**. The project ladder only becomes meaningful
once the system ladder reaches its top rung (you cannot index against a dead
daemon — see §6 ordering).

**System ladder** (one per machine, per tool):

```
installed → running → healthy → version-ok
```

| Rung | Meaning | Grounded in |
|---|---|---|
| `installed` | binary present on PATH / discoverable via the manifest (DOC-2) | `cargo install <tool>`; manifest registry |
| `running` | daemon process is up and bound to its port | trusty-search `GET /health` reachable; PID lockfile present |
| `healthy` | daemon answers introspection and reports itself serviceable | `GET /health` → `running` (DOC-1 D4 health enum) |
| `version-ok` | installed/running version satisfies the manifest's pinned/min version and the `contract_version` floor | DOC-2 pinned version; DOC-1 D2 `contract_version` |

**Project ladder** (one per repo, per two-layer tool):

```
configured → exists → fresh → ready
```

| Rung | Meaning | Grounded in |
|---|---|---|
| `configured` | the project is wired up — `.mcp.json` entry present, project registered with the daemon | `POST /indexes` registration; `.mcp.json` |
| `exists` | the per-project state has been created (index/palace exists, even if empty) | `IndexHandle` present in registry; redb corpus on disk |
| `fresh` | the state is up to date with the project's current content | trusty-search incremental reindex skip via sha2 content fingerprint; `git` HEAD vs indexed HEAD (`head_sha`) |
| `ready` | configured + exists + fresh — the project is fully usable | composite of the three rungs above |

**The load-bearing rule (DOC-1 `pending` semantic):**

> An **unindexed project on a healthy daemon is `system-ready, project-pending`,
> NOT broken.**

A project that is `configured` but not yet `exists`/`fresh` maps to DOC-1's
doctor check `status: "pending"` (D4) — distinct from `fail`. This is precisely
the UUC1 "trusty-search can be set up immediately but needs time to index"
scenario: the controller shows progress, not an error. `pending` is observable
("indexing in progress / queued") and resolves to `ok` once the project ladder
reaches `ready`.

### 3. Verb scope-polymorphism

Each verb addresses one or both layers. (Verb **presence** is advertised per-tool
via DOC-1 D3 `verbs[]`; this table is the **default scope semantics** the
controller applies when a tool implements the verb.)

| Verb | Scope(s) | Notes |
|---|---|---|
| `install` | **system only** | machine-wide; runs once (§4) |
| `upgrade` | **system only** | replaces the binary for all projects |
| `restart` | **system only** | bounces the daemon → disrupts every project/session |
| `start` | **system only** | brings the daemon up |
| `stop` | **system only** | takes the daemon down |
| `health` | **both** | system: daemon liveness; project: per-project state status |
| `doctor` | **both** | emits a mix of system- and project-scoped checks, each tagged with its own `scope` (DOC-1 D7) |
| `config` | **both** | system: daemon-level resource (port, model); project: per-project overrides. **Project config overrides system defaults** (§7), as a read-time resolution rule. The controller's `config` verb is **read/report-only** (RESOLVED — Resolved Decisions Q3). |
| index / reindex | **project only** | valid only once the system layer `exists` |
| palace-create | **project only** | valid only once the system layer `exists` |

**Default-scope rule (matches DOC-1 D7 wire format):**

- Invoked **inside a project directory** (a git root or marker is detected up the
  tree) → default scope is **`all`** (act on both layers).
- Invoked **outside any project** → default scope is **`system`**.
- An explicit `--scope project|system|all` always overrides the default.

This is grounded in the existing `detect_project()` walk
(`crates/trusty-search/src/detect.rs`): "am I in a project?" is already a
first-class, implemented question.

### 4. Idempotency contract

The two layers have **different idempotency lifetimes**, which is the heart of
the UUC1 auto-config engine:

- **`install` runs once.** A second `install` against an already-installed system
  is a no-op (report "already installed at version X") — it must never reinstall
  destructively. Grounded: `cargo install` is itself idempotent-ish; the
  controller wraps it with a presence check first.
- **"ensure project" runs every launch and MUST no-op when already set up.**
  Every time claude-mpm launches in a project directory, the controller runs the
  ensure pass. When the project is already `ready`, the ensure pass changes
  nothing and returns quickly.

**Shape of the ensure pass — check → act → verify:**

1. **Check** — read current readiness without mutating: is the system layer
   `version-ok`? is the project layer `configured`/`exists`/`fresh`?
2. **Act** — perform *only* the missing steps. If `configured` is missing, write
   the `.mcp.json` entry and register the project; if `exists` is missing, create
   the index/palace; if not `fresh`, trigger an (incremental) reindex.
3. **Verify** — re-read readiness to confirm the action moved the ladder up (or,
   for long-running steps like indexing, that it is now `pending` and
   progressing).

Each underlying operation is itself idempotent so the pass is safe to repeat:
trusty-search's `POST /indexes` is explicitly idempotent (re-registering an
existing id returns `created: false`, not an error), and incremental reindex
**skips unchanged files** via sha2 content fingerprints. The controller composes
these idempotent primitives; it does not add its own mutation state.

### 5. Blast radius

Every operation carries a **blast-radius tag** derived from its scope, so the
controller can warn before acting:

- **System-mutating ops** (`install`, `upgrade`, `restart`, `stop`) disrupt
  **every project and session** on the machine. The controller MUST surface this
  ("restarting trusty-search will interrupt all active projects — continue?")
  before executing, especially from the UI.
- **Project ops** (index, palace-create, project `config`) affect **only the
  current repo** and MUST **never implicitly trigger a system op.** If a project
  op finds the system layer not ready, it **reports the unmet system precondition
  and stops** — it does not silently restart or reinstall the daemon. (The
  *ensure* pass may sequence system-then-project deliberately — §6 — but a bare
  project verb never escalates on its own.)

This ties directly to DOC-1 D7's scope tagging: because each verb and each
doctor/health check already carries a `scope`, the controller derives blast
radius mechanically from the tag rather than hard-coding per-tool knowledge.

### 6. Ordering guarantee (progressive readiness)

**Ensure system → then project, always.** You cannot index against a dead
daemon, so the ensure pass climbs the system ladder to `version-ok` (or at least
`running`/`healthy`) *before* touching the project ladder. Within a layer the
ladder is climbed bottom-up (`installed` before `running` before …).

This produces **progressive readiness**, the UUC1 behavior: the controller can
report "system ready; project indexing (pending) — usable now, fully ready in
~N s" rather than blocking. Cross-tool, the same ordering applies per tool; tools
are independent (a slow trusty-search index does not gate trusty-memory).

### 7. Config precedence

**Project config overrides system defaults.** The system layer establishes
machine-wide defaults (e.g. the daemon's default model, default port); a project
may override applicable knobs for its own scope. Precedence, highest-to-lowest:

```
project config  >  system config  >  tool built-in default
```

This mirrors the daemon's own existing precedence for env knobs
(`shell env > daemon.env > tier default` in trusty-search's memory policy) — the
controller's scope-precedence rule is the cross-tool generalization of that
pattern. **Note the spec non-goal** ("not a tool-internal config editor"): the
controller's `config` verb is **read/report-only** — it surfaces effective values
and their precedence. "Project overrides system" is a **read-time resolution
rule**, not the controller mutating files. The controller MAY *dispatch* a tool's
own `config`-write subcommand, but it **never edits a tool's internal config
files directly** (RESOLVED — Resolved Decisions Q3, reconciling the spec
non-goal).

### 8. Shared project-identity convention

A single, stable rule binds every project-scoped op to the right cwd. This
convention is **defined here** and referenced by DOC-6 (conformance — tools must
agree on it) and DOC-8 (install/bootstrap — the auto-config engine uses it).

**Canonical rule (grounded in `detect_project()`; owner-approved, ADR-0008).**
Walk up from the cwd and take the **nearest enclosing git repository root** (the
first ancestor directory containing `.git`) as the project root. The **canonical
stable project id is the full-path slug of that root** (the `id_from_path`
scheme, e.g. `Users_mac_workspace_my-project`), used as both the trusty-search
`IndexId` and the trusty-memory palace id, so a single identity keys all
per-project state across tools. The git-root **basename is a display-only alias**.

Detection precedence (matches the existing implemented walk):

1. nearest ancestor with `.git` → **git root** (strongest signal);
2. else nearest ancestor with a tool marker (e.g. `.trusty-search`, `.claude/`,
   `CLAUDE.md`) → **marker root**;
3. else **fallback** to the cwd itself, derive the id from the **cwd path-slug**,
   and **warn** (`DetectionMethod::Fallback`) — the controller does **not**
   refuse.

**Id-derivation decision (RESOLVED — see Resolved Decisions Q1 and ADR-0008).**
The codebase currently has **two** id schemes that disagree:

- `detect.rs` uses the **basename** of the root (`my-project`), which is short
  and human-friendly but **collides** when two repos share a basename (e.g.
  `~/work/api` and `~/personal/api`).
- `fs_discovery.rs::id_from_path` uses a **full-path slug**
  (`Users_mac_workspace_my-project`), which is collision-free.

The live daemon registry shows **both forms registered for the same root** today
(e.g. `trusty-tools` *and* `Users_mac_workspace_trusty-tools`), confirming the
ambiguity is real, not hypothetical. **The full-path slug scheme wins** as the
single canonical id (collision-free, stable across restarts, already proven
`stable-and-safe` by `id_from_path`'s test); the basename is a display alias
only. This reconciles the live `detect.rs`-vs-`fs_discovery.rs` inconsistency and
is recorded as the authoritative decision in
[ADR-0008](../../../adr/0008-project-identity-convention.md).

**Edge cases the rule names (RESOLVED — see Resolved Decisions):**

- **No git root and no marker** — id = **cwd path-slug + `Fallback` warning**
  (never refuse). (Q1.)
- **Git worktrees** — each worktree gets its **own** id keyed on its
  working-directory path. (Q4.)
- **Monorepo subdirs** — all subdirs share the **nearest enclosing git root's**
  id/index/palace; per-subdir sub-projects only via an explicit marker (e.g.
  trusty-search's existing `trusty-search.yaml` multi-index). (Q4.)
- **Multiple projects sharing one daemon** — fine by design (the daemon is
  multi-index), *provided* ids are collision-free; this is the core argument for
  the path-slug scheme.

### 9. Rollup interplay (feeds DOC-4)

Scope is one axis of DOC-4's **tools × scope matrix**. The rollup rules follow
directly from §1–§5:

- **System failures are global.** A `fail`/`down` system check for a tool means
  that tool is unusable for *every* project; it dominates the rollup and the
  controller surfaces it as a stack-level problem.
- **Project `pending` is local / in-progress.** A `pending` project check is
  scoped to the current repo and is **not** a stack failure — DOC-4 must render
  it as "in progress / setup pending," distinct from `fail`. This is the matrix
  expression of the §2 "unindexed ≠ broken" rule.
- The matrix therefore reads naturally: rows = tools, columns =
  {system, project}, cells use DOC-1 D4 status enums; the worst **system** cell
  drives the global verdict, while **project** cells annotate per-repo readiness.

### 10. Orchestrator forward-compatibility

Per DOC-0's A4 scope framing, the **orchestrator is a pluggable system-layer
member**, not a special case:

- **claude-mpm** (Python, external) is the current orchestrator. In this model it
  is a **system-layer** member: machine-wide install, system-scoped
  `install`/`upgrade`/`restart`, and a `doctor`/`health` surface via its Python
  contract adapter (DOC-6). It may also carry a thin project layer (per-project
  `.mcp.json` wiring) — that is exactly the "ensure project" surface.
- **trusty-mpm** (in-house Rust replacement, not yet ready) slots into the **same
  system-layer role** when it ships. Because the scope model treats the
  orchestrator generically (system member with optional project wiring), swapping
  claude-mpm → trusty-mpm requires **no change to the scope model** — only a
  manifest entry change (DOC-2) and a different contract adapter (DOC-6).

The scope model thus stays orchestrator-agnostic: it never names claude-mpm or
trusty-mpm in any rung, tag, or precedence rule.

---

## Dependencies

### Consumes (inputs)
- **DOC-0** — the chosen `<name>` (`trusty-controller` / `tctl`) and the
  orchestrator-swap forward-compat requirement (A4).
- **DOC-1** — bidirectional. DOC-1 owns the `scope` **wire format** (D7) and the
  envelope/status enums (D3/D4); this doc consumes those and supplies the
  **behavioral model** behind them.

### Produces (consumed by)
- **DOC-1** — feeds the `scope` schema semantics back into the contract
  (bidirectional edge).
- **DOC-4** — the tools × scope rollup rules (§9).
- **DOC-5** — verb scope-polymorphism + default-scope rule drive CLI dispatch.
- **DOC-8** — the ensure-system-then-project ordering, idempotency, and
  project-identity convention drive install/bootstrap.

> These edges match the README dependency graph
> (`DOC-3 → DOC-1, DOC-4, DOC-5, DOC-8`; `DOC-1 ◄──► DOC-3`).

## Grounding (exists vs. net-new)

- **Already exists:**
  - Singleton-daemon + per-project-state architecture: trusty-search
    `IndexRegistry: DashMap<IndexId, Arc<IndexHandle>>` (one daemon, many named
    indexes); trusty-memory one daemon, per-project palaces.
  - Project-identity logic: `crates/trusty-search/src/detect.rs`
    (`detect_project` → git-root → marker → fallback-basename) and
    `crates/trusty-search/src/service/fs_discovery.rs::id_from_path` (path-slug
    id). `trusty_common::project_discovery::discover_claude_projects` shares the
    `.git`/`.claude`/`CLAUDE.md` marker convention.
  - Idempotency primitives: `POST /indexes` returns `created: false` on
    re-register; incremental reindex skips unchanged files via sha2 fingerprints;
    `head_sha` enables HEAD-vs-indexed freshness checks.
  - `pending`-shaped semantics: UUC1 ("setup immediately, index over time") in
    the spec; DOC-1 D4 already reserves `status: "pending"`.
- **Net-new:**
  - The cross-tool **formalization** of the scope axis as a shared contract
    concept: the two named ladders, verb scope-polymorphism table, blast-radius
    tagging, ordering guarantee, and config-precedence rule.
  - A **single canonical** project-identity rule reconciling the two existing,
    currently-divergent id schemes (RESOLVED — Resolved Decisions Q1; ADR-0008).
  - Modeling **single-layer (CLI-only) tools** like trusty-review inside a
    model built around daemon+project state (RESOLVED — Resolved Decisions Q2:
    system-only members with no project layer).

## Cross-cutting notes

- **Project-identity convention is DEFINED HERE** (§8): git root → stable project
  id → index-id / palace-id. Referenced by DOC-6 (tools must agree) and DOC-8
  (auto-config uses it). Keep the chosen `<name>` (DOC-0) stable since it appears
  in project-scoped config keys.
- **Status vocabulary is owned by DOC-1** (D4): this doc uses `pending`, `ok`,
  `running`, `degraded`, `down` exactly as defined there and adds no new values.
- **Orchestrator-swap forward-compat** (DOC-0 A4): the model treats the
  orchestrator as a generic system-layer member (§10).

## TODO / Remaining work

- [x] Formalize the two layers + composite-status rule
- [x] Define the system and project readiness ladders (incl. `pending` semantic)
- [x] Define verb scope-polymorphism + default-scope rule
- [x] Define the idempotency contract (check → act → verify ensure pass)
- [x] Define blast-radius tagging
- [x] Define ensure-system-then-project ordering
- [x] Define config precedence
- [x] Define the shared project-identity convention + edge cases (canonical rule
      now recorded in §8 + ADR-0008)
- [x] Define rollup interplay (feeds DOC-4) and orchestrator forward-compat
- [x] **Owner: resolve the open questions** (all five resolved below; Q1 id
      scheme unblocks DOC-6/DOC-8 and is recorded in ADR-0008)
- [x] Coordinate final `scope` field semantics back into DOC-1 (bidirectional
      edge — DOC-1 D7; the config-provenance `sources[].scope` sub-vocabulary
      `{env|project|system}` is documented in DOC-1 as distinct from the D7
      `--scope {project|system|all}` wire vocabulary)
- [x] Team review → Accepted (owner-approved)

## Resolved Decisions

The five questions previously escalated to the owner are now **resolved
(owner-approved)**. Each resolution is folded into the design body above; the
authoritative statements are recorded here.

1. **Canonical project-id rule (no-git-root case; basename vs path-slug).**
   **Resolution:** the canonical project id is the **full-path slug** of the
   nearest enclosing git root (collision-free, e.g.
   `Users_mac_workspace_my-project`); the git-root **basename is a display-only
   alias**. When there is **no git root and no marker**, derive the id from the
   **cwd path-slug** and emit a `Fallback` warning — the controller does **not**
   refuse. This resolves the live-codebase inconsistency between the **basename**
   scheme in `detect.rs` and the **full-path slug** scheme in
   `fs_discovery.rs::id_from_path` (both currently registered in the daemon for
   the same root); the **slug scheme wins**. Authoritative record:
   [ADR-0008](../../../adr/0008-project-identity-convention.md). Unblocks DOC-6
   (cross-tool agreement) and DOC-8 (auto-config keys). See §8.

2. **CLI-only / serve-only tools (e.g. trusty-review).** **Resolution:** modeled
   as **system-only** members with **no project layer**; the project ladder is
   "not applicable," and project-scoped verbs report **"unsupported"** via
   DOC-1's `verbs[]` graceful-degrade. See §1.

3. **Is `config` read-only?** **Resolution:** the controller's `config` verb is
   **read/report-only** — it surfaces effective values and their precedence.
   "Project overrides system" is a **read-time resolution rule**, not the
   controller mutating files. The controller MAY **dispatch a tool's own
   `config`-write subcommand**, but it **never edits a tool's internal config
   files directly**. This reconciles the spec non-goal ("not a tool-internal
   config editor"). See §7.

4. **Worktrees / monorepo subdirs.** **Resolution:** identity = the **nearest
   enclosing git root**, so **monorepo subdirs share one** id / index / palace
   (matches today's `detect_project` walk, which stops at the first `.git`). A
   **worktree gets its own id keyed on its working-directory path**. Per-subdir
   sub-projects exist **only** via an explicit marker (e.g. trusty-search's
   existing `trusty-search.yaml` multi-index file). Recorded in
   [ADR-0008](../../../adr/0008-project-identity-convention.md). See §8.

5. **Is "fresh" controller-observable or tool-reported?** **Resolution:**
   **tool-reported** — the tool emits `fresh`/`pending` through its
   `doctor`/`health` JSON (DOC-1); the controller **never inspects the repo or
   the index itself**, preserving zero tool-specific logic. Freshness is a
   contract field the tool must emit, not something the controller computes.
   See §2.
