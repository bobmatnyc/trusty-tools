---
spec_refs:
  - id: SPEC-WS-02~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-02~draft
  - id: SPEC-WS-04~draft
    path: docs/specs/DOC-48-tcode-workstreams.md
    anchor: SPEC-WS-04~draft
  - id: SPEC-TCUI-08~draft
    path: docs/specs/trusty-code-harness-ui.md
    anchor: SPEC-TCUI-08~draft
  - id: SPEC-AGENTS-08~draft
    path: docs/specs/trusty-agents-product-spec.md
    anchor: SPEC-AGENTS-08~draft
---

# DOC-52 — Workstream, Task, and Session: The Canonical Cross-Product Glossary

**Status:** Draft (Rev 3)
**Subsystem:** trusty-tools — cross-product (trusty-mpm, trusty-code, trusty-agents, trusty-memory) vocabulary for *workstream*, *task*, and *session*; plus workstream lifecycle and resource governance
**Owner:** Engineering (trusty-mpm, trusty-code, trusty-agents coordination) / Bob Matsuoka
**Last-updated:** 2026-08-01
**Spec ID:** `SPEC-SHAREDWS-01~draft` … `SPEC-SHAREDWS-06~draft` (DOC-52)
**Builds on:**
- [`docs/specs/DOC-48-tcode-workstreams.md`](./DOC-48-tcode-workstreams.md) — the daemon-scoped workstream model for trusty-code, whose `session_ids: Vec` shape this spec now *ratifies* rather than overrides (§4.2)
- [`docs/specs/trusty-code-harness-ui.md`](./trusty-code-harness-ui.md) (DOC-39) — the "infinite thread you pick up, never start over" product framing (§4.1)
- [`docs/specs/trusty-agents-product-spec.md`](./trusty-agents-product-spec.md) (DOC-54) — the memory-classification concept that currently overloads the word "workstream" (§4.3)
- [`docs/adr/0019-unified-ipc-messaging-on-event-bus.md`](../adr/0019-unified-ipc-messaging-on-event-bus.md) (ADR-0019) — workstream-keyed addressing for cross-PM messaging
- [`docs/adr/0016-orchestration-hierarchy-lead-pm-assistant.md`](../adr/0016-orchestration-hierarchy-lead-pm-assistant.md) (ADR-0016) — durable orchestration hierarchy using workstream/role identity
- Issue #3649 (session-owned worktrees) and #2919 (auto-reclamation) — resource lifecycle tied to workstream ownership

**Cross-ref (code):** `crates/trusty-code/src/workstreams/model.rs:119-138`; `crates/trusty-code/src/session/model.rs:138-166`; `crates/trusty-mpm/src/core/session.rs:117-127`; `crates/trusty-mpm/src/core/session_launch/workstream_label.rs:8`; `crates/trusty-agents-common/src/workstreams/types.rs:121-149`; `crates/trusty-agents/src/api/server/workstreams/mod.rs:80`; `crates/trusty-agents/src/api/server/routes.rs:121-128`; `crates/trusty-mpm/src/core/sm/agent/delegate/decision.rs:36`

**Authority (what this document IS):**

This document is the **single authoritative glossary** for the words **workstream**, **task**, and **session** across the trusty-tools workspace. Where any other spec, rustdoc, README, or agent instruction in this repository defines those three words differently, **this document wins** and the other site is drift to be corrected (Appendix A).

Its authority is scoped precisely to that: the *vocabulary* and the *cardinality* between the three terms (§1, §2), the per-product mapping (§3), and the workstream lifecycle/resource rules that follow from them (§5). It does not govern any product's API shape, storage layer, or UI surface — those stay with each product's own spec.

**Supersedes / re-scopes:**
- **Rev 2's own §2.1** ("exactly ONE session per workstream, immutable 1:1, repository-wide") is **REVOKED** by the owner decision of 2026-08-01. The replacement invariant is §2: many sessions over a workstream's lifetime, at most one active at a time. trusty-mpm keeps 1:1 as a **sanctioned permanent exception** (§1.5, §3.1), not as the repository-wide rule.
- **DOC-48 §2.1/§4.1** — Rev 2 declared these OVERRIDDEN. That override is **withdrawn**: DOC-48's append-only `session_ids: Vec<SessionId>` is the correct shape under this Rev and needs no migration or grandfathering (§4.2).
- **DOC-39 §3.1** — re-scoped, not superseded: DOC-39's product framing is compatible; only its silence on tasks is filled in (§4.1).
- **DOC-54 §9** — its "workstream" is a **different concept** (memory-tag topic classification) that should be renamed rather than reconciled (§4.3). This spec does not edit DOC-54; it records the recommendation and the ticket to file.

---

## 1. Canonical definitions {#SPEC-SHAREDWS-01~draft}

**ID:** SPEC-SHAREDWS-01~draft
**Status:** Draft

Three terms, three distinct levels. They are **not** aliases for one another, and no product may use one as a friendly synonym for another.

```
Workstream  ── the durable unit of work: one PM + the agents it dispatches
   │
   ├── Task ── one dispatched unit of work        (many per workstream, over time)
   ├── Task
   └── Task
   ▲
   └── Session ── the connection to the workstream (many over time, one active)
```

### 1.1 Workstream (NORMATIVE)

> A **workstream** is a single PM plus the agents that PM dispatches. It is the **durable unit of work**: it is scoped to one body of files in one repository, it **contains many tasks**, it outlives any individual session, and it stays open until it is closed.

Clause by clause:

- **A single PM plus its dispatched agents.** The PM is the workstream's driver and its identity. Agents the PM dispatches are *inside* the workstream, not workstreams of their own. A second PM means a second workstream.
- **Durable.** A workstream survives disconnects, daemon restarts, and machine reboots. It is not a runtime object; it is a persisted record that runtime objects attach to.
- **Contains many tasks.** This is the load-bearing hierarchy claim (§1.2). A workstream is a *container* of dispatched work, not itself a unit of dispatched work.
- **One repo, one body of files.** One branch, one worktree (§5.2). A workstream does not span repos or projects (§7).
- **Inferred, not explicitly created.** A workstream is established by work starting — the first task or the first commit — and may be named or labeled after the fact. Labeling is not creation.
- **Open until closed.** Closure is explicit and meaningful: branch merged, worktree reclaimed, PM decommissioned (§5.4). A closed workstream is never reopened; later work on the same files establishes a *new* workstream.

### 1.2 A workstream is not a task, and a task is not a workstream (NORMATIVE)

> **Owner decision (Bob, 2026-08-01):** "A workstream is not a task, a workstream can have many tasks, let's keep this distinct for Agents — they should be task based."

This is the single most frequently violated rule in the current vocabulary, so it is stated as an explicit prohibition:

- A workstream **MUST NOT** be described as "a task", "a big task", or "the user-facing name for a task."
- A task **MUST NOT** be described as "a workstream", "a small workstream", or "the internal name for a workstream."
- Documentation that presents one as the user-facing label and the other as the internal label for **the same object** is drift (Appendix A, D-6).

They are different levels of the hierarchy. Cardinality is 1 workstream : N tasks.

### 1.3 Task (NORMATIVE)

> A **task** is a single dispatched unit of work within a workstream. It has a beginning, an end, and a result.

This definition is **descriptive, not prescriptive** — it is what the shipped code already means by "task", and this spec ratifies that meaning rather than changing it:

