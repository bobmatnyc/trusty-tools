# DOC-39 — trusty-code Harness UI: Context-First Interactive Surface

**Status:** Draft
**Subsystem:** trusty-code — API surface (JSON-RPC + events) primarily; SPA (web/Tauri) client downstream
**Owner:** Engineering (trusty-code)
**Last-updated:** 2026-07-16
**Spec ID:** `SPEC-TCUI-01~draft` … `SPEC-TCUI-09~draft` (DOC-39)
**Epic:** trusty-code interactive harness UI (umbrella issue to be linked by the PM)
**Builds on:**
- [`docs/trusty-code/vision-and-architecture-spec.md`](../trusty-code/vision-and-architecture-spec.md)
  — Axiom 3 (§, line 84): **layer priority API → CLI → TUI → Web**; "UIs are deferred
  thin clients … contain NO logic of their own". This spec is that axiom applied: its
  normative core is the **API delta**, not the pixels. §2.1 restates that axiom as an
  **architectural constraint** rather than a build order, and is binding over every
  other section of this document.
- [`docs/trusty-code/parity-spec.md`](../trusty-code/parity-spec.md) — normative for
  cross-model *comparison runs* only; the bake-off is how a harness is **scored**, not
  what one **is** (§1.2).
- Infinite Sessions epic **#2343** (LIVE, PM-only) — memory dual-write, cadence
  compression, goal slots, working-context budget.
- `docs/trusty-code/UI/Harness UI Rethink Proposal Explainer.pdf` (the design handoff:
  core bet, 10 principles, visual system, tokens, shell skeleton) and its companion
  `handoff/` screens 7a–11b. **Design input, not a behavior contract** — where the
  proposal conflicts with shipped behavior, this spec corrects it and says so (§4.2).

**Cross-ref:** the proposal's referenced "interactive wireframe doc (turns 1–11,
options linked by id)" is **absent from the repository** — it likely holds the 8a/8b
and 9a/9b option rationale this spec had to reconstruct from the artifacts (§7, Q1).

> **Scope note.** This is a **functional spec**, not a design doc. It states what the
> product must do — domain objects, state transitions, the API surface each UI need
> requires, and acceptance criteria — without prescribing the implementation. It
> deliberately does **not** restate the visual system: tokens, shell skeleton, and
> component CSS are owned by the design handoff PDF (§3–§6) and referenced here (§8).
> The PR carrying this doc opens **no** Rust changes.

---

## 1. Purpose, core bet, and non-goals

### 1.1 The core bet {#SPEC-TCUI-01~draft}

**ID:** SPEC-TCUI-01~draft
**Status:** Draft

Traditional coding-agent UIs are a **linear stream of work**. When agents fan out and
prompts are one line, the stream stops answering the only question that matters:
**"what actually drove this change?"** Prompts are thin. The real drivers are **memory
recalls** and **semantic/text/graph search**.

So the harness is **context-first**: every view is built to trace a change back to the
memory, the search hits, and the agent that produced it (PDF §1).

This has a direct, non-obvious consequence that governs the whole spec:

> **The differentiated surface of this product is provenance, and provenance lives in
> the event stream — not in the layout.** trusty-code already runs the searches, scores
> the recalls, and attributes the agents. It just **throws that information away at the
> event boundary** (§5). The mockups are not blocked on design; they are blocked on a
> handful of event fields. That is why Phase 1 (§6.2) is an event-schema change and
> contains almost no UI.

### 1.2 Product framing — settled, not an open question

**trusty-code was always meant to be an interactive product. All coding harnesses are.
This is not an open question and this spec does not relitigate it.**

The one-shot `run-task` CLI is **current implementation state, not the destination**.
The existing vision spec already says so in its own voice — "NOT a one-shot CLI tool
(the `run-task` CLI is one of many entry points; the daemon is primary)" and "a
foundation for later UI layers: TUI, TELGUI, REST — all thin clients over the same
JSON-RPC surface" (vision-and-architecture-spec.md §1). The UI is the **missing surface
of the product trusty-code was always meant to be**.

The parity/bake-off harness is **how you score a harness, not what one is**. Comparison
runs remain normative under the parity spec and are unaffected by this document.

**Platform: an SPA (web/Tauri) driving our API.** Not a TUI, not a terminal renderer.
"Driving our API" is the whole of it: **web and Tauri are two shells over one daemon, and
differ in packaging only** (§2.1). The slash in "web/Tauri" is not a fork.

### 1.3 Non-goals

The proposal has no non-goals. A spec must. **Out of scope for this document:**

1. **Not a general-purpose IDE.** The Project/IDE half (10a) is scope-flagged, not
   scope-committed (§7, Q4). trusty-code does not compete with VS Code. If it ships, its
   file reads are **daemon-served** like everything else (§2.1) — an IDE half is not a
   licence for the UI to touch the disk.
2. **Not a replacement for `trusty-mpm`.** No tmux, no multi-project daemon, no
   worktree orchestration. One tcode instance per project (vision spec §1).
3. **Not multi-user / not collaborative.** Single-operator. Auth and tenancy are
   unaddressed by the proposal and remain unaddressed here (§7, Q3).
4. **No visual-system invention.** Tokens, palette, and shell markup are the PDF's
   (§8). This spec adds no new design language.
5. **Not a workflow/delivery engine.** Surfacing PR/branch state (principle 9) requires
   a subsystem that does not exist (§5.7); it is specified as a domain need and
   explicitly deferred out of Phase 1 (§6.3).
6. **No new persistence engine.** Workstream durability (§4.10) names a requirement and
   a prerequisite; it does not pick a store.

---

## 2. Product framing → what the shell must be

Two nested scopes, read top-to-bottom (PDF §2, principle 3):

- **App context** — the header: branding + the workstream switcher.
- **Workstream context** — the service nav below it.

Everything on screen is one or the other. The chrome (header, service nav, status line)
is **always present** and renders either an *empty* state (no project, tabs locked,
muted status) or a *hydrated* one. **Cold start is the empty state of the same shell,
not a different screen** (principle 4). Screen 7a is that empty state; it is a designed
state, not an error screen (PDF §7).

### 2.1 Foundational constraint — the UI communicates with the daemon {#SPEC-TCUI-09~draft}

**ID:** SPEC-TCUI-09~draft
**Status:** Draft
**Owner directive (Bob).** Binding over every other section of this document.

> **The UI communicates with the daemon. All UI services talk to the daemon; the daemon
> provides all functionality.**

This is **stronger than the layer-priority rule** it derives from. Axiom 3
(API → CLI → TUI → Web) reads as a **process** — an ordering, a thing you do *first*.
Stated as **architecture**, it is not about sequence at all: it is a statement about
**where functionality is permitted to live**. Ordering is a consequence; the constraint
is the point.

**C-1 — The UI is a THIN CLIENT.** No business logic, no local capability, no direct
filesystem, process, or git access **in any UI target**. The UI renders daemon state and
issues daemon calls. That is its entire job.

**C-2 — The daemon is the single source of functionality.** Anything the UI needs MUST
exist as a daemon API — a JSON-RPC method, an event, or both. **If a screen needs
something the daemon cannot answer, that is an API gap to specify — never a UI-side
workaround.** This spec's §5 is that rule applied: every UI need is resolved to an API
delta, and a need with no API is recorded as MISSING rather than absorbed by the client.

**C-3 — Corollary: no capability divergence between targets.** A feature MUST NOT exist
in one UI target and not another. This makes **"web vs Tauri" moot at the functional
level**: both are shells over the same daemon API and MUST be behaviorally identical.
Any difference between them is a packaging difference, never a capability one.

**C-4 — Corollary: Tauri MUST NOT use native fs/dialog APIs, even though it can.**
Reaching for `@tauri-apps/plugin-fs` or a native folder dialog is **not permitted as a
functional path**. It is not a shortcut — it places functionality in the UI layer and
manufactures exactly the divergence C-3 forbids: the web target would then be missing a
capability that no daemon API provides, and the gap would be invisible until someone
opened the app in a browser. A native dialog MAY be used **only as a cosmetic shell over
the same daemon-provided data** — the picker's contents, git-ness, and paths still come
from `project.list_dir` (§5.8). The moment it sources its own data it is a violation.

