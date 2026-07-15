# DOC-36 — `tm manager`: Layer-3 Chat-Based Portfolio Project Manager

**Status:** Approved (owner review complete 2026-07-14 — see §7 for per-item resolutions; implementation green-lit)
**Subsystem:** trusty-mpm — daemon / inference layer / external channels
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-07-14
**Approved:** 2026-07-14 by owner. Implementation green-lit; child issues to be filed and
tracked on GitHub milestone 10 (Chat-Based Project Manager) under epic #2109.
**Spec ID:** `SPEC-TMMGR-01~approved` … `SPEC-TMMGR-06~approved` (DOC-36)
**Builds on:** DOC-35 — `tm project`: Deterministic Project/Session Control Plane
(`docs/specs/tm-project-control-plane.md`, epic #2108, CLOSED/shipped-substrate) — especially
§1.1 (three-layer model), §10 (Deliverable/Milestone data model), §11 (the L2/L3 boundary
contract, LOAD-BEARING for this spec), §13 (the ambiguous-NL-routing-to-#2109 and
learned-autonomy-stays-SM-owned decisions this spec conforms to, not re-litigates).
**Cross-ref:** epic **#2109** (`tm manager`, THIS epic); epic **#2108** (`tm project`, CLOSED,
the deterministic substrate this spec consumes); issue **#1440** / `daemon/managed_routes/proxy/`
(the shipped L2 `SessionProxy`/`ManagedBackend` seam this spec's channel router layers on top
of); DOC-33 — One Session Manager (`docs/specs/one-session-manager.md`, adjacent, referenced not
owned); DOC-23 — Learned-Autonomy Auto-Answer (`docs/specs/learned-autonomy-auto-answer.md`,
adjacent, consumed not owned); epic **#2400** / issue **#2411** (`trusty_common::inference`, the
adapter layer this spec's LLM calls target — #2411 will retire `tm`'s
`core/sm/providers/bedrock.rs` onto it, so this spec targets the commons adapter directly rather
than the soon-to-be-retired path).

> **Scope note.** This is a **design spec**, filed per epic #2109's own "spec-first when picked
> up" directive. It proposes the portfolio-manager vision, the API-first daemon surface, the
> channel-binding architecture, the local-testability bar, and a phased delivery plan — it
> **restates DOC-35 §11's boundary contract as normative** rather than re-deriving it, and flags
> every owner-level fork in the road as an explicit decision point. **It carries no Rust changes**
> and files no child issues; the closing work-item table is unfiled, pending owner review of this
> document (see the PR this ships in).

---

## 1. Vision

`tm manager` is the **Layer-3 inference/portfolio layer** the owner's three-layer model
(DOC-35 §1.1) describes as "the last layer — the user talks to a SINGLE agent that has FULL SCOPE
of the user's activities." Concretely: today an operator running five sessions across three
projects has no single place to ask "what's going on across everything I've got running" and get
a reasoned answer, or to say "route this to whichever repo it belongs to" without first deciding
which repo that is themselves. `tm manager` is that single conversational entry point.