| Surface | Site |
|---|---|
| `POST /api/task` (submit), `GET /api/task/{id}`, `DELETE /api/task/{id}` | `crates/trusty-agents/src/api/server/routes.rs:121-128` |
| `TaskSpec` (the PM's dispatch record) | `crates/trusty-mpm/src/core/sm/agent/delegate/decision.rs:36` |
| `task.run` (daemon-owned task execution) | `crates/trusty-code/src/lib.rs:230,276,463` |

**No renaming.** No existing task API, type, route, or CLI verb is renamed by this spec. `POST /api/task` stays `POST /api/task`; `TaskSpec` stays `TaskSpec`; `task.run` stays `task.run`.

**trusty-agents is deliberately task-based.** Its user-facing surface is expressed in tasks, and that is the intended product framing, not an accident to be corrected (§3.3). A user of trusty-agents submits tasks; the workstream is the container those tasks belong to.

**Not to be confused with trusty-memory's Task drawer.** `task_add` / `task_list` / `task_complete` (`crates/trusty-memory/tests/task_mcp.rs`) are a **memory-palace drawer type** for recording goals and milestones. That is a genuinely different, unrelated use of the word "task" — it is a note in a memory palace, not a dispatched unit of work. **Do not unify the two.** No mapping, adapter, or shared type between them is wanted (§7).

### 1.4 Session (NORMATIVE)

> A **session** is the **connection to a workstream**. There are **many sessions over a workstream's lifetime, and at most one active at a time**.

- A session is the runtime attachment: the process, pane, socket, or turn-thread through which work is currently driven.
- A session is **not** the workstream. The workstream outlives it.
- You **attach**, you **detach**, and you **reattach later** — to the *same* workstream (§2.2).
- Within one session, any number of *clients/connections* may observe and drive it simultaneously (§2.3, tmux-attach semantics). Clients are not sessions.

### 1.5 The trusty-mpm exception (NORMATIVE, permanent)

> In **trusty-mpm**, session ≡ workstream. The binding is 1:1 and the two words denote the same object.

This is a **permanent, documented, sanctioned exception** — it is intentional and correct for trusty-mpm's model, in which a PM session *is* the durable unit and its tmux session name *is* the workstream name (the `ws/<name>` GitHub label convention, DOC-53). It is **not** drift, **not** a bug, and **not** something scheduled to be fixed later. No ticket should be filed to "reconcile trusty-mpm with the canonical cardinality"; the exception is the reconciliation.

Consequences that follow from the exception, and that implementers must preserve:

- trusty-mpm has no `Workstream` type, and does not need one. Its `Session` (`crates/trusty-mpm/src/core/session.rs:117`) carries the workstream's identity, and `active_delegations` (`:127`) counts the tasks currently dispatched inside it.
- DOC-53's `ws:<name>` identity, drawn from the tm-assigned session name, remains valid **because of this exception** — not because of a repository-wide 1:1 rule (Appendix A, D-5).

---

## 2. Cardinality: sessions attach to workstreams {#SPEC-SHAREDWS-02~draft}

**ID:** SPEC-SHAREDWS-02~draft
**Status:** Draft

**This section replaces Rev 2's §2.1 in full.** Rev 2 asserted "exactly ONE session per workstream, immutable, repository-wide." That invariant is revoked.

### 2.1 The invariant (NORMATIVE)

> **A workstream has many sessions over its lifetime, and at most one active at a time.**

| Relation | Cardinality | Note |
|---|---|---|
| Workstream → Task | 1 : N | The hierarchy claim of §1.2. Tasks are contained, not aliased. |
| Workstream → Session (lifetime) | 1 : N | Append-only history. Sessions accumulate as the workstream is attached and reattached. |
| Workstream → Session (at any instant) | 1 : 0 or 1 | Zero when detached; one when attached. Never two. |
| Session → Workstream | 1 : 0 or 1 | A session binds to at most one workstream, at creation, immutably. Unbound is valid. |
| Session → Client connection | 1 : N | Multi-client mirror (§2.3). Connections own nothing. |

**Zero active sessions is a normal, healthy state**, not an error and not a closure trigger. A workstream with no active session is *detached*, not *closed*. Only explicit closure (§5.4) ends a workstream.

### 2.2 Operational meaning: reattach resumes, it does not mint

This is the practical consequence implementers must get right:

- **Reattaching after a disconnect resumes the SAME workstream.** A dropped connection, a closed terminal, a daemon restart, a laptop reboot — none of these end the workstream. The next attach picks it back up, with its history, its scope, its worktree, and its task record intact.
- **Reattaching MUST NOT mint a new workstream.** Creating a second workstream where the operator expected to resume the first is a defect. It orphans the worktree (#3649), defeats auto-reclamation (#2919), and splits one body of work across two records.
- **A new session on reattach is expected and correct.** The session id may differ every time; the workstream id must not. Anything that needs a stable key — ADR-0019 message addressing, `owner_session_id` resolution, claim drawers (DOC-53), `ws/<name>` labels — keys on the **workstream**, never on the session.
- **The session-id list is append-only.** Prior sessions stay on the record as lineage. They are history, not garbage to be pruned.
- **Handing off is attach, not fork.** A different operator or a different machine attaching to a workstream continues it. There is no "copy" or "branch" of a workstream.

### 2.3 Access model — multi-client mirror

Within the one active session, access uses tmux semantics:

- **One owning PM.** Work is driven by the workstream's single PM (§1.1).
- **Multiple clients/connections.** A GUI tab, a CLI client, a Slack relay may all attach at once. Each can send input; resulting state mirrors across **all** attached connections simultaneously.
- **No exclusive lease per connection.** Unlike DOC-40's per-agent `AttachmentLease`, workstream sessions use daemon-enforced singleton activation (DOC-48 §6) or the per-product equivalent. Observing clients need no lease.
- **Connections never own anything.** Closing a connection closes neither the session nor the workstream.

### 2.4 Lifecycle

| Phase | Trigger | Meaning |
|---|---|---|
| **Established** | Work starts — first task or first commit; never an explicit "create workstream" command | The workstream record exists; may be named/labeled afterwards |
| **Attached** | A session binds to it and activates | Exactly one active session; tasks may be dispatched |
| **Detached** | The active session ends (disconnect, restart, explicit stop) | Zero active sessions. The workstream is **still open**. Its worktree and scope are retained. |
| **Reattached** | A new session binds to the same workstream | Same workstream resumed (§2.2). The new session id is appended to lineage. |
| **Closed** | Explicit closure: branch merged, `workstream.close`, PM decommissioned | Terminal. No new sessions, no new tasks. Historical review only. |
| **Reclaimed** | Closure triggers cleanup (#2919) | Worktree (tm), build artifacts (tcode), tied resources released per #3649 |

Detach ⇄ reattach may repeat any number of times between *Established* and *Closed*.

---

## 3. Per-product mapping {#SPEC-SHAREDWS-05~draft}

**ID:** SPEC-SHAREDWS-05~draft
**Status:** Draft

What each product calls these things today, what its user-facing surface says, and where it deliberately diverges from §1–§2. A **deliberate divergence** is sanctioned; anything not listed here as deliberate is drift (Appendix A).

| | **trusty-mpm** | **trusty-code** | **trusty-agents** |
|---|---|---|---|
| **Workstream — internal** | No `Workstream` type. The `Session` record *is* the workstream (`core/session.rs:117`). | `Workstream { id, name, session_ids, … }` (`workstreams/model.rs:119-138`) — the closest implementation to canon. | Two disconnected implementations (§3.3). |
| **Workstream — user-facing** | "session" (`tm session new/list/resume`), plus the `ws/<name>` GitHub label (`session_launch/workstream_label.rs`) and DOC-53 claim drawers. | "workstream" — named, listed, activated, closed. | Sidebar grouping over `ws:<name>` memory tags. |
| **Task — internal** | `TaskSpec` (`core/sm/agent/delegate/decision.rs:36`); `Session.active_delegations: u32` (`core/session.rs:127`) counts live ones. | `task.run` daemon-owned execution (`lib.rs:463`). | `TaskStore` behind `POST /api/task` (`api/server/routes.rs:121`). |
| **Task — user-facing** | "delegation" — the PM dispatches agents. | "task" / "turn". | **"task"** — the primary user-facing noun. |
| **Session — internal** | `Session` (`core/session.rs:117`) — tmux or native process. | `Session { id, task, workstream_id: Option<WorkstreamId>, … }` (`session/model.rs:138-166`). | At least eight `*Session` types across the crate (§3.3). |
| **Session — user-facing** | "session" (= the workstream). | "session" — a turn thread inside a workstream. | Largely internal; the surface speaks tasks. |
| **Deliberate divergence** | **session ≡ workstream, 1:1** (§1.5). Permanent, sanctioned. | None. trusty-code is the reference implementation of §2. | **Task-based user-facing surface** (§1.3). Permanent, sanctioned. |

### 3.1 trusty-mpm — session ≡ workstream

trusty-mpm's divergence is the §1.5 exception and needs no further reconciliation. Read every occurrence of "session" in trusty-mpm's own docs, CLI help, and rustdoc as meaning *workstream* in this glossary's terms.

The one thing trusty-mpm must still honor from §1.2: **its sessions contain many delegations, and a delegation is a task.** `active_delegations` is a count of in-flight tasks within one workstream — it is exactly the 1:N hierarchy of §1.2, already implemented.

### 3.2 trusty-code — the reference implementation

trusty-code implements §2 correctly and is the model other products should follow. It carries a real bidirectional link:

- `Workstream.session_ids: Vec<String>` — append-only, "never remove an entry" (`crates/trusty-code/src/workstreams/model.rs:127-131`). This is §2.1's lifetime cardinality, already shipped.
- `Session.workstream_id: Option<WorkstreamId>` — "Set exactly once, at creation … there is no setter that changes it afterward" (`crates/trusty-code/src/session/model.rs:161-166`). This is §2.1's session→workstream immutability, already shipped. `None` (unbound) is explicitly a valid state, not a missing one.

**Known gap:** trusty-code's `Workstream` models **no PM or agent fields**. §1.1 defines a workstream as "a single PM plus the agents that PM dispatches", but the type carries only `id`, `name`, `session_ids`, timestamps, and a free-form `metadata` map. The driver identity is therefore not first-class where canon says it is definitional (Appendix A, D-1).

### 3.3 trusty-agents — task-based by design, with two disconnected workstream implementations

**The task-based surface is deliberate and stays.** Per §1.3, trusty-agents users submit tasks; that framing is the product decision, not an artifact.

Its *workstream* story, however, is split in two:

1. **Canonical-shaped but dead.** `crates/trusty-agents-common/src/workstreams/types.rs:121-149` defines a `Workstream { id, ticket_ref, title, assigned_harness, session_ids: Vec<String>, status, … }` — shaped very close to canon, and its own doc comment already anticipates §2 ("`tm` workstreams typically hold one; `tcode` workstreams may hold several"). It has **zero call sites** outside its own module: a repository-wide search for `workstreams::types::Workstream` returns no hits beyond the defining module. It is dead code awaiting DOC-44, which is unmerged.
2. **Live, but identity-by-memory-tag.** `crates/trusty-agents/src/api/server/workstreams/mod.rs` derives workstreams entirely from trusty-memory's `ws:<name>` tag convention (`WORKSTREAM_TAG_PREFIX = "ws:"`, `:80`), bucketing drawer rows by tag to produce a sidebar listing. There is no workstream record, no session list, and no lifecycle — a "workstream" here is whatever set of memory rows share a tag.

Neither implementation references the other. Additionally, at least eight distinct `*Session` types exist in the crate (`session.rs:77` `AgentSession`, `ctrl_session.rs:46` `Session`, `session_record.rs:32` `SessionRecord`, `memory/session_store.rs:42` `SessionMeta`, `memory/graph/mod.rs:42` a second `AgentSession`, `tm/project.rs:245` `TmSession`, `tmux/session.rs:14` `TmuxSession`, `api/server/tm.rs:29` `TmSessionDto`), none of which is the §1.4 session. Unifying them is out of scope here; the count is recorded so the next implementer knows what they are walking into (Appendix A, D-3).

---

## 4. Reconciliation with DOC-39, DOC-48, and DOC-54 {#SPEC-SHAREDWS-06~draft}

**ID:** SPEC-SHAREDWS-06~draft
**Status:** Draft

Four incompatible definitions of "workstream" existed in `docs/specs/` as of 2026-08-01 — DOC-39, DOC-48, DOC-52 Rev 2, and DOC-54 — **all with status Draft**, so none had formal precedence over the others. This section states, for each, what it currently says and how it is superseded or re-scoped.

### 4.1 DOC-39 — `trusty-code-harness-ui.md` — RE-SCOPED (compatible)

> "The unit of work. An infinite thread with state `active · idle · closed` that you *pick up*, never 'start over'. Name is **inferred** from what you're doing. **N per project.** Resumable across daemon restarts."
> — `docs/specs/trusty-code-harness-ui.md:190`; cardinality restated at `:201`; "**Workstream ≠ session**" at `:204`

**Disposition: compatible, re-scoped — DOC-39 needs no correction.** Its framing is the product-language expression of §1.1 and §2:

| DOC-39 phrase | Canon equivalent |
|---|---|
| "the unit of work" | §1.1 "the durable unit of work" |
| "an infinite thread… you pick up, never start over" | §2.2 reattach resumes the same workstream |
| "name is inferred" | §1.1 inferred, not explicitly created |
| "resumable across daemon restarts" | §1.1 durable |
| "Workstream ≠ session" (`:204`) | §1.4 — **DOC-39 was right, and Rev 2 was wrong to narrow it** |

**What Rev 2 got wrong.** Rev 2 "clarified" DOC-39's infinite thread as "infinite turns within *one* session." That clarification is withdrawn. Under §2, an infinite thread spans **many** sessions — which is what makes it infinite in the first place.

**What DOC-39 is silent on.** DOC-39 does not define *task* and therefore says nothing about the 1:N containment of §1.2. That silence is filled by this document, not by an edit to DOC-39.

### 4.2 DOC-48 — `DOC-48-tcode-workstreams.md` — RATIFIED (override withdrawn)

> "A **Workstream** is a durable named grouping of sessions, scoped to a single daemon instance, with mutable state inferred from session activity."
> — `docs/specs/DOC-48-tcode-workstreams.md:74`
>
> "**session_ids** | `Vec<SessionId>` | Append-only | … Sessions are never removed from the list; only new ones are added."
> — `:81`

**Disposition: RATIFIED. Rev 2's override of DOC-48 §2.1/§4.1 is WITHDRAWN.**

DOC-48 describes what trusty-code's shipped code actually implements (§3.2), and under the 2026-08-01 owner decision it is also what canon requires. The append-only `session_ids: Vec<SessionId>` **is** §2.1's lifetime cardinality. Concretely, everything Rev 2 imposed on DOC-48 is hereby void:

- ❌ **Void:** the `len(session_ids) == 1` invariant for new workstreams.
- ❌ **Void:** the "grandfathered historical lineage" framing for arrays with >1 entries. Those arrays are **normal**, not legacy artifacts.
- ❌ **Void:** any migration, read-only marking, or deprecation of multi-session workstreams.
- ✅ **Retained:** `pickActiveSessionInWorkstream` (`DOC-48:593`) is correct and necessary — selecting the one active session among many is exactly §2.1's "at most one active at a time."
- ✅ **Retained:** DOC-48's daemon-scoped activation (§6) and SSE fan-out over `session_ids` (§5.3, §308-310) were never in dispute and remain unchanged.

**Follow-up required:** DOC-48 §11.1 currently records Rev 2's override as normative (`DOC-48:655`, `:660`, `:664`). That note is now false and must be replaced with a pointer to this section. It is a **doc-only edit to DOC-48**, listed in Appendix A (D-4) — this spec does not perform it, to keep the change reviewable in one place.

**Remaining gap (not a conflict):** DOC-48's `Workstream` is "a grouping of sessions" and does not mention a PM, agents, or tasks. Canon (§1.1) makes the PM definitional. That is an under-specification to close, not a contradiction to resolve (Appendix A, D-1).

### 4.3 DOC-54 — `trusty-agents-product-spec.md` — DIFFERENT CONCEPT, rename recommended

> "**tasks** (user-facing workstreams), **workstreams** (resumable, agent-tagged memory history)"
> — `docs/specs/trusty-agents-product-spec.md:13`
>
> "**User-facing:** Tasks (sidebar, user language). **Internal:** Workstreams (memory tagging, APIs, spec vocabulary)."
> — `:388-389`
>
> "Workstreams are **agent-inferred classifications over the continuous conversation**, persisted in trusty-memory."
> — `:393` (§9, `SPEC-AGENTS-08~draft`, `:381`)

**Disposition: this is a THIRD, unrelated concept overloading the same word.** DOC-54's "workstream" is not a durable unit of work with a PM, tasks, a branch, and a worktree. It is a **memory-classification label**: an agent-inferred tag over a continuous conversation, used to group memory rows and filter a sidebar. It has no lifecycle, no scope, no owner, and no resources. It is implemented as exactly that — the `ws:<name>` tag bucketing at `crates/trusty-agents/src/api/server/workstreams/mod.rs:80` (§3.3).

Two specific conflicts with canon:

1. **`:13` and `:388-389` present "task" and "workstream" as the user-facing and internal names for the same object.** §1.2 prohibits this directly: they are different levels of the hierarchy, not two labels for one thing.
2. **The word itself is overloaded.** DOC-54's concept and canon's concept can both legitimately exist in trusty-agents — an agent may well infer topic labels over a conversation *and* run inside a workstream — but they cannot both be called "workstream."

**Recommendation (NORMATIVE as a recommendation, not as an edit):**

> **DOC-54 §9 should rename its concept to "topic" (preferred) or "thread", repository-wide, and stop using "workstream" for it.** "Topic" reads correctly in every sentence DOC-54 already uses it in ("agent-inferred topic classification", "topic-tagged memory history", "the sidebar filters by topic"), and it frees "workstream" for the canonical meaning. The `ws:` memory-tag prefix may keep its wire form for compatibility, but its documented meaning should follow the rename.
>
> Reconciling DOC-54 to canon in place is **not** recommended — the concept genuinely differs, and forcing it into §1.1's definition would lose what DOC-54 actually needs.

**This spec does not edit DOC-54.** File a ticket against `docs/specs/trusty-agents-product-spec.md` §9 (`SPEC-AGENTS-08~draft`) carrying this recommendation, and cross-reference it from Appendix A (D-2). Until that lands, read every "workstream" in DOC-54 §9 and `:13` as **topic**, and every "workstream" elsewhere in this repository as §1.1.

### 4.4 Precedence summary

| Document | Its "workstream" | Disposition |
|---|---|---|
| **DOC-52** (this doc) | One PM + its agents; durable; contains many tasks; many sessions over time | **Authoritative** for workstream / task / session |
| DOC-39 `:190` | "Infinite thread… N per project. Workstream ≠ session" | Compatible; re-scoped (§4.1). Rev 2's one-session "clarification" withdrawn |
| DOC-48 `:74`,`:81` | "Durable named grouping of sessions"; `session_ids` append-only | **Ratified** (§4.2). Rev 2's override withdrawn; §11.1 note to be corrected |
| DOC-54 `:13`,`:388-393` | "Agent-tagged memory history"; task⇄workstream as user/internal aliases | **Different concept** (§4.3). Rename to *topic*; ticket to file; not edited here |
| DOC-52 **Rev 2** | "Exactly ONE session, immutable 1:1, repository-wide" | **Revoked** by this Rev |

---

## 5. Resource rules and governance {#SPEC-SHAREDWS-03~draft}

**ID:** SPEC-SHAREDWS-03~draft
**Status:** Draft

These rules are unchanged in substance from Rev 2 — they attach to the *workstream*, which §2 leaves as durable as before. Only the wording that presumed 1:1 session binding is corrected.

### 5.1 Concurrency caps on OPEN workstreams — scaled to repo size

**The number of concurrent open workstreams a repo supports is a factor of the repo's size, not a fixed number applied uniformly across projects** (owner decision, 2026-07-29). A small single-crate repo and a large monorepo do not carry the same safe concurrency; the cap is computed from repo-size signals (file count, worktree/build footprint) rather than hardcoded. Machine-wide and per-project quotas remain configurable overrides on top of the computed value.

**Rationale:** A worktree with a Rust `target/` directory costs ~14 GB on average; uncontrolled proliferation exhausts disk (a 1.1 TB leak occurred 2026-07-21 from orphaned worktrees). Caps force intentional closure before opening new work.

**Behavior:**
- Opening a workstream past the computed cap is REFUSED by default; `--force` overrides visibly.
- Per-project disk quota (optional, not Phase 1): a secondary gate on projected worktree + `target/` usage (default ~100 GB).
- **The cap counts OPEN workstreams, not active sessions.** A detached-but-open workstream (§2.4) still consumes a slot, because it still holds a worktree. This is the one place §2's cardinality change matters here: "open" is a workstream state, and it is independent of whether any session is currently attached.
- **Implementation note:** this spec commits to the repo-size-scaling *principle*; the exact sizing function is implementation scope (§7).

### 5.2 Scope declaration and overlap gate

**Every workstream carries a scope** — a set of path globs (in a monorepo: crate names, package paths, directory patterns), inferred from the driving issue or first commits and refinable by the operator.

**Exclusive file ownership (owner decision, 2026-07-29):** any file in a workstream's *owned* scope — the substantive source, docs, and config it is driving change through — belongs to exactly one workstream at a time. Two workstreams never concurrently own edits to the same owned file. A file MAY change hands between workstreams over time (A closes, B later edits it) — the invariant is about concurrent ownership, not permanent assignment. "Owned" **excludes** the exempt class below; those are cross-workstream by necessity and merge-ordered (§5.3) rather than exclusively owned.

**Scope inference:** the first task or first commit determines initial scope (a commit touching `crates/trusty-search/src/` infers `crates/trusty-search/**`); multiple crates union.

**Overlap gate:**
- A new workstream whose scope OVERLAPS an already-OPEN workstream's scope is REFUSED by default.
- **Rationale:** monorepo CI and merge ordering demand scope separation; two workstreams in one crate risk conflicts, lockfile races, and ordering hazards.
- **Override:** `--accept-conflict={workstream_id}` makes the intent visible.

**Expected-conflict files (exempt):** root `Cargo.toml`, `Cargo.lock`, other lockfiles, per-crate `CHANGELOG.md`, generated artifacts (build outputs, vendored deps). Never refused on overlap; touching them WARNS and auto-widens scope with confirmation.

### 5.3 Merge minimization and staleness nudges

**Staleness nudges (informational, never enforced):**
- Branch **>20 commits behind** `main` (configurable) → nudge to rebase or merge.
- **>7 days** with no task dispatched → flagged as a close-or-justify candidate. **Idleness is measured by dispatched-task activity, not by session attachment** — a detached workstream is not thereby idle, and an attached one is not thereby active.

**Merge order:** smallest-scope-first; lockfile/generated-file touchers rebase after each sibling merge; one PR (or one stacked series) per close.

### 5.4 Close triggers reclamation

**Closing a workstream is THE trigger for resource cleanup.** Detaching is not — §2.4's *Detached* state retains the worktree deliberately, because reattach must resume it.

**Stable ownership key.** Issue #3649 tracks worktree ownership via an `owner_session_id` sentinel. Under §2, session ids are **not** stable across detach/reattach, so `owner_session_id` MUST be resolved to the **workstream id** and the workstream id used as the durable ownership key. This satisfies ADR-0019's "never key on fragile session_id" principle (which forbids session-keyed addressing to avoid #3396 pane-ID drift). Rev 2 justified this via a 1:1 mapping; that justification is replaced — the workstream id is the stable key **because** sessions come and go, which is a stronger argument than the one it replaces.

**Reclamation (#2919):**
- Closure triggers auto-reclamation of the worktree (trusty-mpm) or build artifacts (trusty-code).
- Worktree deletion is owner-gated via the workstream-id key; no unowned worktrees are deleted.
- Non-destructive at the git level — the branch may remain on the remote for audit; the local worktree and its artifacts are cleaned up.

---

## 6. Acceptance criteria and conformance {#SPEC-SHAREDWS-04~draft}

**ID:** SPEC-SHAREDWS-04~draft
**Status:** Draft

### AC-1: Vocabulary

**AC-1.1** No spec, rustdoc, README, CLI help string, or agent instruction in this repository defines *workstream*, *task*, or *session* in a way that contradicts §1. Contradicting sites are enumerated in Appendix A and are defects.

**AC-1.2** No document presents *workstream* and *task* as user-facing and internal names for the same object (§1.2).

**AC-1.3** No existing task API, type, route, or CLI verb is renamed as a consequence of this spec (§1.3).

**AC-1.4** trusty-memory's `task_add`/`task_list`/`task_complete` Task drawers remain a separate concept; no unification with §1.3 tasks is proposed or built.

### AC-2: Cardinality

**AC-2.1** A workstream accepts many sessions over its lifetime; the session list is append-only and prior sessions are retained as lineage.

**AC-2.2** At most one session is active per workstream at any instant. Zero active sessions is a valid, healthy state and is not an error, not idleness, and not closure.

**AC-2.3** Reattaching after a disconnect resumes the SAME workstream — same id, same scope, same worktree, same task history. Minting a new workstream on reattach is a defect.

**AC-2.4** A session binds to at most one workstream, set once at creation and never changed. Unbound is a valid state.

**AC-2.5** Multiple clients/connections may observe and drive the one active session simultaneously (tmux multi-attach). Connections are ephemeral and own nothing.

**AC-2.6 (trusty-mpm exception)** In trusty-mpm, session ≡ workstream 1:1. AC-2.1 and AC-2.3 are satisfied vacuously there. This is conformant, permanent, and requires no remediation.

### AC-3: Resource governance

**AC-3.1** Concurrency caps are scaled to repo size, not fixed per project; exceeding the computed cap refuses unless `--force`. The cap counts OPEN workstreams regardless of session attachment.

**AC-3.2** Scope overlap between OPEN workstreams is detected and refused by default; `--accept-conflict={id}` overrides. This enforces exclusive file ownership over *owned* scope.

**AC-3.3** Expected-conflict files (root `Cargo.toml`, lockfiles, `CHANGELOG.md`, generated artifacts) are exempt from the overlap gate; touching them widens scope with confirmation.

**AC-3.4** A commit outside declared scope (not exempt, not overlapping another open workstream) WARNS but is never refused.

**AC-3.5** Staleness nudges (>20 commits behind main; >7 days with no dispatched task) are informational only.

**AC-3.6** Workstream closure — not detach — triggers auto-reclamation.

### AC-4: Cross-product conformance

**AC-4.1** trusty-code conforms to §2 as shipped (`session_ids` append-only + `workstream_id` set-once) and is the reference implementation.

**AC-4.2** trusty-agents' user-facing surface remains task-based (§1.3); this is conformant by design.

**AC-4.3** Workstream-keyed addressing (ADR-0019) resolves to the workstream's currently-active session, or to none when detached. Addressing MUST NOT be keyed on a session id.

**AC-4.4** #3649's `owner_session_id` sentinel resolves to a workstream id, and reclamation keys on the workstream id (§5.4).

---

## 7. Out of scope and non-goals

- **Unifying trusty-memory's Task drawers with §1.3 tasks.** Explicitly not wanted (§1.3).
- **Renaming any existing task API.** Explicitly not wanted (§1.3, AC-1.3).
- **"Fixing" the trusty-mpm exception.** Sanctioned and permanent (§1.5).
- **Editing DOC-54.** This spec records the rename recommendation and the ticket to file; it does not touch that document (§4.3).
- **Unifying trusty-agents' eight `*Session` types**, or wiring its dead `trusty-agents-common` `Workstream` to its live tag-derived one. Recorded as drift (Appendix A, D-3), scoped elsewhere.
- **Multi-user workstreams.** Single-owning-PM only; team collaboration is future scope.
- **Cross-repo / cross-project workstreams.** One body of files in one repo (§1.1).
- **Enforcement implementation.** §5 states behavior contracts; where checks live, their CLI/API shape, and the cap-sizing function are implementation follow-ups.

---

## 8. Relationship to other specs

| Spec | Relationship |
|---|---|
| DOC-39 (trusty-code Harness UI) | Compatible; re-scoped (§4.1). Its "Workstream ≠ session" and "infinite thread" framing is upheld; Rev 2's one-session narrowing is withdrawn. |
| DOC-48 (tcode Workstreams) | **Ratified** (§4.2). Rev 2's override of §2.1/§4.1 is withdrawn; append-only `session_ids` is canon. DOC-48 §11.1's override note needs correcting (Appendix A, D-4). |
| DOC-53 (Workstream Claim-Drawer Convention) | Its `ws:<name>` identity from the tm session name stays valid — via the §1.5 trusty-mpm exception, not via a repo-wide 1:1 rule. Its §4.4 justification needs restating (Appendix A, D-5). |
| DOC-54 (Trusty Agents Product Spec) | Uses "workstream" for a **different concept** (§4.3). Rename to *topic* recommended; ticket to file (Appendix A, D-2). |
| DOC-59 (P1/P2 Instruction Restructure) | Cites `SPEC-SHAREDWS-01~draft` for the workstream/session binding its §7 makes the resolved corpus immutable against. The anchor still resolves; the binding it names is now §2's, not Rev 2's 1:1. |
| DOC-40 (Durable Background Agents) | Its per-agent exclusive `AttachmentLease` is a *different* access model from §2.3's multi-client mirror. Both coexist; neither is canon for the other. |
| ADR-0019 (Unified IPC) | Workstream-keyed addressing. §2 strengthens the case: session ids are not stable across reattach, so the workstream id is the only viable key (§5.4, AC-4.3). |
| ADR-0016 (Orchestration hierarchy) | Uses workstream/role identity for durable orchestration; §1.1's "one PM plus its dispatched agents" is the hierarchy's unit. |
| #3649 (Session-owned worktrees) | `owner_session_id` must resolve to a workstream id; reclamation keys on the workstream (§5.4). |
| #2919 (Auto-reclamation) | Closure — not detach — is the reclamation trigger (§5.4). |

---

## 9. Worked examples

### Example 1: Detach and reattach (the case Rev 2 got wrong)

Alice starts work on a search-indexer bug. The first commit establishes **workstream `ws-indexer-fix`**, scope `crates/trusty-search/**`, in its own worktree. Her PM dispatches four tasks across the afternoon (research, fix, test, review).

- Her laptop sleeps; the connection drops. `ws-indexer-fix` is now **Detached** — zero active sessions, still **open**, worktree retained, four tasks recorded.
- Next morning she reattaches. A **new session** binds to **the same workstream**. Same id, same scope, same worktree, same four tasks. The new session id is appended to lineage; the old one stays as history.
- Under Rev 2 this was illegal (the binding was immutable and the session terminal), which would have forced a second workstream and orphaned the worktree. Under §2 it is the expected path.
- She dispatches two more tasks and merges. **Now** the workstream closes, and the worktree is reclaimed.

### Example 2: Hierarchy, not aliasing

The same `ws-indexer-fix` workstream contains six tasks. It is not "the big task"; the six are not "six workstreams." In trusty-agents' surface those six would be six entries under `POST /api/task`; in trusty-mpm they would be six delegations counted by `active_delegations`; in trusty-code, six `task.run` invocations. One workstream, six tasks, in all three (§1.2).

### Example 3: Scope overlap

Alice opens a workstream for `crates/trusty-search/**`, then tries to open "Refactor embeddings" with the same inferred scope.

> Cannot open workstream 'Refactor embeddings': scope overlaps open workstream 'ws-indexer-fix' (`crates/trusty-search/**`). Pass `--accept-conflict=<id>` to override.

She either closes the first, or accepts the conflict and takes the rebase burden (§5.2).

---

## Appendix A — Drift register (work list)

Known sites that contradict §1–§2 as of 2026-08-01, with `file:line`. Each is a defect against AC-1.1 unless marked otherwise. **No code is changed by this spec** — this is the implementer's work list.

| ID | Site | Contradiction | Remedy |
|---|---|---|---|
| **D-1** | `crates/trusty-code/src/workstreams/model.rs:119-138`; `docs/specs/DOC-48-tcode-workstreams.md:74` | `Workstream` models **no PM or agent fields**, and DOC-48 defines it as "a grouping of sessions". §1.1 makes the single PM definitional. Under-specification, not a conflict. | Add PM/driver identity to the model (or document `metadata` as its carrier); restate DOC-48:74 as "a grouping of tasks and the sessions that drive them, owned by one PM". |
| **D-2** | `docs/specs/trusty-agents-product-spec.md:13`, `:388-389`, `:393` (§9, `SPEC-AGENTS-08~draft` at `:381`) | Uses "workstream" for a memory-tag topic classification, and presents task⇄workstream as user-facing/internal aliases for one object — prohibited by §1.2. | **File a ticket** to rename the concept to *topic* (or *thread*) throughout DOC-54 §9 and `:13` (§4.3). Do not edit DOC-54 as part of this spec's PR. |
| **D-3** | `crates/trusty-agents-common/src/workstreams/types.rs:121-149` (dead: zero call sites for `workstreams::types::Workstream` outside its own module) vs. `crates/trusty-agents/src/api/server/workstreams/mod.rs:80` (live, identity from the `ws:` memory tag) | Two disconnected `Workstream` implementations in one product; neither references the other; the live one has no lifecycle, session list, or owner. Compounded by ≥8 unrelated `*Session` types (`session.rs:77`, `ctrl_session.rs:46`, `session_record.rs:32`, `memory/session_store.rs:42`, `memory/graph/mod.rs:42`, `tm/project.rs:245`, `tmux/session.rs:14`, `api/server/tm.rs:29`). | Decide which implementation survives (the canonical-shaped one is closer to §1.1 but is blocked on unmerged DOC-44); delete or wire the other. Separately scope the `*Session` inventory. Not urgent — the task-based surface (§1.3) is unaffected. |
| **D-4** | `docs/specs/DOC-48-tcode-workstreams.md:655`, `:660`, `:664` (§11.1) | Records Rev 2's 1:1 override as normative, including the `len(session_ids) == 1` invariant and "grandfathered" framing. Now false (§4.2). | Doc-only edit: replace §11.1's override note with a pointer to DOC-52 §4.2 stating the override was withdrawn and `session_ids` append-only is canon. |
| **D-5** | `docs/specs/DOC-53-workstream-claim-drawer-convention.md:21`, `:127` | Justifies its `ws:<name>` identity by citing DOC-52's repository-wide "1:1 session↔workstream binding" — "the session name IS the workstream name". The *conclusion* is still correct; the *justification* is not. | Doc-only edit: restate the justification as resting on the **trusty-mpm exception** (§1.5), which is where DOC-53's session names come from, rather than on a repo-wide 1:1 rule. |
| **D-6** | `crates/trusty-mpm/src/core/session_launch/workstream_label.rs:8` | Rustdoc asserts "a workstream is **ephemeral and freely renamable**", inverting §1.1's durability. The surrounding argument (labels over milestones) is sound and unaffected — only this clause is wrong. | Rustdoc-only edit: replace with the real reason labels beat milestones (multi-valued, cheap, filterable, one-milestone-per-issue limit) and drop the ephemerality claim. A workstream is durable; its *label* is cheap. |
| **D-7** | This document, Rev 2 (superseded text) | Rev 2 asserted repository-wide 1:1 session binding, immutable bindings, terminal-on-close sessions, and a DOC-48 override. | Resolved by this Rev. No action; listed for auditability. |

**Not drift (do not "fix"):**

- `crates/trusty-mpm/src/core/session.rs:117-127` — trusty-mpm's `Session` carrying workstream identity with no `Workstream` type is the §1.5 sanctioned exception.
- `crates/trusty-agents/src/api/server/routes.rs:121-128` — the task-based surface is deliberate (§1.3, AC-4.2).
- `crates/trusty-memory/tests/task_mcp.rs` and the `task_add`/`task_list`/`task_complete` MCP tools — a different, unrelated concept (§1.3). Never unify.

---

## Appendix B — Change log

| Rev | Date | Change |
|---|---|---|
| Rev 3 | 2026-08-01 | Rewritten as the authoritative glossary for workstream/task/session (owner decisions, Bob, 2026-08-01). Workstream = one PM + its dispatched agents, containing many tasks. Task = one dispatched unit of work; no API renames. Session = the connection; many over time, one active — **revoking Rev 2's 1:1 invariant** and **withdrawing its DOC-48 override**. trusty-mpm's session ≡ workstream recorded as a permanent sanctioned exception. Added per-product mapping (§3), reconciliation of DOC-39/DOC-48/DOC-54 (§4), and the drift register (Appendix A). |
| Rev 2 | 2026-07-29 | Product-owner canonical definition; repo-size-scaled concurrency replacing the fixed per-project cap; exclusive file ownership made explicit. |
| Rev 1 | 2026-07-22 | Owner-approved 1:1 session binding, resource caps, scope-overlap rules. |