**AC-19.1** No UI target reads the filesystem, spawns a process, or shells out to `git`
directly. Every such fact arrives from a daemon call or event.
**AC-19.2** A capability matrix of web vs Tauri is **empty by construction** — any row
in it is a spec violation, not a platform note.
**AC-19.3** Client-side derivation of a value the daemon owns is a violation, not an
optimization (see §5.6 AC-17.1's "not recomputed", which is this rule applied early).

> **This constraint resolves §7's Q2 (web vs Tauri) rather than answering it** — the fork
> dissolves instead of being decided. See §7 Q2, now **RESOLVED**.

---

## 3. Domain model {#SPEC-TCUI-02~draft}

**ID:** SPEC-TCUI-02~draft
**Status:** Draft

The proposal introduces vocabulary that does not exist in the runtime. Naming which
objects are **NEW** vs **EXISTING** is the single highest-value thing this spec does —
it converts "the UI is behind" into "these four domain objects are missing."

| Object | Runtime status | Definition |
|---|---|---|
| **Workstream** | **NEW** — no such symbol exists | The unit of work. An infinite thread with state `active · idle · closed` that you *pick up*, never "start over". Name is **inferred** from what you're doing. **N per project.** Resumable across daemon restarts. |
| **Project** | **PARTIAL** — two disagreeing surfaces | A codebase a workstream may bind to. **Three** binding states (§4.2), not two. |
| **Agent** | **PARTIAL** — events exist, no roster | A named worker (PM, Auth, Schema, Test) with a model, a task, todos, and file artifacts. |
| **Monitor** | **NEW** — pure client concept | A live view of one service (Search / Memory / Agents) with a **lifecycle** (§4.6), not a layout choice. |
| **Goal slot** | **EXISTING** — API already shipped | One of exactly **5** durable goal texts, each `Model`- or `Operator`-authored. The mechanism that makes "the workstream never ends" true. **Zero UI surface today** (§4.5). |
| **Artifact** | **PARTIAL** — implied by events | A file a specific agent changed, deep-linkable into the activity viewer with back-nav. |

### 3.1 Workstream (NEW)

- **State:** `active | idle | closed`. Inferred, not set by the operator.
- **Name:** inferred from the first task ("Token rotation hardening"); mutable.
- **Cardinality:** N per project; a workstream binds to **0 or 1** project (§4.2).
- **Durability:** MUST survive daemon restart (§4.10). This is the hard part.

> **Workstream ≠ session.** Today's `SessionRegistry` is a `HashMap` behind a `Mutex`
> whose own module doc says persistence is "(Phase 2+, out of scope)"
> (`session/registry.rs:1-12`). A daemon restart loses the session list — though
> trusty-memory retains turn history via the dual-write sink. **An infinite thread on
> top of a non-durable registry is a contradiction**, and it is the reason workstream
> persistence is called out as a *prerequisite domain object* rather than an endpoint
> (§4.10).

### 3.2 Agent (PARTIAL)

`AgentSpawned` / `AgentStarted` / `AgentDone` / `AgentFailed` / `PmDelegating` events
exist (`events.rs:186-211,312-316`). What's missing for the 10b roster:

- **No snapshot endpoint** — a late-joining client cannot ask "who is running?"; it
  must fold the event stream from the SSE ring buffer.
- **No `model` field** on any agent event. 10b renders `sonnet` on the PM card. Model
  appears only on `LlmRequested`/`LlmResponded`, correlated by **`agent_name` string**
  rather than a stable id.
- **No stable `agent_id`.** String-name correlation is the root cause of the
  attribution gap (§5.2) — it is why Phase 1 leads with an id, not a UI.

### 3.3 Goal slot (EXISTING — and already exposed)

`agent_loop/goals.rs`: `GoalSlots = [Option<GoalSlot>; 5]`, where
`GoalSlot { text, source: Model | Operator, updated_at }`. The model writes via
`tools/goals.rs` (`set_goal` / `clear_goal`); slots render at message position **[1]**.

**The API already exists** (`session/protocol_goals.rs:39-95`):

- `session.set_goal(session_id, slot, text) -> {}` — operator writes use
  `source:"operator"`; last-write-wins.
- `session.clear_goal(session_id, slot) -> {}`
- `session.get_goals() -> { goals: [{ slot, text, source, updated_at }] }`

This is the **only** major mechanism in this spec that needs **no** API work — it needs
a place to live in the shell (§4.5).

---

## 4. The 10 design principles as functional requirements

The PDF states 10 principles as design intent. Restated here as requirements with
acceptance criteria, **corrected where they conflict with shipped behavior**.

### 4.1 The workstream is the unit, and it never ends

**Requirement.** The unit of work is a workstream, not a session. It has state
`active | idle | closed`, an inferred name, and is picked up rather than restarted.

**AC-1.1** A workstream survives daemon restart and reappears in the switcher with its
state and name intact. *(Blocked on §4.10 — this is the prerequisite, not a nicety.)*
**AC-1.2** State is **inferred** from activity, never a manual control.
**AC-1.3** Name is inferred from the first task; operator-editable.
**AC-1.4** "Never ends" is only true if context is managed indefinitely — see §4.5
(goal slots + budget) and §4.7 (thread at turn 500). A principle-1 claim with no
context-budget surface is **unfalsifiable to the operator**; the budget indicator is
what makes it visible.

### 4.2 The project is inferred, not chosen — **CORRECTED** {#SPEC-TCUI-03~draft}

**ID:** SPEC-TCUI-03~draft
**Status:** Draft

> **The proposal is wrong here, and it matters.** PDF §2 principle 2 says: "the moment
> work touches files **in a git repo** it binds to that project." That git-only rule
> **excludes non-git directories (#2728) and temp directories (#2747) — both of which
> we deliberately support and have already shipped.** `run_task/mod.rs:100-107` indexes
> the project **regardless of git**. Binding MUST NOT be gated on `.git`.

**Requirement — three binding states, not two:**

| State | Meaning | Indexing | Git affordances |
|---|---|---|---|
| **Projectless** | No directory bound. Chat/planning only. **Bob-mandated; MUST be supported.** | none | none |
| **Bound · non-git** | A plain directory or temp dir (#2728/#2747). Files are read/written and **indexed**. | **yes** | **none** — no branch, no PR, no dirty-tree |
| **Bound · git repo** | A git working tree. | yes | full (workflow lane, §4.9) |

**AC-2.1** A workstream MUST be creatable and usable with **no project bound**, and the
shell MUST render its empty state (7a) rather than an error.
**AC-2.2** Binding occurs when work touches files in **any** directory — git or not.
**AC-2.3** A non-git bound project MUST index and MUST surface readiness (§4.3), and
MUST **hide** (not fake, not error) git-only affordances.
**AC-2.4** The project name attaches to the workstream on bind and appears in the
header switcher and status bar (7b).

**API reconciliation (the two surfaces disagree today) — corrected:**

- `task.run`'s per-request params carry **no project field at all** —
  `TaskRunRequestParams` (`task/protocol.rs:61-91`) has none. The `project: PathBuf` at
  `task/protocol.rs:48` is a parameter to `register()`, wired **once at daemon-boot time**:
  `serve::build_router(project: PathBuf)` (`serve/mod.rs:91`) receives it from the process's
  own startup arguments and passes it once into
  `task::protocol::register(&mut router, sessions.clone(), project, agents_dir)`
  (`serve/mod.rs:97`), which closes over it for the life of the router. Projectless is
  therefore **not a request-DTO type swap** — it is a **daemon-bootstrap / binding-lifecycle**
  change: `build_router` (and everything it wires) MUST accept an absent binding, and every
  session the daemon creates for the life of that process inherits it.
- `session.create` takes `project: Option<String>` — an **untyped, per-request label**
  (`protocol.rs:157`), disconnected from the path baked into the daemon at boot.

These MUST converge on **one** typed project binding, resolved **once when the daemon
starts** and carried by every session it creates for that process's lifetime. A
per-request label that cannot be indexed and a boot-time path that cannot be omitted are
two halves of one missing object, on two different lifecycles. See §5.5.

#### 4.2.1 Directory inspection — the 7a picker is a daemon capability

**The 7a local-folder picker renders a directory tree. Under §2.1 that tree MUST come
from the daemon** (`project.list_dir`, §5.8) — not from Tauri's native fs, not from a
browser file-input, not from a native folder dialog (§2.1 C-4).

> **The daemon is already local.** This was previously treated as a platform question —
> *"a browser cannot walk `~/code/`, so does web lose project-open?"* (the old §7 Q2). The
> question dissolves once the constraint is applied: **directory inspection is an API
> concern, not a platform capability.** The daemon runs on the operator's machine and can
> already read the disk. Serve it from the daemon and **both targets get it identically**
> — which is C-3, and which is why 7a is no longer at risk of being a Tauri-only screen.

**Requirement.** The picker MUST render, per entry: the entry **name**, whether it is a
**directory**, and its **git-ness** — plus a **breadcrumb** of the current path
(7a: `~/code / acme-api /`). A git entry renders the `git` badge; a non-git entry renders
`—` (7a's `scratch` row).

**The leverage — one endpoint serves two jobs.** Git-ness is not picker decoration: it
**feeds §4.2's three-state binding model directly**. The same field that draws the badge
decides the binding:

| `list_dir` entry | Badge (7a) | Binding on select (§4.2) |
|---|---|---|
| `git: true` | `git` | **Bound · git repo** — full git affordances |
| `git: false`, `is_dir: true` | `—` | **Bound · non-git** — indexed, git affordances hidden |
| *(nothing selected)* | — | **Projectless** |

So the picker and the binding decision are **one API call**, not two subsystems that
must agree. This is also why the git-only rule the proposal states (and §4.2 corrects) is
not merely wrong in principle — it is contradicted by the very screen that would have to
implement it: 7a renders `scratch` as a **selectable** non-git entry.

**AC-2.5** The picker's entries, git-ness, and breadcrumb are **daemon-served**
(`project.list_dir`). No UI target enumerates the filesystem itself.
**AC-2.6** Selecting a `git: false` directory binds **Bound · non-git** and MUST NOT
error — non-git binding already ships (#2728/#2747, `run_task/mod.rs:100-107`).
**AC-2.7** The picker behaves **identically** in web and Tauri (§2.1 C-3).

### 4.3 Index readiness is first-class {#SPEC-TCUI-04~draft}

**ID:** SPEC-TCUI-04~draft
**Status:** Draft

**Requirement.** The operator MUST be able to tell whether search results are
trustworthy *before* trusting them. Readiness is a status-bar indicator plus a
first-class empty/degraded state.

> **Pure wiring — the value is already computed and then dropped on the floor.**
> `trusty_common::search_readiness::log_index_readiness` (called at
> `run_task/mod.rs:105`) computes real per-lane `IndexReadiness` and then only
> `tracing::info!/warn!`s it. **It is stderr-only.** An SPA cannot read stderr.

**AC-3.1** Per-lane readiness (semantic / text / graph) is retrievable via RPC and
pushed as an event on change.
**AC-3.2** The status bar renders readiness; a degraded/empty index is a **designed
state** (PDF §7: "empty states are first-class … the same shell, not error screens").
**AC-3.3** A search returning nothing **because the index is cold** MUST be
distinguishable from one returning nothing **because there are no hits**. Conflating
these two is the failure this requirement exists to prevent.

### 4.4 Consistent-but-dynamic chrome

**Requirement.** Header, service nav, and status line are always present, rendering an
*empty* or *hydrated* state. Cold start is the empty state of the same shell.

**AC-4.1** No screen replaces the shell. Every state in §4.2 is the same DOM.
**AC-4.2** Tabs render **locked** (not hidden) when projectless — the operator sees what
binding would unlock.

### 4.5 Goal slots, todos, and the context budget — **THE MISSING SURFACE** {#SPEC-TCUI-05~draft}

**ID:** SPEC-TCUI-05~draft
**Status:** Draft

> **Gap.** Goal slots, todos, and the working-context budget appear in **no mockup** —
> yet they are the mechanism that makes principle 1 ("it never ends") *true*, and the
> goal-slot API is **already shipped** (§3.3). The proposal designed the infinite
> thread and omitted the machinery that makes it infinite.

**Requirement — goal slots.** The shell MUST render the 5 goal slots as **workstream
context** (the service-nav scope, per principle 3 — goals are per-workstream, not
per-app).

**AC-5.1** Exactly **5** slots render, including empty ones. The fixed cardinality is
the point: it is a **budget**, and an invisible budget is not a budget.
**AC-5.2** Each slot shows its `source` — **`Model` vs `Operator` must be visually
distinct**. The operator has to know whether the agent set that goal or they did.
The visual system already carries this: amber `--accent` is "inferred" (PDF §3).
**AC-5.3** Slots are **operator-editable** in place → `session.set_goal` /
`session.clear_goal`. Last-write-wins; a concurrent model write MAY silently overwrite
an operator edit (§7, Q5).
**AC-5.4** `updated_at` renders as relative time.
**AC-5.5 (no API work required).** `session.get_goals` + set/clear already exist
(`session/protocol_goals.rs:39-95`). **Goal slots are pure UI wiring.**

**Requirement — todos.** 10b already renders per-agent todo checklists (`Rotate refresh
token in createSession` ✓, `Revoke server-side token on logout` ☐). These MUST be
per-agent, not global, and MUST be readable from the roster (§5.4).

**Requirement — working-context indicator.** The shell MUST surface the
**≥60% working-context floor / ≤40% overhead cap** (`cadence.rs::enforce_budget` vs
`overhead_cap_tokens = context_window * 40/100`).

> **The ratio is computed and then discarded.** `cadence::maybe_cadence_compress`
> returns `CadenceOutcome { fired, rounds, within_budget }` (`cadence.rs:218`) whose own
> doc comment says it exists **"for observability/tests"** — and observability never
> consumes it. `AgentLoop::maybe_cadence_compress` (`agent_loop/mod.rs:739`) calls it at
> **`mod.rs:747` and drops the return value on the floor**; the turn-boundary call site
> at `mod.rs:470` invokes that `()`-returning wrapper. Nothing is logged, evented, or
> exposed. **Un-discarding one return value is the whole task.**

**AC-5.6** The status bar renders the working-context ratio, sourced from a real
`CadenceOutcome`, not an estimate recomputed in the client.
**AC-5.7** `within_budget == false` (the documented floor: the active zone alone exceeds
the cap after full compaction) MUST be visible — red `--gap` per the token map.
**AC-5.8** A compaction firing (`fired == true`) MUST be observable in the thread (§4.7).
**AC-5.9 — cadence is PM-only today.** `AgentLoopConfig.cadence` defaults `None`; only
`task/executor.rs:327` sets `Some`. The indicator MUST render "not applicable" for
non-PM agents rather than implying an unmanaged context.

### 4.6 Services are first-class — and 8a/8b is a **lifecycle, not an A/B** {#SPEC-TCUI-06~draft}

**ID:** SPEC-TCUI-06~draft
**Status:** Draft

> **8a vs 8b is a false A/B.** They are presented as competing layouts. They are not.
> Read the artifacts: **8a's cards say `live`** and its rail says `PM searching · 1
> recall`; **8b's cards say `done · 240ms`** and its rail says `answered · 2 recalls
> used`. Those are the **same monitor at two points in time**. Choosing between them
> is a category error — you need both, in order.

**Requirement — the monitor lifecycle (normative):**

1. **Live** → the monitor renders in the **docked right rail** (8a) while the service
   is running. Streaming, ephemeral, attention-seeking.
2. **Settled** → on completion it **collapses into a compact inline card in the thread**
   (8b) as the **durable record** of what drove that turn. Permanent, scannable,
   scrolls away with its turn.

This is the only reading consistent with the core bet: the docked rail answers *"what
is happening now?"*; the inline card answers *"what drove this change?"* — and the
second question is the product.

**9a/9b are rungs on the same ladder, not rivals.** PDF §2 principle 5 already states a
**progressive-disclosure ladder**: `live dot → mini activity dropdown → pinnable monitor
column`. 9a **is** rung 2; 9b **is** rung 3. The proposal states the ladder in prose and
then presents its rungs as options.

**AC-6.1** A running service renders a live dot in its nav tab (rung 1).
**AC-6.2** Clicking the dot opens the mini activity dropdown (rung 2 = 9a).
**AC-6.3** A live monitor docks to the right rail (8a) and, on settle, MUST collapse
into an inline thread card (8b) preserving lane, query, hit count, and latency.
**AC-6.4** The inline card is **durable** — it survives scrollback and is part of the
turn's permanent record.
**AC-6.5** Pinning promotes a monitor to a column (rung 3 = 9b), subject to §4.6.1.

#### 4.6.1 Monitor columns (9b) — **demote to a transient trace mode** (RECOMMENDATION)

**9b does not survive its own screenshot.** At 3 pinned monitors the thread is squeezed
to roughly **25%** of the pane and the PM's answer visibly degrades to a truncated
*"Short version: logout clears the cookie but never revokes the rotated token."* Compare
8b, where the same answer is a full paragraph with a follow-up and two action buttons
(`Fan out Auth + Test` / `Show the diff first`). **The layout ate the product.**

The CSS confirms this is structural, not a tuning issue: `.svcol { flex:1 }` with
`.svcol.chat { flex:1.25 }` (PDF §6) gives the thread a **1.25/4.25 ≈ 29%** share at
three monitors. `flex:1.25` is not a fix; it is a rounding error against the thread's
own content.

**Recommendation (normative):** monitor columns are a **transient trace mode** —
deliberately entered to audit a fan-out, deliberately exited. **Not a daily driver
layout, and never the default.**

**Justification.** (a) The thread is where the product's answer lives; a layout whose
own mockup shows the answer degrading is disqualified as a default. (b) Principle 5's
ladder makes columns rung **3** — the *deepest* rung, i.e. the exceptional case, not
the resting state. (c) The 8b inline card **already delivers the trace** in the thread
at full width, which is exactly what pinning was reaching for — columns are largely
redundant once the lifecycle (§4.6) exists. (d) Nothing is lost: the operator opts in
when auditing and drops back out.

**AC-6.6** Monitor columns MUST NOT be the default layout.
**AC-6.7** Entering column mode is explicit; exiting restores the thread to full width.
**AC-6.8** The navigator rail collapses to make room (PDF §6: `.wsrail.collapsed`).

### 4.7 "Search" is two different things — keep them apart

**Requirement.** Two distinct surfaces that must never merge:

- **Literal find** — jump to a file/symbol/memory. Lives in the **Project page as a ⌘K
  box** (10a). Operator-driven.
- **The Search tab** — the **audit trail of agent search operations**
  (semantic/text/graph), each attributed to the requesting agent (10d). **Provenance,
  not a search field.**

10d makes this explicit in the UI itself: *"'Search' here isn't a box you type in… This
tab is the audit trail of the searches your agents performed."* That a screen needs a
banner to explain what it is not is a signal worth heeding — but the split is right, and
the banner is the cheapest way to hold it.

**AC-7.1** The Search tab has **no input field**.
**AC-7.2** Every row shows lane badge, query, hit count, latency, **requesting agent**,
and age (10d). This requires §5.2 + §5.3.
**AC-7.3** ⌘K find is scoped to the bound project and is unavailable when projectless.
**AC-7.4** ⌘K find is a **daemon query** (§2.1 C-1). It MUST NOT be backed by a
client-side file list or a shadow index — "jump to a file/symbol/memory" is a question
about the indexed project, which is daemon-owned state. The UI sends the term and renders
the answer.

### 4.8 Attribution everywhere

**Requirement.** Memory entries show **who requested** them; search operations show the
**requesting agent**; changed lines carry an **agent tag**; recalls are **scored** and
marked **injected vs held back**.

This is the core bet made literal, and it is **the single largest API gap** (§5.2–§5.3).

**AC-8.1** Every search op, recall, and tool call carries a stable `agent_id`.
**AC-8.2** Recalls render their score and their **injected vs held-back** status (8a/8b
render `92% match · → injected` and `41% match · held back`).
**AC-8.3** Memory entries render `requested by <Agent>`, workstream, and recall count
(10c).
**AC-8.4** Changed lines carry an agent tag in the gutter, **distinct from git status**
(PDF §7). *(Depends on AC-8.1; not Phase 1 — §6.3.)*

### 4.9 Workflow guards delivery — **new subsystem, deferred**

**Requirement.** A per-workstream `spec → epic → issue → branch → PR → review → deploy`
pipeline, newest first, side-scrolling, explicitly flagging **missing stages** and
surfacing **PRs awaiting merge, unpushed commits, uncommitted files** (10e), so a
fan-out cannot quietly skip review or deploy.

> **Nothing to build on.** There is **no branch, PR, dirty-tree, or unpushed concept
> anywhere in trusty-code**. `Phase*` events track *internal harness phases*, not
> delivery stages. `verify_gate.rs` gates `finish_task` on "did you run the named test
> command" — that is a test gate, not a delivery gate. This is a **new subsystem**, and
> it is git-only (undefined for the non-git binding state, §4.2).

**AC-9.1** Lanes render per workstream, newest first, side-scrolling independently.
**AC-9.2** Missing stages are explicit (10e renders `— / no epic` in red `--gap`).
**AC-9.3** A "needs attention" banner aggregates open PRs, unpushed commits, dirty files.
**AC-9.4** The Workflow tab is **hidden** for non-git and projectless workstreams (§4.2).

### 4.10 Back-nav preserves the trail

**Requirement.** Any deep-link (artifact → viewer, event dot → pane) keeps a breadcrumb
back to where you clicked, so exploring provenance never strands you.

11b shows the shape: `← Back | Agents · Auth Agent / session.ts` with `opened from agent
artifact`.

**AC-10.1** Every deep-link pushes a breadcrumb naming its origin.
**AC-10.2** Back returns to the exact prior scroll position and tab.
**AC-10.3** Agent file artifacts link into the **activity viewer**, never the IDE tree
(PDF §7, principle 8) — from an agent you care about the **change**, not the tree.

---

## 4A. The infinite thread at turn 500 {#SPEC-TCUI-07~draft}

**ID:** SPEC-TCUI-07~draft
**Status:** Draft

> **Gap.** Every mockup shows a thread with **2–3 turns**. The workstream is infinite.
> Nothing in the proposal says what turn 500 looks like — and the three mechanisms that
> make turn 500 possible (compaction, the memory dual-write, the SSE ring buffer) each
> leave a visible seam the operator will hit.

**Requirement — virtualization.** The thread MUST virtualize; only the visible window
renders. Turn count MUST NOT bound client memory or paint cost.

**Requirement — pagination.** History MUST page **backwards on demand** from the durable
record, not from the event ring buffer.

> **`GET /sessions/{id}/events` replays a ring buffer, then streams** (`serve/http.rs:98`).
> A **ring buffer is not history** — it is bounded and evicts. At turn 500 the early
> turns are **gone from the ring but present in trusty-memory** (the `TurnMemorySink`
> dual-write, `session/memory_sink.rs`, on by default via `task/executor.rs:270,380`).
> The thread therefore has **two backing stores with different retention**, and the
> client MUST NOT confuse them: the ring is *live tail*, trusty-memory is *history*.

**AC-11.1** The thread virtualizes; turn 500 paints in the same budget as turn 5.
**AC-11.2** Scrolling back past the ring buffer pages from the trusty-memory durable
record. The seam MUST NOT surface as a gap, an error, or a silent truncation.
**AC-11.3** The SSE ring replay MUST be used for the **live tail only**, never presented
as complete history.

> **§2.1 names the tempting shortcut here.** trusty-memory is a *separate service* with
> its own reachable surface, so "page history straight from trusty-memory" looks like a
> free win. **It is a C-2 violation:** it makes the UI a client of two backends, teaches it
> which store answers which turn range, and puts the ring-vs-durable retention seam —
> genuine domain logic (AC-11.6) — inside the client. **The client talks to the tcode
> daemon and nothing else**; tcode owns the dual-store read and hands back one coherent
> history. The two backing stores are an implementation fact of the daemon, not a fact the
> UI is allowed to know.

**AC-11.8** History paging is served by a **tcode daemon** method. No UI target queries
trusty-memory (or any other service) directly — one daemon, one client (§2.1 C-2).

**Requirement — the compaction boundary is a visible, honest object.**

When cadence compression fires (N=8, `agent_loop/cadence.rs::maybe_cadence_compress`),
turns above the active zone are **summarized and the originals leave the model's
context**. The operator's mental model breaks if the thread silently rewrites itself.

**AC-11.4** A compaction boundary renders as an **explicit horizontal marker** in the
thread — e.g. *"12 turns compacted · ~8.4k tokens reclaimed"* — at the point it fired.
**AC-11.5** The marker is **expandable**: the original turns are still in trusty-memory
and MUST be retrievable on click. **Compaction removes turns from the model's context,
not from the record** — the UI MUST make that distinction legible, because it is the
entire reason the dual-write exists.
**AC-11.6** The marker distinguishes **compacted** (summarized, recoverable) from
**evicted-from-ring** (live tail gone, still in memory). Same visual seam, different
cause.
**AC-11.7** Cadence is PM-only (§4.5, AC-5.9); non-PM threads show no boundaries because
none fire — not because they are hidden.

---

## 4B. Workstream durability — a prerequisite, not an endpoint {#SPEC-TCUI-08~draft}

**ID:** SPEC-TCUI-08~draft
**Status:** Draft

> **The collision.** "The workstream never ends" (principle 1) sits directly on top of a
> `SessionRegistry` that is a `HashMap` behind a `Mutex`, whose module doc states
> persistence is **"(Phase 2+, out of scope)"** (`session/registry.rs:1-12`). **A daemon
> restart loses the session list.** trusty-memory retains the *turn history* via the
> dual-write — so the paradox today is that the **conversation outlives the workstream
> that contained it**.

**A workstream requires:**

1. **Restart-surviving** — id, name, state, and project binding persist across daemon
   restarts and are restored on boot.
2. **Project-scoped** — N workstreams per project; the switcher groups by project (7a
   renders exactly this: `acme-api` with 3 workstreams, `data-pipeline` with *"no active
   workstreams"*).
3. **Listable** — enumerable without replaying an event stream.
4. **Resumable** — picked up, not restarted (principle 1).

**AC-12.1** A workstream MUST be recoverable after `tcode serve` restarts.
**AC-12.2** The switcher lists workstreams grouped by project, with state and inferred
name, from a **snapshot RPC** — not by folding events.
**AC-12.3** Turn history reattaches to its workstream on resume.

> **This is flagged as a PREREQUISITE DOMAIN OBJECT, not an endpoint.** The temptation
> is to "add `workstream.list`". That is backwards: there is no workstream to list.
> This is a domain + storage change — deciding what a workstream *is*, where it lives,
> and how it is keyed — with an RPC as its *consequence*. **It is the single largest
> item in this spec and the reason it is not in Phase 1** (§6.3). Sequencing an
> event-schema change (Phase 1) ahead of it is deliberate: provenance ships value on
> the existing non-durable registry; durability does not block it.

---

## 5. API surface — THE CORE

**Per the binding layer-priority rule (vision spec Axiom 3: API → CLI → TUI → Web),
every deterministic feature lands in the HTTP API BEFORE any UI surfaces it. This
section is therefore the normative core of the spec; the pixels are downstream.**

**And per §2.1, "before" understates it: the API is not merely first, it is the _only_
place functionality may live.** Every MISSING row in §5.1 is therefore a hard blocker on
its screen — never an invitation for the client to cover the gap itself. A UI need with no
API is an unbuilt feature, not a UI problem.

### 5.0 Today's surface (verified at `origin/main`)

**The API is 3 HTTP routes** (`serve/http.rs:96-98`):

| Route | Purpose |
|---|---|
| `POST /rpc` | All JSON-RPC (`session.*`, `task.*`) |
| `GET /health` | Health |
| `GET /sessions/{id}/events` | SSE — replays the ring buffer, then streams |

**There is no REST resource surface.** The event taxonomy is `crate::events::Event`
(`events.rs:75`, ~30 variants). Every UI need below routes through either a new
JSON-RPC method or a new/extended `Event` variant.

### 5.1 Summary — UI need → API delta

| # | UI need (screen) | Required API | Today | Delta |
|---|---|---|---|---|
| 1 | Per-tool agent attribution (10a gutter, 10d, 11a) | `agent_id` on `ToolStarted/ToolFinished/ToolError` | **MISSING** | Add `agent_id` + call-context to `ToolEventSink` |
| 2 | Search audit trail (10d, 8a/8b) | `Event::SearchPerformed{lane,query,hit_count,latency_ms,agent_id}` | **MISSING** | New structured event |
| 3 | Memory recall w/ score + injected (8a/8b, 10c) | `Event::MemoryRecalled{query,results:[{score,injected}],agent_id}` | **MISSING** | New structured event |
| 4 | Index readiness (status bar, 7a) | `IndexReadiness` as event + RPC | **MISSING** (stderr-only) | Surface computed value |
| 5 | Working-context budget (status bar) | `Event::ContextBudget{working_pct,overhead_pct,within_budget,fired}` | **MISSING** (discarded) | Stop dropping `CadenceOutcome` |
| 6 | Goal slots (§4.5) | `session.get_goals` / `set_goal` / `clear_goal` | **EXISTS** | **none — UI wiring only** |
| 7 | Projectless + binding (7a, §4.2) | `task.run` `project` → optional; unify with `session.create` | **PARTIAL** | Type change + reconciliation |
| 8 | Agent roster (10b, 11a) | `session.get_agents` snapshot + `model` field | **EXISTS** | `session.get_agents` ships, backed by an always-retained per-session map (not a ring-buffer fold) so it cannot lose a still-running agent to eviction (closes #2962); `model` field remains deferred — see §5.4 |
| 9 | Workstream list/resume (7a/7b switcher) | Workstream domain object + persistence + `workstream.list` | **MISSING** | Domain + storage (§4B) |
| 10 | Workflow lanes (10e) | Branch/PR/dirty-tree subsystem | **MISSING** | New subsystem, nothing to build on |
| 11 | Clone-from-URL (7a) | `project.clone_from_url` | **MISSING** | Not present anywhere. Daemon capability when it lands (§5.8) |
| 12 | Streaming cost (header `$0.31`) | Per-turn cost event | **PARTIAL** | Tokens stream; cost only aggregates at run-end. **Must be a daemon event** — §2.1 forbids client-side pricing (§7 Q6) |
| 13 | Local-folder picker + binding (7a, §4.2.1) | `project.list_dir{path}` → entries `{name,path,display_path,is_dir,git}` | **MISSING** | New RPC on the existing `/rpc` route (§5.8). **Phase 1** |

### 5.2 Tool attribution — one schema change unlocks the rest

**Today.** `ToolEventSink::tool_started(call_id, tool, args_preview)` and
`tool_finished(call_id, tool, success, result_preview)` (`agent_loop/sink.rs:35,40`)
have **no agent parameter**. The agent that ran a tool is structurally unrecoverable
downstream — it was never passed in.

**Required.**

```rust
async fn tool_started(&self, call_id: &str, tool: &str, args_preview: &str, agent_id: &AgentId);
async fn tool_finished(&self, call_id: &str, tool: &str, success: bool, result_preview: &str, agent_id: &AgentId);
```

with `agent_id` propagated onto `Event::ToolStarted / ToolFinished / ToolError`
(`events.rs:131-146`).

> **This is the keystone.** Principle 7 ("attribution everywhere"), the 10a gutter tags,
> the 10d requesting-agent column, and 11a's artifact links are **all** downstream of
> this one field. It is also a **prerequisite for #2 and #3** — a search event without
> an `agent_id` cannot populate 10d. Ship it first (§6.2).

**AC-13.1** A stable `agent_id` (not an `agent_name` string) identifies every tool call.
**AC-13.2** Name-based correlation is **removed** as a correlation mechanism, not
supplemented. Two agents may share a name; ids are the fix.

### 5.3 Structured search + recall events

**Today — search.** `tools/trusty_search.rs` routes `search_code` to **real lanes**
(semantic/text/graph). But the events emitted are generic `ToolStarted/ToolFinished`
carrying `args_preview` / `result_preview` — **truncated display strings**
(`events.rs:131-146`). The lane, the hit count, and the latency are **destroyed at the
event boundary**. 10d cannot be built by parsing a truncated preview string, and should
not be attempted.

```rust
Event::SearchPerformed {
    agent_id: AgentId,
    lane: SearchLane,      // Semantic | Text | Graph
    query: String,
    hit_count: usize,
    latency_ms: u64,
    hits: Vec<SearchHit>,  // { path, score } — powers 8a/8b relevance bars
}
```

**Today — recall.** Scores exist **inside** `tools/recall_session.rs` but never escape
the rendered text. **There is no `MemoryRecalled` event at all.** The injected-vs-held
distinction — which 8a, 8b, 9b, and 10c all render, and which is the most literal
expression of the core bet in the entire proposal — **exists nowhere in the API.**

```rust
Event::MemoryRecalled {
    agent_id: AgentId,
    query: String,
    vectors_queried: usize,
    results: Vec<RecallResult>, // { text, score, injected: bool, run_id }
}
```

**AC-14.1** Lane, query, hit count, and latency are **structured fields**, never parsed
from a preview string.
**AC-14.2** Every recall result carries `score` and `injected: bool`.
**AC-14.3** `injected` reflects **what actually entered the model's context** — not what
was returned by the query. A held-back recall is the operator's evidence that the
harness *chose*; conflating the two silently voids the core bet.

### 5.4 Agent roster (EXISTS — endpoint shipped, eviction-safe; `model` still deferred)

**Shipped (closes #2962).** `session.get_agents` now exists, backed by an
ALWAYS-RETAINED per-session agent map (`SessionEntry::agents`,
`registry.rs`) — not a fold over the capacity-bounded ring buffer. A first
cut folded `SessionRegistry::replay`'s ring-buffer snapshot on every call;
code-critic flagged that as a HIGH, because the ring evicts oldest-first
(`ring_capacity`, default 1000) and a long-running agent's `ToolStarted`
could age out of it before any later attributed event for that same
`agent_id` landed — making the agent vanish from the roster entirely,
indistinguishable from "never spawned" while it may still genuinely be
running. That was the exact silent-data-loss shape this section originally
warned about; moving the fold server-side had fixed WHERE it ran but not
WHAT it could lose. The shipped design closes that: `SessionRegistry::record`
— the SAME critical section that pushes onto the ring for every
`agent`/`agent_id`-carrying event (`ToolStarted`/`ToolFinished`/`ToolError`/
`SearchPerformed`/`MemoryRecalled`, #2898) — also updates the per-session
map, which is evicted only when the session itself goes away, never by ring
capacity. `AgentSpawned/AgentStarted/AgentDone/AgentFailed/PmDelegating`
still exist as unused-in-production event shapes (`events.rs:186-211,312-316`)
— see `session::registry::agents`'s module docs for why they are not
sources. `model` remains unpopulated: it lives only on
`LlmRequested/LlmResponded`, correlated by `agent_name` **string**, which
AC-13.2 retires as a correlation key — so folding it onto `agent_id` would
reintroduce the exact same-name collision bug #2898 closed.

```rust
session.get_agents() -> { agents: [{ agent_id, name, model, state, task, todos, files_changed }] }
```

**AC-15.1** `model` is carried on agent lifecycle events, not correlated by name.
**NOT YET MET** — tracked as the remaining gap above, not closed by #2962.
**AC-15.2** A late-joining client can render the roster. **MET as of #2962** —
`session.get_agents` reads the always-retained per-session map, so a
late-attaching client gets one RPC call, with no ring-eviction risk, instead
of replaying and folding the SSE stream itself.

> **§2.1 tension — GENUINELY RESOLVED by #2962: both the C-1 thin-client
> concern and the ring-eviction concern.** This section previously recorded
> event-folding as a time-boxed Phase-1 loan on TWO counts: (1) "who is
> running?" was daemon-owned state a UI client had to reconstruct by folding
> the SSE replay — the shape C-1 forbids; (2) even a daemon-side fold over
> the bounded ring would silently lose an agent once its `ToolStarted` aged
> out. `session.get_agents` closes both: the daemon owns the computation
> (no client-side derivation), and the state it reads
> (`SessionEntry::agents`) is retained for the life of the session
> independent of ring capacity, so it can never silently lose a still-running
> agent to eviction. The `model` gap (AC-15.1) is a separate, still-open
> debt (see above), not a re-opening of either concern this callout
> originally raised.

### 5.5 Project binding (PARTIAL — the two surfaces must converge)

**Corrected model** (§4.2) — this is a **daemon-bootstrap / binding-lifecycle** change,
not a per-request DTO type swap: `task.run` never had a project field on its request
params, so there is no request type to widen. The change is to what the daemon is
willing to *start* without, and to what every session it creates then inherits.

```rust
// task/protocol.rs:48 — register()'s parameter, bound ONCE at daemon-boot time via
// serve/mod.rs:91 build_router(project: PathBuf) → serve/mod.rs:97
// task::protocol::register(&mut router, sessions, project, agents_dir).
// TaskRunRequestParams (task/protocol.rs:61-91) carries no project field to change.
project: PathBuf            →  binding: ProjectBinding   // register()'s / build_router()'s param

// protocol.rs:157 — untyped, PER-REQUEST label, disconnected from the boot-time path above
project: Option<String>     →  project: Option<PathBuf>  // resolved into ProjectBinding per call

enum ProjectBinding { None, Directory(PathBuf), GitRepo(PathBuf) }  // §4.2's three states
```

**AC-16.1** The daemon itself MUST be startable with **no project bound** — `build_router`
and `register` MUST accept an absent binding — since `task.run`'s own request params never
carried one to begin with; a session created against such a daemon is projectless.
**AC-16.2** `session.create`'s per-request binding and the daemon's boot-time binding share
**one** typed binding. Today one is a boot-time path that cannot be omitted and the other
is a per-request label that cannot be indexed; that is one object split in half across two
surfaces on two different lifecycles (process-lifetime vs. per-call).
**AC-16.3** The binding distinguishes non-git from git (§4.2) — non-git indexing already
ships (#2728/#2747, `run_task/mod.rs:100-107`).

### 5.6 Index readiness + context budget (pure wiring)

```rust
session.get_readiness() -> { lanes: [{ lane, state, doc_count, last_indexed_at }] }
Event::IndexReadinessChanged { lanes: [...] }
Event::ContextBudget { working_pct, overhead_pct, within_budget, fired, rounds }
```

**AC-17.1** Readiness is served from the **same** `IndexReadiness` value
`log_index_readiness` already computes (`run_task/mod.rs:105`) — not recomputed.
**AC-17.2** `ContextBudget` is emitted from the **real** `CadenceOutcome` returned at
`agent_loop/mod.rs:747` and currently discarded. The wrapper
(`AgentLoop::maybe_cadence_compress`, `mod.rs:739`) must return it rather than `()`.

### 5.7 Workflow / delivery (MISSING — new subsystem)

No branch, PR, dirty-tree, or unpushed concept exists. `Phase*` events are internal
harness phases; `verify_gate.rs` gates `finish_task` on a test command only. 10e needs a
git+forge integration subsystem built from zero. **Deferred (§6.3).**

### 5.8 Directory inspection — `project.list_dir` (NEW, Phase 1)

Serves the 7a picker (§4.2.1) and, with the same response, the §4.2 binding decision.
Namespaced `project.*` to match the `session.*` / `task.*` convention already registered
in `session/protocol.rs:45-81`; it is a new method on the existing `POST /rpc` route —
**no new HTTP route** (§5.0's 3-route surface is unchanged).

```rust
// Request
project.list_dir({
    path: Option<String>,   // absolute path; None => daemon's default root(s)
})

// Response
{
    path: String,            // canonical absolute path listed, e.g. "/Users/bob/code"
    display_path: String,    // home-abbreviated, e.g. "~/code"  — powers the breadcrumb
    parent: Option<String>,  // absolute parent path; None at a root (disables "up")
    entries: [
        {
            name: String,          // "acme-api"
            path: String,          // "/Users/bob/code/acme-api"
            display_path: String,  // "~/code/acme-api"
            is_dir: bool,          // true => expandable (7a's ▸ arrow)
            git: bool,             // true => `git` badge; false => `—`
        }
    ]
}
```

**Field notes (normative):**

- **`git`** is "is this entry itself a git working tree" (a `.git` entry directly
  inside it) — **not** "is it inside one". 7a's `acme-api`/`marketing-site`/`data-pipeline`
  are `true`; `scratch` is `false`. This is the field §4.2.1's binding table keys on.
- **`display_path` is daemon-owned, not a client string-trim.** The UI **cannot** know
  `$HOME` — it has no environment (§2.1 C-1). Abbreviating `/Users/bob/code` → `~/code`
  is daemon knowledge, so the daemon serves it. Sorting/filtering entries is pure
  presentation and stays in the client; deriving `~` is not.
- **`path: None`** returns the daemon's default root(s) rather than erroring, so the
  picker has a defined cold-start entry point without the client inventing one.
- **`entries: []` simply means the directory is empty.** There is no result-state enum:
  unreadable or nonexistent paths are **ordinary JSON-RPC errors**, exactly as a local app
  surfaces a failed `ls`. See the guard note below for why nothing more elaborate is
  specified here.

**AC-20.1** `project.list_dir` returns entry name, path, `is_dir`, and `git` for every
entry — the minimum 7a renders.
**AC-20.2** `git` distinguishes a git working tree from a plain directory, and that same
value drives §4.2's binding state. One call, both jobs.
**AC-20.3** The breadcrumb renders from `display_path` / `parent`, not from client-side
`$HOME` inference.

> **No path-guard layer — deliberate, do not "fix" this** (owner directive, Bob).
> **tcode is a local app; the daemon runs as the operator with the operator's own
> entitlements.** A directory listing exposes nothing the operator cannot already `ls`.
> Decisively: **the daemon already exposes `task.run`, which executes arbitrary code as
> the operator.** `project.list_dir` is **strictly less powerful than what the API already
> permits** — bolting a denylist onto browse while `task.run` sits beside it is ceremony,
> not security: it does not move the threat boundary.
>
> **The #2747 / trusty-search `allow_sensitive_path` precedent does NOT transfer here.**
> That denylist guards **indexing** — file *contents* leaving the machine for an embedder
> — which is a categorically different act from *listing paths*. Do not cite it as
> precedent for this method.
>
> **macOS TCC:** the app inherits whatever entitlements it is granted, as any local app
> does. **No bespoke permission-state machine and no designed "permission needed" state**
> — those are not in this spec's empty/degraded-state inventory. (Index readiness, §4.3,
> **stays** in that inventory: it is a real degraded state and is Phase 1.)

**Clone-from-URL stays deferred (§6.3) — but it is a DAEMON capability when it lands.**
7a's second card ("GitHub URL → Clone & attach") is out of Phase 1 for the reason already
recorded: it exists nowhere, is purely additive, and local-folder binding already covers
the mandated projectless→bound path. That deferral is unchanged by §2.1. What §2.1 *does*
settle is **where it lands when it lands**: cloning a repo is filesystem-and-process work,
so it is `project.clone_from_url` on the daemon (§5.1 #11) — **never** a UI-side git
operation, and never Tauri-native. Deferring it therefore costs nothing architecturally.

---

## 6. Phased delivery

### 6.1 Sequencing principle

Per the layer-priority rule, phases are cut by **API delta**, not by screen. A phase
that ships pixels ahead of its events is not a phase; it is a mock.

### 6.2 **Phase 1 cut line** — the smallest surface that unlocks the differentiated core

**Phase 1 is six event/RPC changes and almost no UI.** It is chosen to unlock exactly
what makes this harness different from a linear stream — **provenance, the search audit
trail, memory injected-vs-held, readiness, and the context budget** — plus the one call
that makes the entry screen reachable at all. Items 1–5 are already computed in the
runtime and discarded at a boundary; that part of Phase 1 is mostly **un-discarding
values we already have**. Item 6 is the exception and is new code — small, and load-bearing
for 7a (§6.2.1).

| # | Item | Why in Phase 1 | Size |
|---|---|---|---|
| **1** | `agent_id` + call-context on `ToolEventSink` → `Event::ToolStarted/ToolFinished/ToolError` (§5.2) | **The keystone.** One schema change unlocks all attribution; #2 and #3 depend on it. | M |
| **2** | `Event::SearchPerformed { lane, query, hit_count, latency_ms, agent_id }` (§5.3) | The Search tab (10d) **is** the audit trail; lanes are already routed, only the event is missing. | S |
| **3** | `Event::MemoryRecalled { query, results:[{score, injected}], agent_id }` (§5.3) | Injected-vs-held is the most literal form of the core bet; scores already exist internally. | S |
| **4** | Surface `IndexReadiness` as event + RPC (§5.6) | Pure wiring — already computed, stderr-only. Without it, search results are untrustworthy by construction. | S |
| **5** | Stop discarding `CadenceOutcome` → emit working-context ratio (§5.6) | One return value. Makes "it never ends" falsifiable. | S |
| **6** | `project.list_dir` — daemon-served directory inspection (§5.8) | **7a's picker has no other legal source** (§2.1 C-4 bars Tauri-native fs). Ships with projectless (§6.2.1): that pair is what makes the entry screen reachable. Its `git` field also drives §4.2's binding, so it is not picker-only. | S |

**Phase 1 UI:** the status bar (readiness + budget), the 8b inline monitor card, the
10d Search tab, the **goal-slot panel** (§6.2.1), and the **7a local-folder picker**
(§4.2.1). These are the surfaces that require **no** new domain object — they render
Phase 1's events against today's non-durable registry.

#### 6.2.1 Projectless — **Bob mandated it; here is where it lands and why**

**Projectless is Phase 1.** Rationale, stated deliberately:

- It is a **daemon-bootstrap / binding-lifecycle change, not a request-DTO type swap**
  (§4.2, §5.5) — `task/protocol.rs:48`'s `project: PathBuf` is `register()`'s parameter,
  wired once at process startup via `serve::build_router` (`serve/mod.rs:91,97`), not a
  field on `task.run`'s own request params. Making it optional touches the daemon's
  startup path (CLI arg parsing → `build_router` → `register`) and how an absent binding
  then propagates to every session the process creates — plus reconciling
  `session.create`'s untyped per-request label (§5.5) onto that same typed binding. Still
  Phase-1-sized and still mechanical, but it is a boot-time binding-lifecycle concern, not
  a leaf-node signature edit — size and review it accordingly.
- It is **load-bearing for the shell**: principle 4 says cold start is the empty state
  of the same shell. **Today the empty state is not merely unstyled — it is
  unreachable**, because `task.run` cannot be called without a project. Screen 7a is
  literally unimplementable. Every other Phase-1 surface renders inside a shell whose
  entry state does not exist.
- **7a is blocked twice, and `project.list_dir` (§5.8) clears the second block.**
  Projectless makes the screen *reachable*; the picker makes it *actionable* — a 7a you
  can enter but whose folder list has no legal data source is still not a screen. Under
  §2.1 C-4 the Tauri-native shortcut that would have unblocked it is barred, so the two
  ship together or 7a ships in neither target.
- It **de-risks the binding model early**: the three states (§4.2) are the correction
  this spec makes to the proposal, and the cost of discovering that reconciliation is
  wrong is far lower now than after the workstream object is built on top of it.
- Deferring it would invert the layer-priority rule — the SPA's **first screen** would
  be blocked on an API change we chose not to make.

**Phase 1 explicitly does NOT include:** the workstream domain object, monitor columns,
the Workflow tab, the IDE half, or clone-from-URL.

### 6.3 Explicitly NOT Phase 1 (with reasons)

| Item | Why deferred |
|---|---|
| **Workstream persistence** (§4B) | Real **domain + storage** work — deciding what a workstream *is*, not adding an endpoint. The largest item in the spec. Phase 1 ships value on the existing registry without it. |
| **Per-line diff attribution UI** (10a gutter) | **Depends on §5.2** (`agent_id`). Ship the schema first; the gutter is a downstream consumer. |
| **Workflow / delivery pipeline** (10e) | **New subsystem, nothing to build on** — no branch/PR/dirty-tree concept exists anywhere (§5.7). Also git-only, so it needs §4.2 settled first. |
| **Agent roster `model` field** (10b) | **Shipped except `model`** — `session.get_agents` (§5.4) landed with #2962, backed by an always-retained per-session map (not a ring-buffer fold), closing BOTH the §2.1 client-side-fold tension AND the ring-eviction data-loss risk. `model` per `agent_id` is the one still-deferred sub-item; see §5.4. |
| **Clone-from-URL** (7a) | Does not exist anywhere; pure additive scope with no dependents. Local-folder binding (`project.list_dir`, §5.8) covers the mandated projectless→bound path. **When it lands it is `project.clone_from_url` on the daemon — never a UI-side git op** (§5.8). |
| **Monitor columns** (9b) | Demoted to transient trace mode (§4.6.1); the 8b inline card delivers the same trace at full width. |

### 6.4 Phase 2+

Workstream durability (§4B) → agent roster snapshot + `model` → thread virtualization &
compaction boundaries (§4A) → workflow subsystem (§4.9) → IDE half (pending §7 Q4) →
clone-from-URL → monitor columns as an opt-in trace mode.

---

## 7. Open questions

**Q1 — The missing wireframe doc.** The PDF's closing line pairs it with an
"**interactive wireframe doc (turns 1–11, options linked by id)**". **That document is
not in the repository** — only the PDF and `handoff/` (14 PNGs, 7a–11b). It very likely
holds the **rationale for the 8a/8b and 9a/9b options** this spec had to reconstruct
from pixels (§4.6). If it exists, it should be committed alongside the PDF and this
spec's §4.6/§4.6.1 conclusions re-checked against it. **Owner: design.**

**Q2 — Platform split: web vs Tauri — what actually differs? — RESOLVED.**
*Resolution (Bob): "Tauri can walk a local directory? But we should be able to provide an
API to inspect directories."*

**The fork dissolves; it was not decided.** The question assumed directory access was a
**platform capability**, so the two targets had to differ. It is an **API concern**: the
daemon is **already local** and already reads the disk. Serve directory inspection from
the daemon (`project.list_dir`, §5.8) and **both targets get it identically**.

What follows:

- **Nothing differs functionally.** Per §2.1 C-3, a capability in one target and not the
  other is a spec violation. Web and Tauri are shells over one daemon API and MUST be
  behaviorally identical; the difference is packaging, not capability.
- **No Phase-1 surface is invalidated.** The prior draft flagged that 7a's picker might
  not survive the web target. It survives in both — the picker is `project.list_dir`, and
  the same `git` field also drives §4.2's binding (§4.2.1).
- **Tauri-native fs/dialog is barred, not merely unnecessary** (§2.1 C-4). An earlier
  framing called a native picker "optional polish"; that was **wrong** under §2.1 — it
  would relocate functionality into the UI layer and create the very divergence C-3
  forbids. A native dialog is permitted only as a **cosmetic shell over daemon-provided
  data**.
- **The IDE half (10a)** is unaffected by platform and remains scoped by **Q4** — if it
  ships, its file reads are daemon-served like everything else. Q2 never gated it; Q4 does.
- **The daemon is assumed local.** tcode is a local app (§5.8): one instance per project
  (§1.3 non-goal 2), running as the operator with the operator's own entitlements. Remote
  daemons are out of scope for this spec.
- **Still open, and belongs to Q3, not here:** token storage (OS keychain vs browser
  storage) is an *auth* question. It is the one place a real web-vs-desktop difference may
  surface — and it surfaces as an **identity-model** question (Q3), not a capability split.

**Owner: Bob. → RESOLVED 2026-07-16.**

**Q3 — Auth / multi-user.** Wholly unaddressed by the proposal. 7a's header renders
"**OAuth login**" and 7a's GitHub-URL card says private repos "use your **connected
token**" — implying an identity model that does not exist. Single-operator is assumed
throughout this spec (§1.3) and stated as a non-goal, but if the SPA is served over a
network, "no auth" is a decision, not a default. **Owner: Bob.**

**Q4 — Is the IDE half (10a) in scope at all?** Principle 8 routes agent artifacts to
the **activity viewer, not the tree** — and PDF §7 restates it ("not the IDE"). 11a/11b
confirm: artifacts deep-link into the changed-files viewer with back-nav. So the IDE
tree + editor + ⌘K find (10a) serves only *operator-initiated browsing* — a large
surface (tree, editor, gutter tags, syntax highlighting) whose only unique contribution
is literal find (§4.7). **Recommendation: defer 10a entirely; ⌘K find can attach to the
activity viewer.** If deferred, principle 7's gutter attribution (AC-8.4) loses its
only home and should be re-scoped to the 10f/11b diff view — which already renders
`Auth Agent · edited 2m ago` and a **"WHAT DROVE THIS"** panel. Notably, **10f/11b
already deliver the core bet's payoff without the IDE.** **Owner: Bob.**

**Q5 — Goal-slot write conflicts.** `session.set_goal` is **last-write-wins** with no
version/CAS (`session/protocol_goals.rs`). A model write can silently clobber an
operator edit mid-turn. Acceptable, or does the operator source win until cleared?
**Owner: engineering.**

**Q6 — Streaming cost — NARROWED by §2.1.** The header renders live cost
(`$0.02 → $1.42` across screens), but dollar cost only aggregates at run-end
(`aggregate_usage_per_role` → `registry.set_run_outcome`). Tokens stream live
(`LlmRequested{prompt_tokens}`, `LlmResponded{completion_tokens, latency_ms}`).

> **§2.1 eliminates one of the two options.** This was posed as "compute cost client-side
> from streaming tokens, **or** add a streaming cost event?" **The first option is now a
> violation** — token→dollar conversion needs a per-model pricing table, which is business
> logic and daemon-owned state; putting it in the client is exactly C-1, and would drift
> per target the moment one shell updated its table (C-3). **A streaming cost event is
> therefore the only conforming answer.**

The remaining question is scope, not architecture: emit cost per `LlmResponded`, or a
periodic `Event::CostUpdated` aggregate? **Owner: engineering.**

**Q7 — Non-PM context budget.** Cadence is PM-only by construction
(`AgentLoopConfig.cadence` defaults `None`; only `task/executor.rs:327` sets `Some`).
Sub-agents run **unmanaged** contexts. Is that intended long-term, or does the budget
indicator (§4.5) need a per-agent story once fan-out is routine? **Owner: engineering.**

---

## 7A. Project-first entry flow {#SPEC-TCUI-10~draft}

**ID:** SPEC-TCUI-10~draft
**Status:** Draft

> **Epic #3174 feedback (Bob).** The create-session ceremony (screen 7a, the form) is the wrong entry point. Instead: **pick a project from recents/registered list → prompt box is live immediately → typing + Enter creates session and sends first task in one gesture**. Principle 4 (cold start is the empty state of the same shell) stands; the empty state is now "no project picked", not "create ceremony".

**Requirement — project selection drives immediate task creation:**

1. **Project picker** — a curated list of recent/registered projects (from `trusty-mpm GET /projects/discover`), not raw filesystem browse. Selection is fast and repeatable.
2. **Live prompt box on selection** — once a project is picked, a **single text input + Enter** is the entry point. No separate form step.
3. **One-shot create+prompt** — typing the prompt text and pressing Enter calls `POST /tasks` with the prompt as the first `task_run`, binding the project per-call (§5.5 / §7). The session is created (or resumed into an existing workstream) as a side effect; the prompt is the primary action.

**AC-21.1** The project picker shows recent projects (cached from prior selections) alongside registered projects from `trusty-mpm`.
**AC-21.2** Selecting a project populates the session shell with that project's context (§7B); the prompt box becomes the only input.
**AC-21.3** `POST /tasks` wired to accept per-call `project` binding (§5.5, issue #3178) so the prompt-box entry is possible at all.
**AC-21.4** Session creation is now inferred from the first task, not a separate step (issue #3177).

### 7A.1 Recents and project discovery

Feeds the picker. Two sources:

- **`trusty-mpm GET /projects/discover`** — registered projects (from `tm project register`)
- **Recents cache** — operator's prior selections in this SPA, persisted in browser/app storage (client-side only; no backend)

**AC-21.5** Recents are stored client-side and updated on every successful project binding.
**AC-21.6** The picker sorts by recency, with a fallback to alphabetic for ties.

---

## 7B. Service hydration on project selection {#SPEC-TCUI-11~draft}

**ID:** SPEC-TCUI-11~draft
**Status:** Draft

> **Gap.** §4.2 says binding occurs when work touches files. By that point the index may be cold, the palace nonexistent, or a daemon unreachable — and the operator has no warning until they ask a question. **Hydration on selection makes readiness visible before the first prompt.**

**Requirement — project-scoped status aggregation:**

The moment a project is picked, the shell fetches and renders the **full status** of that project's services (search, memory, analyze):

- **Search:** index exists? readiness lanes (semantic/text/graph), doc count, last-indexed-at.
- **Memory:** palace exists? word count, summary stats.
- **Analyze:** daemon reachable? health status.
- **Git (if bound repo):** branch, dirty/clean, ahead/behind (§7D).

All via a **single daemon aggregation call** that fans out to each service.

**AC-22.1** `POST /rpc` method `project.status{path}` returns aggregated health for all services bound to that project root.
**AC-22.2** The call resolves daemon addresses via `trusty_common::daemon_addr::resolve_daemon_base_url` — no hardcoded ports.
**AC-22.3** Response shape (not yet specified in this addendum; see issue #3181):
```rust
project.status { path } -> {
  project: { name, root_path, git_repo?: bool },
  search: { ready: bool, lanes: [{lane, state, doc_count}] },
  memory: { palace_id?: string, word_count: u32 },
  analyze: { reachable: bool },
  git?: { branch, dirty: bool, ahead: u32, behind: u32 }  // if git repo
}
```

**AC-22.4** A degraded service (search index cold, palace missing, daemon unreachable) renders as a **designed state in the status chrome** — not an error and not hidden. Status bar shows a visual indicator per service.

---

## 7C. Project context panel {#SPEC-TCUI-12~draft}

**ID:** SPEC-TCUI-12~draft
**Status:** Draft

> **Gap.** The workstream is context-first (principle 1), but the operator sees only the prompt and thread. **Project context** — what can be indexed, what instructions the harness reads, what stack is detected — must be discoverable in the shell chrome without leaving the thread.

**Requirement — a read-only project-info panel:**

1. **Inferred stack** — language/toolchain detected via trusty-mpm's marker-table logic (reuse `project_lang::LANGUAGE_ENGINEERS` + `stack_profile.rs`, issue #3182).
2. **Instruction summaries** — short prose summary of `CLAUDE.md` and `AGENTS.md` (if present), with an **edit link** (§7C.1).
3. **Service status** — read from §7B aggregation.

**AC-23.1** Stack is rendered as a set of tags (e.g. `Rust`, `Tokio`, `PostgreSQL`).
**AC-23.2** Instruction summaries are **daemon-generated** — one-off LLM summarize call (reuse `DispatchingLlmClient`, issue #3184), not client-side markdown parsing.
**AC-23.3** Summary length is capped at ~200 chars; a "show full" link opens the instruction file for editing.
**AC-23.4** Summaries are cached server-side (per project + file mtime) to avoid re-summarizing on every project pick.

### 7C.1 Instruction file editing — daemon-served route {#SPEC-TCUI-15~draft}

**ID:** SPEC-TCUI-15~draft
**Status:** Draft
**Owner directive (Bob).** Binding architectural constraint. Distinct from the deferred IDE (§1.3 non-goal 1).

> **The "edit link" cannot use:** (1) an external editor (no `open -a VSCode`; breaks §2.1 C-1); (2) Tauri-native fs (barred by §2.1 C-4); (3) a general fs-write API (barred by §2.1 C-2 — daemon is the sole source).

**Requirement — dedicated read+write instruction-file route:**

A **single, path-guarded REST/RPC surface** locked to **two filenames only** (`CLAUDE.md` and `AGENTS.md`, once #3183 lands) within the bound project root:

```rust
// READ
project.read_instruction { filename: "CLAUDE.md" | "AGENTS.md" }
  -> { content: String, mtime: u64 }

// WRITE
project.write_instruction { filename, content: String }
  -> { content: String, mtime: u64 }  // re-read after write
```

**Field notes (normative):**

- **Filename allowlist only** — no path parameters. `CLAUDE.md` resolves from root or `.claude/` (precedence per `project_context/mod.rs`); `AGENTS.md` same.
- **No traversal.** A rejected request that attempts `../../../etc/passwd` fails with a guard error, never a generic fs error (AC-24.3).
- **No arbitrary fs access.** This is NOT a general file-serve route; it MUST NOT be extended to read/write arbitrary project files.
- **Parity across targets** (§2.1 C-3) — the route exists on the daemon so both web and Tauri get the same behavior.

**AC-24.1** The route is available over both REST and JSON-RPC, wired into `POST /rpc` alongside the other project.* methods (§5.8, §5.0).
**AC-24.2** Write succeeds with a `conflict` error (HTTP 409 / JSON-RPC error code) if the file has been edited externally since the last read (compare mtime).
**AC-24.3** Path traversal attempts (detected by canonicalization or naive `..` checks) fail with **`guard: PathTraversalAttempted`** error code, distinct from a genuine not-found or permission error.
**AC-24.4** The GUI opens an **in-app editor** (not an external app) backed by this route — the UI is a thin client over the daemon (§2.1 C-2).

---

## 7D. Git status — branch, dirty, ahead/behind {#SPEC-TCUI-13~draft}

**ID:** SPEC-TCUI-13~draft
**Status:** Draft

> **Gap.** §4.2 binds git repos; §4.9 defers workflow lanes (the full PR/branch/deploy pipeline is Phase 2+). But operators need **immediate visibility** into whether the working tree is clean and where they stand relative to the remote — a simple, high-signal three-tuple that does not require the full subsystem.

**Requirement — lightweight git status:**

```rust
project.git_status { path } -> {
  branch: String,          // current branch name
  is_dirty: bool,          // git status --porcelain empty?
  ahead: u32,              // rev-list --left-right --count … | head -1
  behind: u32              // rev-list --left-right --count … | tail -1
}
```

**Per-field semantics:**

- **`branch`** — symbolic-ref HEAD, or detached commit hash if applicable.
- **`is_dirty`** — true if `git status --porcelain` is non-empty (working-tree modifications, staged, or untracked).
- **`ahead` / `behind`** — relative to tracking remote (if set); both 0 if no remote. Computed via `git rev-list --left-right --count HEAD...@{u}`.

**AC-25.1** The route errors (JSON-RPC / REST) if the path is not a git repo or git is unreachable.
**AC-25.2** Status renders in the project-context panel (§7C) and, if live-polling (§7D.1), updates while the session runs.
**AC-25.3** Distinct from §4.9's workflow lanes (PRs, branches, deploy stage); git_status is Phase 1, workflow is Phase 2+.

### 7D.1 Optional: live polling {#SPEC-TCUI-14~draft}

**ID:** SPEC-TCUI-14~draft
**Status:** Draft

> **Not normative for Phase 1, but worth spelling out.** If the harness is modifying files and the operator is watching the project context, they will want to see the dirty/ahead-behind state update in real time without re-fetching the whole project.status aggregate.

A lightweight **poll on interval** (5–10s) of `project.git_status` while the workstream is active. If this lands, the UI is responsible for debouncing the requests (no more than one per 5 seconds).

**AC-25.4** Live polling of `project.git_status` is optional; poll interval is a client-side preference (default 10s if implemented).

---

## 7E. Project creation {#SPEC-TCUI-16~draft}

**ID:** SPEC-TCUI-16~draft
**Status:** Draft

> **Gap.** The picker (§7A) offers registered projects, but operators also need to create new ones on the fly — "pick a directory, optionally init git, register it, and start coding". **Not clone-from-URL** (that is §5.1 #11, deferred to Phase 2+); just a **local directory + git init + recents cache update**.

**Requirement — create-dir + registration flow:**

1. **Browse and create** — the project picker includes a "New Project" entry (or button) that opens a **directory picker** (same daemon-served `project.list_dir` surface used by the 7a binding picker, §5.8) and allows the operator to **select or create a directory**.
2. **Optional git init** — prompt to `git init` if creating a new dir or if selected dir is not already a repo.
3. **Register + bind** — call `project.create_dir` to persist the binding (and optionally register it in trusty-mpm if that API exists); add to recents cache.

```rust
project.create_dir {
  path: String,       // absolute path to create or bind
  git_init?: bool,    // true => `git init` if directory is new or non-git
} -> { path: String, git: bool }
```

**AC-26.1** Creating a project calls `project.create_dir` which MUST handle **mkdir with traversal safety** — no `../`, no symlink escapes.
**AC-26.2** Git init is optional; a plain directory project is valid (§4.2 Bound · non-git).
**AC-26.3** On success, the new project is added to the recents cache and immediately selectable.
**AC-26.4** GUI affords "New Project" as a prominent entry in the project picker or as a separate button.

---

## 8. Visual system — Foundry design system {#SPEC-TCUI-17~draft}

**ID:** SPEC-TCUI-17~draft
**Status:** Draft

**Normative: Foundry (v1)** replaces the missing design handoff PDF references. See
[`docs/design/UI/design-system/`](../../design/UI/design-system/) (PR #3170, branch
`docs-foundry-design-system`):

- **Philosophy:** robot-themed, rust-colored. Machines talk mono; humans talk sans. Flat,
  honest, clean line borders (no drop shadows on resting surfaces).
- **Tokens:** `docs/design/UI/design-system/tokens.css` — single source of truth for
  colors, typography, spacing, radii. Both **light** (`:root`) and **dark** ("Night Shift",
  `[data-theme="dark"]` or `.dark` class) palettes ship in the same file.
- **Role mapping** (normative; palette is not):
  - **Rust (#B7410E light, #D97742 dark)** — primary action, highlighted datum, active nav (one per region, never body text or large backgrounds).
  - **Green (#3F6F2A)** — done, active, shipped.
  - **Blue (#3D6B8A)** — info, in-progress.
  - **Red (#C2331F)** — gaps, blocked, dirty (used in AC-5.7 context-budget overflow, AC-9.2 missing stages).
  - **Amber (via `--accent-soft`, `--surface-hover`)** — inferred state (AC-5.2 model-authored goals).
- **Typography:**
  - **Chakra Petch** — display/headings; hero numbers.
  - **IBM Plex Sans** — body prose; UI labels.
  - **IBM Plex Mono** — all system metadata (ids, paths, counts, status, table headers). This split is **load-bearing**: it separates *content* from *telemetry* visually — and in a context-first harness, telemetry **is** the product's evidence.
- **Shell skeleton** (unchanged from PDF) — one flex column: header → service nav → body (rail + active pane) → status line. **§8.1 nesting invariant (AC-18.1) remains binding.**

### 8.1 Dark theme ("Night Shift") and OS integration

**Requirement — automatic theme activation:**

The shell MUST follow the operator's OS setting (light/dark mode preference). On startup,
read `window.matchMedia('(prefers-color-scheme: dark)')` and set `<html data-theme="dark">`
or remove it accordingly. When the OS setting changes (via Settings or during session), the
shell MUST update in real-time (listen to the `matchmedia` event).

**AC-27.1** Dark theme activates via `[data-theme="dark"]` on the root `<html>` element;
light is the default.
**AC-27.2** On init, the shell reads OS preference via `window.matchMedia('(prefers-color-scheme: dark)')`.
**AC-27.3** The shell listens to OS theme changes and updates the `data-theme` attribute in real-time (no page reload).
**AC-27.4** The operator MAY override the OS setting with a manual toggle (stored in
browser/app settings); the override persists across sessions.
**AC-27.5** All components reference tokens only — hardcoded hex colors are a spec
violation and must fail review.

### 8.2 Component state quick-reference (Foundry §1 guardrails)

Foundry defines component states that normalize across the SPA. Normative constraints:

- **Buttons:** PRIMARY (solid rust) · SECONDARY (card bg) · TERTIARY (raised bg) · DANGER (soft-rust bg) · GHOST (transparent) · DISABLED (raised bg + muted text).
- **Badges:** rectangular stamps, 4px radius, mono uppercase 10px, one status color per region.
- **Tables:** no zebra stripes; row hover uses `--trusty-surface-hover`.
- **Toasts:** dark chassis panels, bottom-right stack, 3px status left edge (color for success/warning/danger), mono title + sans body. Success auto-dismisses at 5s; errors persist.
- **Modals:** card with raised header strip, Chakra Petch title, actions right-aligned in footer.
- **Empty states:** muted idle-robot mark, one mono label, one line of sans guidance, one primary action.

**AC-27.6** New components added to the shell MUST follow Foundry precedent or extend
Foundry's open rules (the "extend without breaking" guardrails in `docs/design/UI/design-system/README.md`).

### 8.1 The nesting note — keep it, and assert it

> **`.statusbar` MUST be a sibling of `.body`** (a direct child of `.app`), **never
> inside `.body`** — otherwise it steals the rail's row width and squishes the pane.
> The PDF's own words: *"This bit us twice in the wireframe; assert it in a test."*

**AC-18.1** A DOM/structural test asserts `.statusbar` is not a descendant of `.body`.
This is carried forward verbatim because it is the one piece of the visual system with a
**testable invariant** — and it already regressed twice.

---

## 9. Follow-ups

| ID | Item | Depends on |
|---|---|---|
| **F1** | Add the DOC-39 catalog row to `docs/specs/README.md` and refresh its stale "next free `DOC-N`" note. **Update catalog status** after addendum merge: spec now spans `SPEC-TCUI-01~draft` … `-17~draft`; next free is **DOC-42** (per 2026-07-16 scan). | this spec |
| **F2** | Commit the missing interactive wireframe doc (§7 Q1); re-check §4.6/§4.6.1 against it. | design |
| **F3** | File the Phase-1 issues (§6.2) under the UI epic, sequenced with §5.2 first. | this spec |
| **F4** | Resolve ~~Q2~~/Q3/Q4 (~~platform~~, auth, IDE scope) before Phase 2 planning. **Q2 is RESOLVED** (§7); Q3/Q4 remain. | Bob |
| **F6** | File the `project.list_dir` Phase-1 issue (§5.8), paired with projectless (§6.2.1) — 7a needs both to be a screen. | this spec |
| **F7** | Q6's client-side-cost option is now a §2.1 violation; scope the streaming cost **event** instead. | engineering |
| **F5** | Note for DOC-38 §10 F3 (DOC-28 renumber): DOC-39 is now **claimed by this spec**; that follow-up must re-scan and take **DOC-42**. | DOC-38 |
| **F8** | File epic #3174 sub-issues (issues #3177–#3188) under the UI epic; sequence them per the dependency map in the epic description. Per-issue specs (especially #3181 and #3188) will refine the `project.status` shape and `project.{read,write}_instruction` guards. | epic #3174 |
| **F9** | Merge `docs-foundry-design-system` branch (PR #3170) into main; verify the `docs/design/UI/design-system/` path exists and both token files are present. Update any UI crates referencing `tokens.css` to use the new Foundry version. | PR #3170 |

## Changelog

- **2026-07-19** — **DOC-39 addendum pass** (epic #3174, PR #3170 Foundry design system). Adds **§7A–§7E** (`SPEC-TCUI-10~draft` … `-16~draft`) covering the **project-first session flow** (pick project → live prompt → one-shot create+prompt; issue #3177–#3178), **service hydration on selection** (search + memory + analyze status aggregation; issue #3181), **project-context panel** with instruction summaries and edit affordance (issues #3183–#3184), **git status** (branch, dirty, ahead/behind; issue #3185), and **project creation** (mkdir + git init; issue #3186). Amends **§8 Visual system** to make **Foundry design system (v1)** (`docs/design/UI/design-system/`) the **normative visual reference**, replacing the missing handoff PDF citations; documents token sourcing, dark theme ("Night Shift") OS integration (§8.1, AC-27), and Foundry component guardrails (§8.2). Adds **§7C.1** (`SPEC-TCUI-15~draft`) specifying the **daemon-served instruction-file edit route** (read + write CLAUDE.md / AGENTS.md, path-guarded, issue #3188) with AC-24 guards against traversal and arbitrary fs access. Adds optional §7D.1 for live git-status polling. Updates Follow-ups to reflect addendum dependencies and Foundry merge (F8–F9). Documents that `SPEC-TCUI-10~draft` … `-17~draft` (or `-16~draft` depending on AC audit) are the new spec IDs claimed by this addendum.
- **2026-07-16** — Initial draft (DOC-39, `SPEC-TCUI-01~draft` … `SPEC-TCUI-09~draft`).
- **2026-07-16** — **Daemon-is-everything amendment** (owner directive, Bob). Adds §2.1
  `SPEC-TCUI-09~draft` — *"The UI communicates with the daemon; the daemon provides all
  functionality"* — as a foundational **architectural** constraint (C-1 thin client, C-2
  daemon-as-sole-source, C-3 no capability divergence, C-4 no Tauri-native fs/dialog), and
  propagates it: **Q2 RESOLVED** (the web/Tauri fork dissolves — directory inspection is an
  API concern, not a platform capability); new **§4.2.1** (the 7a picker is daemon-served,
  and its `git` field drives §4.2's binding — one call, two jobs); new **§5.8**
  `project.list_dir` **added to the Phase-1 cut line** (now six items), paired with
  projectless because 7a is blocked twice; **Q6 narrowed** (client-side cost computation is
  now a violation → streaming cost event is the only conforming answer); **AC-11.8** (no
  direct trusty-memory access from the client); **AC-7.4** (⌘K find is a daemon query);
  **§5.4** roster event-folding recorded as a time-boxed §2.1 loan rather than a design.
  Records why `project.list_dir` carries **no path-guard layer**: tcode is a local app and
  the daemon already exposes `task.run` (arbitrary code as the operator), so a listing is
  strictly less powerful — and #2747's `allow_sensitive_path` guards *indexing*, not path
  listing, so it is not precedent here. No TCC permission-state machine; index readiness
  (§4.3) remains the empty/degraded-state inventory's real member.
- **2026-07-16** — **Review-fix amendment** (PR #2855 review). Corrects §4.2, §5.5, and
  §6.2.1's claim that `task.run` "takes `project: PathBuf` — REQUIRED (`task/protocol.rs:48`)"
  as a per-request field: `task/protocol.rs:48` is `register()`'s parameter, wired **once at
  daemon-boot time** (`serve::build_router`, `serve/mod.rs:91,97`) — `TaskRunRequestParams`
  (`task/protocol.rs:61-91`) never carried a project field to begin with. Projectless is
  therefore a **daemon-bootstrap / binding-lifecycle** change, not the request-DTO
  `project: PathBuf → Option<ProjectBinding>` type swap the spec previously described; §6.2.1's
  "a type change, not a subsystem — small, mechanical, testable" sizing claim is corrected to
  match. The corrected binding model matches what **PR #2860** (`feat-tcode-api-projectless`,
  open) actually implements: a `binding::ProjectBinding` enum (`None` / `Directory(PathBuf)` /
  `GitRepo(PathBuf)` — exactly §4.2's three states) threaded as `ProjectBinding` (not
  `Option<ProjectBinding>`) through `build_router`/`task::protocol::register` at boot time, and
  `session.create`'s `CreateParams.project` retyped from the untyped label to `Option<PathBuf>`
  resolved per-call via `ProjectBinding::resolve`. Also syncs the `docs/specs/README.md`
  DOC-39 catalog row to `SPEC-TCUI-01~draft` … `-09~draft` (it had not been bumped for §2.1).