**Portfolio-level, not session-level.** DOC-35 built the deterministic substrate — `tm projects`,
`tm sessions`, the Deliverable/Milestone data model, per-project status rollups — precisely so
this layer would not have to invent portfolio-wide state itself (DOC-35 §1.1: "`tm project` exists
so that when `tm manager` is built, it inherits a data model and status surface that already spans
the whole portfolio deterministically"). `tm manager` is the reasoning layer over that substrate:
it synthesizes across projects, drafts digests, notices when something needs attention, and
disambiguates ambiguous requests — all things that require judgment, not just a poll.

**In contrast to Layer 2.** The shipped `SessionProxy`/`ManagedBackend` seam (#1440,
`crates/trusty-mpm/src/client/proxy.rs`) is explicitly single-session-focused per the owner's own
framing — even when a channel can reach many sessions serially through focus/inject/summarize, it
is still "one session at a time" conversation state. `tm manager` is different in kind, not degree:
one agent, one conversation, **simultaneous** awareness of the whole portfolio. A user talks to L2
about *a* session; a user talks to L3 about *their work*.

**What "full scope" cashes out to in this spec:**
1. Portfolio-wide status synthesis and digests (read, reason, summarize — never mutate).
2. Judgment on ambiguous natural-language routing (DOC-35 §13 Q1's resolution: the deterministic
   resolver primitive lives in #2108; *acting* on ambiguous/low-confidence input is #2109's).
3. Proactive stall/escalation detection across the portfolio (notify, per §2's resolution below).
4. A single external-channel identity for portfolio-scope conversation (Telegram/Slack), distinct
   from L2's per-session channel bindings.
5. Two-way communication with individual sessions, but *routed through* L2's existing
   inject/summarize primitives rather than reimplementing them (§3, §5).

### 1.1 Primary user story — owner directive (added 2026-07-14)

> **Owner's directive, verbatim (2026-07-14):** "The goal is that a given managed project runs a
> single primary task, though secondary ones can be added. The user should be able to ask the
> manager to perform a task in a project, which can include one or more repos. The manager will
> spin up one or more sessions (a session is always in a single repo) to handle the task, as well
> as cross project communication and tracking (the manager should have gh project skills and other
> ticketing agents). The manager spins up the sessions, tracks the goal of the sessions (in a
> tagged memory), sees them through to completion, asking the user for input when it lacks the
> confidence to auto-complete a task, watches context and total cost (and pauses, clears, and
> resumes sessions as needed), handles cross-session communication, and drives the project to
> completion. Note a project can have multiple sessions in a single repo, it should be orthogonal
> to the manager which tracks sessions from multiple repos or a single one. The manager tracks all
> active projects, and prioritizes communication with the user based on urgency and/or user
> provided scheduling/priority."
>
> **Owner's refinement (2026-07-14):** "It also looks like claude code with it's suggestions is
> doing much of the 'keep going' work for us. We should defer to those (but track them, verify
> they align with the manager's goals)."

**As the operator**, I want to ask `tm manager` to perform a task in a project — which may span one
or more repos — and have it own the full lifecycle: spinning up whatever sessions the task
requires, tracking each session's goal, watching context and total cost, communicating across
sessions and projects, and asking me only when it lacks the confidence to auto-complete a step —
so that I never have to hand-orchestrate sessions, babysit cost/context, or relay messages between
sessions myself.

**Derived requirements** (R1–R5):

- **R1 — Project/task model.** A given managed project runs exactly one PRIMARY task at a time;
  secondary tasks may be added alongside it. The primary/secondary distinction is a property of the
  project's task list, not of any one session.
- **R2 — Project↔repo↔session cardinality.** A project spans **1..N repos**. A session is always
  bound to **exactly one repo**. A project may run **multiple sessions in a single repo**. Repo
  count is explicitly **orthogonal** to the manager's session tracking — the manager tracks
  sessions the same way whether they come from one repo or many.
- **R3 — Manager responsibilities across a task's lifecycle.** For a task the user hands it, the
  manager:
  - spawns one or more sessions to execute the task (each session bound to one repo, per R2);
  - records each session's goal in **tagged `trusty-memory`** so the goal survives independent of
    any one conversation turn;
  - tracks each session through to completion;
  - **escalates to the user for input** whenever its confidence to auto-complete a step is
    insufficient — consistent with DOC-23's confidence-signal consumption (§5) and the notify-only
    posture of §2.2 OWNER DECISION 1;
  - **monitors context usage and total cost** per session, and **pauses, clears, and resumes**
    sessions as needed to manage both;
  - handles **cross-session communication** (within a project) and **cross-project communication**
    (portfolio-wide);
  - drives the project to completion end-to-end.
- **R4 — Tooling.** The manager has **gh-project (GitHub Projects) skills** and access to
  **ticketing agents** as first-class tool surfaces, alongside #2108's project/session APIs and
  L2's `SessionProxy` (§3.5).
- **R5 — Portfolio awareness and prioritization.** The manager tracks **all active projects**
  simultaneously (consistent with §1's "full scope" framing) and **prioritizes communication with
  the user based on urgency and/or user-provided scheduling/priority**, rather than a flat
  first-in-first-out or round-robin notification order.
- **R6 — Deference to Claude Code's native continuation mechanisms.** The manager **defers to the
  harness's native continuation/suggestion mechanisms** (Claude Code's suggestions, auto-continue,
  task notifications) for keeping sessions moving, rather than injecting its own redundant "keep
  going" nudges. It **tracks** those harness-driven suggestions and **verifies** they align with
  the session's tagged goal (per R3). It **intervenes** (pause/clear/resume) only on misalignment
  or cost/context pressure (per R3), never to override or duplicate the harness's native "keep
  working" signaling. This positions the manager as a **verify-and-steward layer** atop the
  harness's session mechanics, not as a competing continuation engine.

**Reconciliation note.** This user story *extends* the vision already approved via §7's seven
resolved owner decisions; it does not reopen or amend any of them. Two places deserve explicit
reconciliation in a later phase or spec revision, rather than being resolved here:

- **Cost/context-triggered pause/clear/resume, and the "keep going" default (R3, R6)** — The owner's
  refinement (R6) fundamentally reframes the manager's posture: it defers to the harness's native
  "keep working" signaling and intervenes only on misalignment or cost/context pressure. This
  largely resolves the tension between R3's "watches context and pauses/clears/resumes" and §2.1's
  "never act on a session without an explicit call" boundary rule. The manager's default is to
  **observe and verify** (consistent with §2.1 and §2.2 OWNER DECISION 1's notify-only default),
  with pause/clear/resume reserved for resource stewardship or goal misalignment — neither of which
  is a "proactive intervention" in the sense OWNER DECISION 1 regulated. A later revision should
  formalize this distinction and ensure cost/context-triggered pause/clear/resume is explicitly
  carved out (not folded into the opt-in intervention tier WI-13).
- **gh-project skills and ticketing-agent access (R4)** are new tool-surface additions not yet
  enumerated in §3's architecture or §5's layering table; a later revision should add them (e.g. a
  new §3.6 "Ticketing and gh-project tool access" and a corresponding §5 row).
- **Tagged per-session goal memory (R3)** should be reconciled against §3.4's portfolio-palace
  design — it is not yet specified whether per-session goal tags live in the portfolio palace or a
  project-scoped palace.

The remaining requirements (R1, R2, R5) are additive detail consistent with, and do not contradict,
the vision and boundary contract already approved in §1–§7.

---

## 2. Boundary conformance (normative — restates DOC-35 §11)

DOC-35 §11 is the load-bearing contract between #2108 and #2109. This spec does not re-derive it;
it restates it as **normative** for every work item this spec proposes, and resolves the one
tension DOC-35 flagged but left open.

### 2.1 DOC-35 §11, restated

| `tm manager` (#2109, this spec) OWNS | `tm manager` MUST NEVER |
|---|---|
| Cross-project synthesis — reasoning across MULTIPLE projects at once (portfolio-level), the one thing DOC-35 §11 explicitly excludes from #2108 even for pure counting/rollup | Poll/report ONE named project's already-computed state without adding anything — that's #2108's job; #2109 must not duplicate a #2108 endpoint verbatim |
| LLM-authored digests, summaries, and narratives not already produced by an existing deterministic or opt-in-LLM-with-fallback field | Mutate a Deliverable/Milestone record directly (DOC-35 §10) — status transitions happen through #2108's `set-status` verb, which #2109 may *call*, never bypass |
| Proactive stall/escalation detection (watching for "this needs attention") | Own or set an autonomy tier (T1–T4) — Session Manager / `AUTONOMY_POLICY.md` remains sole owner (DOC-35 §10.9 row 10, DOC-30 Decision #10, unchanged) |
| Judgment on ambiguous NL routing — disambiguation choices, not the deterministic matching primitive itself (DOC-35 §13 Q1: SPLIT — the 3-strategy confidence-scored resolver stays a #2108 substrate API; *acting* on a low-confidence/ambiguous result is #2109's) | Act on a session — inject, launch, kill, escalate — **without an explicit, traceable call driving it** (DOC-35 §11 row 5); every action this spec's manager takes must resolve to one deliberate API call, never a silent background side effect |
| Portfolio two-way channels — a single external-channel identity across the whole portfolio (§5) | Connect to external channels for **single-session** communication — that remains #1440/L2's `ManagedBackend` seam; #2109's channel binding is additive (a portfolio persona), not a fork of L2's session persona |

**The test DOC-35 §11 gives for "which epic does this belong to"** applies unchanged: pure
function of already-materialized state, same output every time → #2108. Requires an LLM call,
cross-project synthesis, or a judgment call not reducible to an explicit stored flag → #2109.

### 2.2 The one tension DOC-35 left open, and its proposed resolution

DOC-35 §11 says #2109 owns "proactive stall/escalation detection" (a watching, noticing role) in
the same table row that says #2109 must never "decide to intervene, escalate, or act on a session
without an explicit CLI/API call driving it." Read literally, these are in tension: *proactive*
implies acting without being asked; *no acting without an explicit call* implies waiting to be
asked. DOC-35 flagged this as a boundary the owner drew but did not fully resolve for #2109's own
spec to work out — this is that resolution.

**Where they meet:** "proactive" describes *what the manager watches for and surfaces*
(stall/escalation *detection*), not *what it does about it unprompted*. The manager can run a
background poll loop over #2108's activity/status endpoints and DOC-23's autonomy signals
continuously — that's read-only and satisfies "proactive." What it does with a detected stall is
where the "no acting without an explicit call" rule bites.

> **OWNER DECISION 1 (proposed default — confirm or amend).** `tm manager`'s proactive loop is
> **notify-only by default**: when it detects a stall/escalation-worthy condition, it surfaces a
> notification (portfolio digest entry, channel message, `GET /api/v1/manager/escalations` row) —
> it never calls a mutating endpoint (`kill`, `resume`, `decommission`, `set-status`,
> `answer_decision`) on its own initiative. Any actual intervention requires either (a) the user
> explicitly acting on the notification (a deliberate follow-up call, satisfying DOC-35 §11's
> "explicit call" requirement), or (b) an **opt-in intervention tier** the operator explicitly
> enables per-project/per-category (phase 3, §6) — e.g. "auto-resume sessions that idle-stalled for
> >30 min with no pending decision," itself a config the user turns on, not a default behavior.
> This keeps phase-3 "proactive monitoring" entirely within the letter of DOC-35 §11's ban on
> unprompted action while still delivering the "agentic oversight" epic #2109 promises. See §7 Q1.

---

## 3. Architecture

### 3.1 Manager as a daemon-owned component

`tm manager` is a new module inside the existing `tm` daemon (`crates/trusty-mpm/src/daemon/`),
not a separate process — following the same "daemon as source of truth" principle DOC-35 §1.3
established for the control plane. A `ManagerState` sits alongside `DaemonState` (the same struct
`daemon/managed_routes/proxy/mod.rs` already threads through for L2), holding: a handle to the
portfolio poll loop (§6), a reference to the configured `trusty_common::inference::InferenceAdapter`
(§3.3), and the portfolio `trusty-memory` palace handle (§3.4).

### 3.2 API-first surface: `/api/v1/manager/*`

Every verb below is a daemon HTTP endpoint first, a CLI/channel client second — same ordering
DOC-35 §3 enforced ("API-first build ordering... no slice ships a CLI verb or TUI pane ahead of
the endpoint it calls"). Proposed verb set (confirm/amend per §7 Q2):

```
GET  /api/v1/manager/status
    # Deterministic cross-project rollup: composes #2108's per-project
    # `GET /api/v1/projects/{name}/status` (DOC-35 §4.1) across every registered
    # project. NO LLM call — this is a pure aggregation, but it lives here (not
    # #2108) because DOC-35 §11 scopes "reason across MULTIPLE projects" to #2109,
    # regardless of whether that reasoning happens to be non-inferential.

GET  /api/v1/manager/digest?scope=portfolio|project:<name>
    # LLM-authored prose narrative — "what's going on right now" — built by
    # feeding /status's rollup plus each in-scope session's `activity.rs`
    # summary/pending_decision (already-deterministic-or-opt-in-LLM fields per
    # DOC-35 §11 row 4) to the configured InferenceAdapter (§3.3). Deterministic
    # fallback (a templated bullet list from /status) when no adapter is
    # configured, mirroring DOC-16 D1's "clearly marked fallback" rule.

POST /api/v1/manager/chat
    # Body: { conversation_key: string, message: string }.
    # Conversation-keyed (same shape as SessionProxy's focus-map keying,
    # `client/proxy.rs`) — a channel-agnostic chat turn against the portfolio
    # manager persona. Tool-calls out to #2108's read endpoints and, for
    # session-directed replies, to L2's SessionProxy inject/summarize (§3.5) —
    # never a direct tmux/session mutation.

POST /api/v1/manager/route-task
    # Body: { text: string }.
    # Runs DOC-22's deterministic resolver primitive (§13 Q1's #2108 substrate
    # API) for candidate scoring, then — because #2109 owns disambiguation —
    # resolves ties/low-confidence cases via judgment (an LLM call when the
    # resolver returns `needs_disambiguation()`, or a direct pass-through when
    # confidence is unambiguous) and returns a resolved `{ project, confidence,
    # rationale }`. Does NOT launch a session itself; a follow-up explicit call
    # (chat turn or `tm sessions launch`, DOC-35 §3.2) does that — keeping this
    # endpoint's output advisory, satisfying §2.2's "no acting without an
    # explicit call."

GET  /api/v1/manager/escalations
    # The notify-only surface from §2.2 OWNER DECISION 1 — a list of
    # detected stall/escalation conditions across the portfolio, each carrying
    # enough context (project, session, DOC-23 confidence signal if any, age)
    # for the user or a channel binding to act on explicitly. Read-only.
```

**Deliberately excluded from this surface (per §2.1):** no `POST` that mutates a Deliverable, sets
an autonomy tier, or kills/resumes/decommissions a session directly — those remain #2108/L2 calls
the manager's chat loop or CLI *invokes explicitly*, never a manager-owned mutating verb.

### 3.3 LLM calls via `trusty_common::inference`

All inference calls (`/digest`'s narrative pass, `/chat`'s conversational loop, `/route-task`'s
disambiguation judgment) go through `trusty_common::inference::InferenceAdapter`
(`crates/trusty-common/src/inference/adapter.rs`) — the unified adapter trait from epic #2400,
not a bespoke client. This is a deliberate target: issue #2411 is retiring `tm`'s own
`core/sm/providers/bedrock.rs` duplicate onto this same commons layer, so `tm manager` builds
directly against the surface that will be canonical rather than against a client already flagged
for removal. Concretely: `Configurator`/`provider_for` (`inference::configurator`) resolves the
configured provider at daemon startup; each manager call issues one `InferenceAdapter::chat`
request per DOC-16 D1's existing "one LLM call per operation, deterministic fallback on failure"
pattern already established for `activity.rs`'s `summary` field.

### 3.4 Memory via `trusty-memory` palaces

Per-project palaces already exist as the natural home for project-scoped memory. This spec
proposes one **additional portfolio palace** — a palace scoped to `tm manager` itself, not to any
one project — holding: digest history (for "what changed since the last digest" framing),
escalation dispositions (what the user did when notified, feeding future confidence), and
chat-session turns for the portfolio conversation (via the existing `chat_session_*` MCP surface).
Per-project palaces remain the source of truth for project-scoped facts; the portfolio palace never
duplicates project state, only manager-level observations *about* the portfolio. See §7 Q3 for the
naming/provisioning question this raises.

### 3.5 Channel bindings reuse the `ManagedBackend`/`SessionProxy` seam

`tm manager`'s external-channel binding (phase 4, §6) does **not** reinvent the Telegram/Slack
wiring #1440 already shipped. It reuses the same architectural pattern
`daemon/managed_routes/proxy/backend.rs`'s `DirectManagedBackend` establishes: a thin backend
trait implementation talking to daemon-local state (here, `ManagerState` instead of
`SessionManager`), wired through the same curl-testable local-HTTP-first discipline
`daemon/managed_routes/proxy/mod.rs` documents ("exercise this entire state machine with `curl`
against the daemon before ever wiring up a Telegram bot token"). Where L2's `SessionProxy` keys
conversations to a *focused session*, L3's channel binding keys conversations to the *portfolio
manager persona* — a distinct bot identity/chat surface from L2's, so a user is never confused
about whether they're talking to one session or to the whole portfolio (see §7 Q7). When the
manager's chat loop needs to talk to a specific session (e.g., "ask the trusty-search session for
a status update"), it calls into L2's existing `SessionProxy::inject`/`summarize` rather than
building a parallel path — L3 is a *consumer* of L2's single-session primitives, per §1's point 5.

---

## 4. Local-testability bar

Bob's standing principle — hermetic, no external credentials required to exercise core behavior —
applies here exactly as it did to #1440's L2 proxy (`proxy/mod.rs`'s "curl-testable... before any
channel is connected") and to DOC-35's daemon-first design. Adapted for L3's inference-heavy
surface:

- **No channel/bot token required for any core behavior.** `/api/v1/manager/*` is fully
  operable via `curl` against a locally-running daemon; Telegram/Slack bindings (phase 4) are
  additive transports over the same endpoints, never a required dependency for `/chat`, `/digest`,
  `/status`, or `/route-task` to function.
- **Hermetic CI via the commons mock inference adapter.** `trusty_common::inference::test_support`
  ships exactly this: `ScriptedAdapter` (deterministic in-memory, always available with the
  `inference-client` feature) for unit/integration tests of the chat/digest/route-task loops, and
  `MockInferenceServer` (a real loopback HTTP mock, behind `axum-server`) for tests that need to
  exercise actual HTTP client mechanics without a live provider key. CI wires the manager's
  `Configurator` to `ScriptedAdapter` by default; no `OPENROUTER_API_KEY` or Bedrock credentials
  are needed for the test suite to pass.
- **Curl recipes for manual live verification** (illustrative — exact JSON shapes finalized at
  implementation time):

```bash
# Portfolio deterministic rollup (no LLM call)
curl -s http://127.0.0.1:7880/api/v1/manager/status | jq

# Portfolio digest (LLM-backed, deterministic fallback if no adapter configured)
curl -s 'http://127.0.0.1:7880/api/v1/manager/digest?scope=portfolio' | jq

# A chat turn against the portfolio manager
curl -s -X POST http://127.0.0.1:7880/api/v1/manager/chat \
  -H 'content-type: application/json' \
  -d '{"conversation_key":"local-test-1","message":"what needs my attention right now?"}' | jq

# Ambiguous NL routing — advisory only, does not launch anything
curl -s -X POST http://127.0.0.1:7880/api/v1/manager/route-task \
  -H 'content-type: application/json' \
  -d '{"text":"fix the flaky auth test"}' | jq

# Notify-only escalation surface
curl -s http://127.0.0.1:7880/api/v1/manager/escalations | jq
```

---

## 5. Layering

Explicit statement of what `tm manager` consumes from each adjacent layer/spec, so no work item in
§8 accidentally reimplements a primitive that already exists:

| From | Consumes | Never |
|---|---|---|
| **#2108 (`tm project`, DOC-35)** | The deterministic project/session registry, per-project `GET /status` (composed by `/manager/status`, §3.2), Deliverable/Milestone read + explicit `set-status` calls, session lifecycle verbs (`launch`/`kill`/`resume`/`decommission`) invoked explicitly from a chat turn or `route-task` follow-up | Reimplements #2108's registry, status computation, or Deliverable state machine; calls a mutating #2108 verb without an explicit driving call (§2.1) |
| **DOC-22's resolver (`project/resolver/mod.rs`)** | The deterministic 3-strategy confidence-scored matching primitive (`resolve_project`) as an input signal to `/route-task` | Reimplements name/URL/keyword matching — the primitive itself stays a #2108 substrate API per DOC-35 §13 Q1's SPLIT decision; #2109 owns only the disambiguation judgment layered on top |
| **L2 (`client/proxy.rs`'s `SessionProxy`)** | `inject`/`summarize` for single-session communication reached from a portfolio chat turn (§3.5); the `ManagedBackend` architectural pattern for its own channel bindings | Bypasses `SessionProxy` to talk to a session's tmux pane directly; forks a second single-session focus state machine |
| **DOC-23 (learned-autonomy auto-answer)** | Read-only: `DecisionAdjudicator` confidence/disposition signals as an input to escalation prioritization (§2.2, §6 phase 3) — "was this decision auto-answerable, and how confident" informs whether a stall is worth surfacing | Sets or evaluates an autonomy tier (T1–T4); calls `answer_decision` on the adjudicator's behalf — DOC-23's adjudicator remains the sole path from decision to auto-answer, unchanged (§2.1) |

---

## 6. Phasing proposal

Each phase is API-first (the endpoint ships before any CLI/channel client calls it), matching
DOC-35's build ordering discipline.

**Phase 1 — Read-only portfolio chat.**
`GET /manager/status`, `GET /manager/digest`, `POST /manager/chat` (read-only tool-calls: no
session mutation, no `route-task`). CLI: `tm manager status|digest|chat`. This alone delivers
epic #2109's "inference management of the full portfolio" and "inference-based summarization"
goals (items 1 and 4 in the epic text) without touching the "acts on a session" surface at all —
the lowest-risk slice to ship and dogfood first.

**Phase 2 — Task routing and disambiguation into sessions via L2.**
`POST /manager/route-task` ships; the chat loop gains the ability to *propose* (not silently
execute) a session launch/inject following a resolved route, requiring the user's explicit
confirmation in the same turn (satisfying §2.1's "explicit call" requirement conversationally,
not just via a separate API call). CLI: `tm manager route`.

**Phase 3 — Proactive monitoring and notifications.**
The background poll loop (§2.2) and `GET /manager/escalations` ship, notify-only by default. The
opt-in intervention-tier config (§2.2's "b" branch) is a phase-3 **follow-up**, not part of the
initial phase-3 slice — it ships only after notify-only has been dogfooded and the owner has
confirmed which categories (if any) are safe to auto-intervene on. CLI: `tm manager watch` (a
foreground tail of `/escalations`, useful before any channel binding exists).

**Phase 4 — External channel portfolio binding.**
Telegram first (reusing #1440's bot infrastructure with a distinct portfolio-scope persona/chat
surface, §3.5), Slack as a follow-on using the identical `ManagedBackend`-pattern seam. This is
last because it's the highest-blast-radius surface (a live external channel) and the one phase
that most benefits from phases 1–3 already being dogfooded internally via curl/CLI first.

---

## 7. Owner-decision checklist

1. **Proactive-vs-explicit-call resolution (§2.2).** Confirm the proposed default: `tm manager`'s
   stall/escalation detection is **notify-only** (surfaces via digest/escalations/channel message,
   never auto-mutates), with actual intervention gated behind either explicit user follow-up or an
   **opt-in** per-category intervention tier the operator turns on later (phase-3 follow-up, not
   initial scope). Confirm, or specify a different default.
   - **RESOLVED 2026-07-14:** notify-only default confirmed; opt-in per-category intervention tier
     deferred to the phase-3 follow-up (WI-13), gated on notify-only being dogfooded first.
2. **Verb set for `/api/v1/manager/*` (§3.2).** Confirm the proposed five endpoints (`status`,
   `digest`, `chat`, `route-task`, `escalations`), or amend — in particular, confirm `status`
   (deterministic cross-project rollup) belongs on the manager surface rather than as a new #2108
   fleet-wide endpoint, given DOC-35 §11 scopes "reason across MULTIPLE projects" to #2109
   regardless of whether the reasoning is inferential.
   - **RESOLVED 2026-07-14:** five-endpoint surface (`status`, `digest`, `chat`, `route-task`,
     `escalations`) confirmed as proposed; `status` stays on the manager surface per DOC-35 §11's
     scoping, not moved to #2108.
3. **Portfolio `trusty-memory` palace (§3.4).** Confirm a single new portfolio-scoped palace
   (holding digest history, escalation dispositions, and portfolio chat turns) is the right shape,
   and confirm a naming/provisioning convention (e.g. auto-created at daemon startup vs. explicit
   `tm manager init`) — distinct from every per-project palace, which remains untouched.
   - **RESOLVED 2026-07-14:** single portfolio-scoped palace confirmed; provisioning convention is
     auto-created at daemon startup (consistent with §3.1's "daemon as source of truth"
     principle), not a separate explicit `tm manager init` step.
4. **Phasing (§6).** Confirm phase boundaries and ordering (1 read-only chat/digest → 2 task
   routing → 3 proactive monitoring → 4 external channel), or flag any phase that should move
   earlier — e.g., whether a Telegram binding for phase-1 read-only digests (lower risk than a
   full portfolio persona) should be pulled into phase 1 rather than held for phase 4.
   - **RESOLVED 2026-07-14:** phase boundaries and ordering confirmed as proposed (1 → 2 → 3 → 4);
     no phase pulled earlier — Telegram binding stays in phase 4.
5. **Disambiguation UX surface.** DOC-35 §13 Q1 resolved that acting on ambiguous NL routing is
   #2109's; confirm that lives entirely inside the chat surface (`POST /manager/chat`,
   `POST /manager/route-task`) with no separate CLI-only disambiguation prompt required for v1, or
   specify that a scriptable `tm manager route` CLI (§6 phase 2) needs its own non-chat
   confirmation UX (e.g. `--yes` flag vs. an interactive picker) for headless/CI use.
   - **RESOLVED 2026-07-14:** disambiguation lives entirely within the chat surface
     (`POST /manager/chat`, `POST /manager/route-task`) for v1; no separate CLI-only confirmation
     UX required initially.
6. **Autonomy-tier consumption, read-only confirmation.** Confirm `tm manager` only ever *reads*
   DOC-23's adjudication confidence/disposition as an escalation-prioritization signal (§5) and
   never requests, suggests, or triggers a tier change on the user's behalf — or specify whether a
   future phase should let the manager *propose* (never apply) a tier adjustment based on observed
   patterns, distinct from DOC-23's own learned-autonomy loop.
   - **RESOLVED 2026-07-14:** read-only confirmed — `tm manager` only reads DOC-23's adjudication
     confidence/disposition signals for escalation prioritization; it never requests, suggests, or
     triggers an autonomy-tier change.
7. **Portfolio channel identity vs. L2 session identity (§3.5, §6 phase 4).** Confirm the phase-4
   Telegram/Slack binding should present as a **distinct bot persona/chat surface** from #1440's
   per-session L2 bot, so users are never ambiguous about whether they're talking to one session
   or the whole portfolio — or specify that a single bot identity should serve both L2 and L3
   roles, disambiguated some other way (e.g. a `/portfolio` command prefix within the same chat).
   - **RESOLVED 2026-07-14:** distinct bot persona/chat surface confirmed for the phase-4
     portfolio binding, separate from #1440's per-session L2 bot.

---

## 8. Proposed work items (unfiled — pending owner review of this spec)

No GitHub issues are filed for these; per the "resolve then file" convention DOC-35 §8 and §12
both followed, filing happens after §7's decisions are resolved. Grouped by phase (§6).

| # | Phase | Title |
|---|---|---|
| WI-1 | 1 | `/api/v1/manager/*` daemon route skeleton + `ManagerState` wiring (no endpoints active yet — the scaffold §7 Q1–Q2 decisions land into) |
| WI-2 | 1 | `GET /manager/status` — deterministic cross-project rollup composing #2108's per-project status endpoint |
| WI-3 | 1 | `GET /manager/digest` — LLM-authored portfolio/per-project narrative via `trusty_common::inference`, deterministic fallback |
| WI-4 | 1 | `POST /manager/chat` (read-only tool-calls only) — conversation-keyed portfolio chat loop |
| WI-5 | 1 | Portfolio `trusty-memory` palace provisioning + digest/chat-turn read-write wiring |
| WI-6 | 1 | `tm manager status\|digest\|chat` CLI (API-first, ships after WI-2–WI-4) |
| WI-7 | 1 | Hermetic CI suite for chat/digest using `trusty_common::inference::test_support::ScriptedAdapter`/`MockInferenceServer` |
| WI-8 | 2 | `POST /manager/route-task` — DOC-22 resolver primitive input + #2109-owned disambiguation judgment |
| WI-9 | 2 | Chat-loop session launch/inject proposal-and-confirm flow (routes into #2108 launch + L2 `SessionProxy`) |
| WI-10 | 2 | `tm manager route` CLI (confirm UX per §7 Q5) |
| WI-11 | 3 | Background stall/escalation poll loop over #2108 activity/status + DOC-23 confidence signals |
| WI-12 | 3 | `GET /manager/escalations` notify-only surface + `tm manager watch` CLI tail |
| WI-13 | 3 (follow-up) | Opt-in per-category intervention-tier config (gated on §7 Q1 confirmation and phase-3 dogfood) |
| WI-14 | 4 | Telegram portfolio channel binding — distinct persona, `ManagedBackend`-pattern seam (§7 Q7) |
| WI-15 | 4 | Slack portfolio channel binding — same seam, follow-on to WI-14 |

---

## 9. References

- **Epic #2109** — `tm manager`, this epic.
- **Epic #2108** (CLOSED) — `tm project`, the deterministic substrate this spec builds on.
- **DOC-35** — `docs/specs/tm-project-control-plane.md` — §1.1 (three-layer model, quoted
  verbatim in §1 above), §10 (Deliverable/Milestone data model, consumed read/write via explicit
  calls only), §11 (boundary contract, restated normative in §2), §13 (resolver-split and
  learned-autonomy-ownership decisions this spec conforms to).
- **Issue #1440** — channel-agnostic SM proxy (L2), `crates/trusty-mpm/src/client/proxy.rs`,
  `crates/trusty-mpm/src/daemon/managed_routes/proxy/` — the seam §3.5's channel bindings reuse.
- **DOC-33** — One Session Manager, `docs/specs/one-session-manager.md` — adjacent (session
  lifecycle consolidation), not owned by this spec.
- **DOC-23** — Learned-Autonomy Auto-Answer, `docs/specs/learned-autonomy-auto-answer.md` —
  adjacent, consumed read-only per §5.
- **Epic #2400 / Issue #2411** — `trusty_common::inference` adapter layer
  (`crates/trusty-common/src/inference/`), the LLM call target per §3.3; #2411 retires `tm`'s
  duplicate Bedrock client onto this same layer.
- **Multi-Repo Session Routing (DOC-22)** — `crates/trusty-mpm/src/project/resolver/` — the
  deterministic matching primitive §3.2's `route-task` and §5 consume without reimplementing.
- **Code referenced:**
  - `crates/trusty-mpm/src/client/proxy.rs` — `SessionProxy`, `ManagedBackend`, `FocusTarget`,
    `ActivityDigest` — the L2 pattern §3.5 layers on top of.
  - `crates/trusty-mpm/src/daemon/managed_routes/proxy/{mod.rs,backend.rs,tests.rs}` — the local
    HTTP surface for L2, the architectural template for §3.2's manager routes.
  - `crates/trusty-mpm/tests/proxy_routes.rs` — the curl-facing HTTP contract test pattern this
    spec's §4 local-testability bar follows.
  - `crates/trusty-mpm/src/telegram/focus.rs` — existing per-session Telegram focus wiring, the
    precedent §6 phase 4 and §7 Q7 reference.
  - `crates/trusty-common/src/inference/{adapter.rs,configurator,test_support}` — the
    `InferenceAdapter` trait, `Configurator`/`provider_for`, and `ScriptedAdapter`/
    `MockInferenceServer` doubles §3.3 and §4 target.
  - `crates/trusty-mpm/src/project/resolver/mod.rs` — `resolve_project`, `DISAMBIGUATION_FLOOR`,
    `needs_disambiguation()` — the deterministic primitive §3.2's `route-task` calls into.

---

## 10. Change log

- **2026-07-13 (v1)** — Initial draft (DOC-36, `SPEC-TMMGR-01~draft`). Vision, DOC-35 §11 boundary
  restatement plus the proactive-vs-explicit-call resolution (§2.2), API-first architecture
  proposal (`/api/v1/manager/*`), local-testability bar, layering table against #2108/DOC-22/L2/
  DOC-23, a four-phase delivery proposal, a seven-item owner-decision checklist, and an unfiled
  15-item work-item table. Opened for owner review; no child issues filed.
- **2026-07-14 (v2)** — Approved by owner (DOC-36, `SPEC-TMMGR-01~approved` … `-06~approved`). All
  7 items in §7's owner-decision checklist resolved as proposed (see per-item RESOLVED annotations
  in §7). Implementation green-lit; child issues to be filed and tracked on GitHub milestone 10
  (Chat-Based Project Manager) under epic #2109.
- **2026-07-14 (v3)** — Added §1.1 Primary user story and owner's refinement: single-primary-task-plus-secondaries
  project model; project↔repo↔session cardinality (1..N repos per project, exactly 1 repo per
  session, N sessions per repo, repo count orthogonal to session tracking); full manager-lifecycle
  responsibilities (R3: spawn, tagged-memory goal tracking, completion tracking, confidence-gated
  escalation, context/cost monitoring with pause/clear/resume, cross-session and cross-project
  communication); gh-project/ticketing-agent tooling (R4); portfolio-wide urgency/priority-based
  user-communication prioritization (R5); **deference to harness-native "keep going" signaling
  with verify-and-steward oversight (R6)** — the manager observes, verifies against tagged goals,
  and intervenes on misalignment or cost/context pressure only, never duplicating the harness's
  continuation engine. Reconciliation note updated to reflect that R6 largely resolves the
  tension between cost/context-triggered pause/clear/resume and §2.1/§2.2's notify-only default
  (the manager's default is observe-and-verify, not proactive intervention). Flagged two remaining
  open points (new tool surfaces in §3/§5; per-session goal memory location) for later
  reconciliation — does not reopen or amend §7's resolved items.
