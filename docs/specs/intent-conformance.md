# DOC-15 — Intent / Method Conformance (two-gate ticket/spec verification)

**Status:** Draft
**Subsystem:** cross-crate — trusty-mpm (front gate) + trusty-review (back gate) + trusty-common (shared resolver)
**Owner:** Engineering (trusty-tools platform)
**Last-updated:** 2026-06-16
**Spec ID:** `SPEC-CONFORMANCE-01~draft` (DOC-15)
**Epic:** epic: intent/method conformance (umbrella issue # to be linked by the PM)
**Builds on:**
- trusty-review pipeline + context-source registry (`crates/trusty-review/src/pipeline/`, `src/integrations/context/`)
- trusty-mpm driver autonomy policy (`crates/trusty-mpm/src/driver/policy.rs`) + managed-session spawn (`crates/trusty-mpm/src/daemon/managed_routes/lifecycle.rs`)
- trusty-common shared `tickets` backend + `github_path` (`crates/trusty-common/src/tickets/`, `src/github_path.rs`)
- SLD (`spec-linked-docs` skill) for spec-ID linkage in docstrings; `spec-authoring` for the spec ID grammar
**Cross-ref:** PR→ticket extraction in tga (`crates/trusty-git-analytics/src/collect/ticket.rs`),
the `~80% auto / ~20% escalate` autonomy operating model
(`docs/trusty-mpm/spec/SESSION_MANAGER_DRIVER_AGENT.md` §4, `docs/trusty-mpm/spec/AUTONOMY_POLICY.md`),
and issues **#1269** (headless-spawn blocker — soft-blocks the front gate's auto-spawn path).

> **Scope note.** This is a **behavior contract + architecture requirements** spec for a
> new **intent/method conformance** capability: two verification gates that check an
> implementation's *method* (approach/technique/constraint) against the *intent* declared
> in its ticket and any linked spec. It specifies *what the gates do*, the normative
> decision matrix, the shared intent-source-resolver contract, and the integration seams in
> each crate. It does **not** implement anything; the PR carrying this doc opens **no** Rust
> changes. Implementation is decomposed into child issues (§13).

---

## 0. Terminology

| Term | Meaning |
|---|---|
| **Intent** | *What* and *how* the work was meant to be done — distinct from *whether the code is correct*. Lives in the ticket body and/or a linked spec section. |
| **Method** | The specific approach/technique/constraint the intent prescribes (e.g. "use cursor-based pagination", "no new dependency", "reuse the existing `ContextSource` trait", "gate behind a feature flag"). |
| **Front gate (FRONT)** | A **pre-work** check in **trusty-mpm**, run *before* a session begins implementing a ticket: `ticket = spec`. Verifies the planned method against ticket + spec and **escalates only on divergence**. |
| **Back gate (BACK)** | A **post-implementation** check in **trusty-review**, run during review of a PR: `ticket (spec) = implementation`. Flags when the code contradicts an explicit method the ticket/spec stated. |
| **Intent-source resolver (ISR)** | The shared component (in trusty-common) that, given a PR/diff or a ticket-id, resolves the **ticket method** and the **spec method**, applies precedence, and returns a single `ResolvedIntent`. Consumed by both gates. |
| **Divergence** | The implementation (or planned method) contradicts an **explicitly stated** method/approach/constraint in the ticket or spec. |
| **Gap** | Neither ticket nor spec states a method for the work in question (no intent to conform to). |
| **Conformance finding** | A back-gate review finding whose category is *method-conformance*, distinct from a correctness finding. |

**Precedence (NORMATIVE, fixed):** **ticket > spec.** When a ticket explicitly specifies a
method, it wins over a spec that says otherwise; a spec that conflicts with the ticket is
treated as **stale** and downgraded to advisory. Rationale: the ticket is the most recent,
most specific statement of intent for *this* unit of work; specs drift.

---

## 1. Purpose & Scope

### 1.1 What we are building

Two narrow, conservative verification gates plus the shared resolver that feeds them:

- **FRONT (trusty-mpm):** before a managed session starts coding a ticket, resolve the
  ticket+spec method and check the *planned* approach. If ticket and spec agree, or the
  ticket unambiguously specifies the method, **auto-proceed** (no human in the loop). Only
  on a ticket/spec **mismatch** or a **method gap that matters** does the gate **escalate
  to human judgment before any code is written**. This ties directly to the established
  ~80% auto / ~20% escalate autonomy operating model.

- **BACK (trusty-review):** during PR review, after the implementation exists, resolve the
  ticket+spec method and check the *actual* implementation. Posture is **conservative**:
  only emit a `method-conformance` finding when the ticket/spec states an **explicit**
  method/approach/constraint that the code **contradicts**. Minimize false positives — a
  gap is advisory, never a blocking finding.

- **ISR (trusty-common):** one resolver both gates call so ticket+spec resolution and the
  precedence rule are implemented **once**, not duplicated across two crates.

### 1.2 Goals

- **G1 — Two gates, one contract.** FRONT and BACK share the same `ResolvedIntent` and the
  same decision matrix (§4), so "method conformance" means the same thing pre- and post-work.
- **G2 — Escalate only on divergence (FRONT).** The default is auto-proceed; escalation is
  the ~20% exception, reserved for genuine ticket/spec mismatch or a material method gap.
- **G3 — Conservative, low-false-positive flagging (BACK).** A conformance finding requires
  an *explicit* contradicted method; ambiguity and gaps never block.
- **G4 — No duplicated resolution.** Ticket fetch, spec resolution, and precedence live in
  the shared ISR in trusty-common; both crates consume it.
- **G5 — Reuse existing seams.** Build on trusty-review's `ContextSource` registry and
  verdict floor; trusty-mpm's `AutonomyTier`/`evaluate_autonomy_tier` policy and the
  `pending_decision` escalation channel; trusty-common's `tickets` backend and `github_path`.

### 1.3 Non-goals (see §12)

Implementing the gates in Rust; building an LLM judge from scratch (gates reuse the existing
review LLM / a small classification call); a full SLD CI traceability enforcement pass (the
ISR *reads* SLD links but does not enforce the four-status model); per-symbol spec coverage
metrics; auto-fixing divergences; gating non-ticketed work (no ticket → no intent → no gate).

---

## 2. Background: current state (investigation findings)

These are the verified seams the gates attach to. File:symbol references are load-bearing.

### 2.1 trusty-review (BACK gate target)

- **Findings have no category taxonomy today.** `Finding.kind` is a free-form `String`
  (`models/mod.rs:163`) populated from the LLM finding's `title` (`parser.rs:288`). The only
  structured dimension is `severity → Effort` (`parser.rs:277`). A `method-conformance`
  category is genuinely new: it needs a `category` field threaded through `LlmFinding`
  (`parser.rs:67`), `Finding` (`models/mod.rs:156`), `convert_llm_finding` (`parser.rs:277`),
  and the prompt JSON schema (`prompt.rs`, `prompt_templates.rs`).
- **Context-source registry is the clean insertion seam.** `trait ContextSource`
  (`integrations/context/mod.rs:324`: `name`/`is_enabled`/`mode`/`async gather`) + the
  fan-out `gather_external_context` orchestrator (`orchestrator.rs:56`, bounded concurrency,
  per-source timeout, fail-open). New sources register in **one line** at
  `pipeline/runner_context.rs:203` (today: `JiraSource`, `ConfluenceSource`,
  `GithubIssuesSource`). A conformance source registers here.
- **PR→issue linkage today is fuzzy keyword search, not explicit.** `GithubIssuesSource`
  builds `repo:… is:issue <keywords>` (`github_issues.rs:403`) — it does **not** parse
  `Closes #N`. The ISR adds explicit linkage.
- **Verdict enum** (`models/mod.rs:43`): `Approve` → `APPROVE`, `ApproveWithReservations`
  → `APPROVE*`, `RequestChanges` → `REQUEST_CHANGES`, `Block` → `BLOCK`, `Unknown` →
  `UNKNOWN`. Ordering `verdict_ord` (`grade.rs:373`): APPROVE=0 < APPROVE*=1 <
  REQUEST_CHANGES=2 < BLOCK=3 < UNKNOWN=4; `stricter_of` (`grade.rs:359`).
- **Grade clamp / floor logic** (`grade.rs`): `derive_verdict_with_grade` (`grade.rs:410`,
  the runner's entry via `runner_helpers.rs:60`); `derive_verdict` (`grade.rs:178`);
  `severity_floor` (`grade.rs:313`, four-tier: any `Effort::High` → BLOCK; ≥2 Medium >0.80
  conf → REQUEST_CHANGES; 1 → APPROVE*; else APPROVE); `clamp_grade_to_verdict`
  (`letter_grade.rs:265`). The recent High-effort carve-out is `is_substantive`
  (`grade.rs:268`): a non-refuted `Effort::High` finding is retained in the floor input even
  below `FLOOR_COUNT_MIN_CONFIDENCE = 0.50` (`grade.rs:151`). This is the precedent for
  category-specific floor treatment (§5.4).
- **MCP entrypoints**: `review_pr` (`mcp/tools.rs:50`, inputs owner/repo/pr) and
  `review_diff` (`tools.rs:90`, inputs diff/context); both hard-wired dry-run; `run_review`
  is the pipeline (`runner.rs:121`), context gathered at Step 5 (`runner.rs:249`).

### 2.2 trusty-mpm (FRONT gate target)

- **The autonomy policy engine is complete, tested, and has zero callers.** `policy.rs`
  exports `AutonomyTier {T1,T2,T3,T4}` (`policy.rs:209`), `classify_tier` (`policy.rs:319`),
  and the single entry point `evaluate_autonomy_tier(ctx, signals) -> AutonomyDecision`
  (`policy.rs:357`) returning `Disposition::{AutoAccept{reason}, Escalate{reason}}`
  (`policy.rs:247`). Inputs: `ActionContext` (`policy.rs:159`: `pending_decision`,
  `change_class`, `correlation`, `prior_rejections`) and `GuardrailSignals` (`policy.rs:96`:
  `review`, `ci`, `search_consistent`, `memory_consistent`, `scope`). `first_failing_signal`
  (`policy.rs:472`) names the escalation cause. The FRONT gate is a **new caller** of this
  engine.
- **Work begins at `spawn_managed`** (`daemon/managed_routes/lifecycle.rs:66`),
  transport-agnostic, shared by HTTP (`managed_routes/mod.rs:339`) and MCP `session_new`.
  Step 2 creates the `SessionRecord` (`lifecycle.rs:119`); **Step 3
  `adapter.spawn(tmux_name, path, task)` (`lifecycle.rs:153`) is where the agent actually
  begins coding.** The FRONT gate inserts **between Step 2 and Step 3** (or right after Step
  0 to gate before workspace side-effects).
- **Ticket content reaches the session via `build_task`** (`bin/tm/commands/ticket/mod.rs:63`),
  which flattens `Issue {number,title,body,...}` (`ticket/system.rs:50`) into the task prompt
  (`SessionRecord.task`). There is **no structured spec linkage today** — `Issue.body` is the
  only ticket content.
- **Escalation channel to reuse:** `SessionRecord.pending_decision` + `proposed_default`
  (`record.rs:158`), surfaced via `GET …/activity` (`mod.rs:235`), MCP `session_status`,
  supervisor metrics, and the `tm` CLI; resolved by a human via `POST …/answer` →
  `answer_decision` (`manager.rs:280`). The supervisor never auto-answers
  (`poller.rs:54`). On `Disposition::Escalate`, the FRONT gate sets `pending_decision` +
  `proposed_default` and **withholds `adapter.spawn`**, leaving the session
  `AwaitingApproval` (`core/session.rs:41`). `OverseerDecision::FlagForHuman`
  (`core/overseer.rs:43`) is the established escalation vocabulary the disposition maps onto.

### 2.3 trusty-common (ISR home)

- **Both gate crates already depend on trusty-common**; neither depends on the other, on
  trusty-agents-common, or (mpm) on tga. trusty-common is the only shared home.
- **Ticket client already lives here**, behind the `tickets` feature: `Backend` trait with
  `async fn get_issue(&self, id) -> Result<Issue>` (`tickets/api/backends/mod.rs:106`),
  GitHub/JIRA/Linear backends, shared `Issue` model (`tickets/api/models.rs:72`, fields
  `number`/`title`/`body`/`html_url`). GitHub backend uses reqwest + `GITHUB_TOKEN` bearer
  (`tickets/api/backends/github/client.rs:22`).
- **Owner/repo derivation**: `github_path::{parse_github_path, derive_github_path}`
  (`lib.rs:370`). **LLM surface**: the `chat` `ChatProvider` (`lib.rs:31`). **PR→ticket
  regexes** live in tga (`collect/ticket.rs:120,155`: `is_ticketed`, `extract_ticket_id` —
  `Closes #N`, JIRA/Linear, ADO) and should be **lifted into trusty-common** so both gates
  parse linkage without pulling tga's git2/rusqlite stack.
- **No spec-resolution mechanism exists anywhere** (changed-file/symbol → spec section): no
  Rust traceability test, no CI spec check. `SPEC-…`/`DOC-N §X` references in code are
  hand-written doc-comment pointers only. The spec-resolution half of the ISR is greenfield;
  it aligns to the `spec-linked-docs` (SLD) docstring convention.

### 2.4 SLD spec-linkage (how code declares its spec)

Per the SLD skill, Rust code declares spec linkage in **rustdoc**, not comments:
module-level `//! # Spec References` with `[\`SPEC-{SUBSYSTEM}-{NN}~v{rev}\`](docs/specs/{file}.md#SPEC-…)`,
and function-level `/// # Spec References`. Spec sections carry a stable
`{#SPEC-{SUBSYSTEM}-{NN}}` anchor (the convention already used by DOC-13/DOC-14). The ISR's
spec-resolution leg parses these docstring references out of changed files to find the
governing spec section(s) — it does **not** invent linkage where none is declared.

---

## 3. The two-gate model (overview)

```
              ┌──────────────── intent-source resolver (ISR, trusty-common) ────────────────┐
              │  inputs: ticket-id  OR  PR/diff (→ extract ticket-id + changed files)        │
              │  → fetch ticket (tickets::Backend) → ticket method                           │
              │  → resolve spec section(s) (SLD docstring refs in changed files) → spec method│
              │  → apply precedence (ticket > spec) → ResolvedIntent                          │
              └───────────────┬──────────────────────────────────────────┬──────────────────┘
                              │                                          │
        FRONT (trusty-mpm, PRE-work)                       BACK (trusty-review, POST-impl)
        ticket = spec                                      ticket (spec) = implementation
        before adapter.spawn (lifecycle.rs:153)            ContextSource + method-conformance finding
        ESCALATE-ONLY-ON-DIVERGENCE                        CONSERVATIVE (explicit contradiction only)
                  │                                                   │
        Disposition::AutoAccept → spawn                    Verdict floor (severity → Effort)
        Disposition::Escalate  → pending_decision,         method-conformance finding → REQUEST_CHANGES
                                  withhold spawn           gap → ADVISORY (no verdict impact)
```

Both gates consume the **same** `ResolvedIntent` and apply the **same** decision matrix
(§4). They differ only in *what they compare the intent against* (planned method vs. actual
implementation) and *what they do on each outcome* (escalate-before-coding vs. emit a
review finding).

---

## 4. Decision matrix (NORMATIVE behavior) {#SPEC-CONFORMANCE-01~draft}

**ID:** SPEC-CONFORMANCE-01~draft
**Status:** Draft

This is the single normative contract both gates implement. **Precedence is fixed:
ticket > spec.**

### 4.1 The matrix

| # | Condition | Resolved comparison | FRONT (trusty-mpm) | BACK (trusty-review) |
|---|---|---|---|---|
| **M1** | Ticket explicitly specifies a method | compare subject to **ticket** method | agree → **AUTO-PROCEED**; planned method diverges → **ESCALATE** | impl matches → no finding; impl diverges → **`method-conformance` finding → REQUEST_CHANGES** |
| **M2** | Ticket silent **and** changed code is spec-linked (SLD ref present) | compare subject to **spec** method | agree → **AUTO-PROCEED**; diverges → **ESCALATE** | impl matches → no finding; impl diverges → **`method-conformance` finding → REQUEST_CHANGES** |
| **M3** | Neither ticket nor spec specifies a method (**gap**) | — | **AUTO-PROCEED** (no intent to conform to) | **ADVISORY** note only — never a blocking finding |
| **M4** | Ticket vs. spec **conflict** (both specify, they disagree) | **ticket wins**; spec marked **stale** | follow ticket → AUTO-PROCEED; spec stale → **ADVISORY** note, not an escalation | conform to **ticket**; spec conflict → **ADVISORY**; impl matching the stale spec but violating the ticket → **REQUEST_CHANGES** |
| **M5** | Subject diverges from the *specified* method (M1 or M2) | — | **ESCALATE** before coding | **REQUEST_CHANGES** |

### 4.2 Behavior Contract (WHAT)

- **Inputs (both gates):** a `ResolvedIntent` from the ISR (§6) — carrying
  `{ ticket_method: Option<Method>, spec_method: Option<Method>, precedence_winner, conflict:
  bool, stale_spec: bool }` — plus the **subject** being judged: for FRONT, the *planned
  method* derived from the ticket task + decomposition; for BACK, the *implementation* (the
  PR diff + changed files).
- **Outputs (FRONT):** a `Disposition` — `AutoAccept{reason}` or `Escalate{reason}` — mapped
  onto `trusty-mpm`'s `Disposition` (`policy.rs:247`). On `AutoAccept`, the session spawns;
  on `Escalate`, the session does not spawn and a `pending_decision` is written.
- **Outputs (BACK):** zero or more `Finding`s with `category = MethodConformance`. M1/M2/M5
  divergence → a finding whose `Effort` drives the verdict floor toward `REQUEST_CHANGES`;
  M3/M4-advisory → a low-`Effort`/advisory finding that does **not** raise the floor.
- **Preconditions:** the subject is **ticketed** (a ticket-id resolved). Non-ticketed work
  yields `ResolvedIntent::none()` → both gates no-op (FRONT auto-proceeds, BACK emits
  nothing).
- **Postconditions:** the decision is a pure function of `(ResolvedIntent, subject)` and the
  gate posture; the matrix is total (every input falls into exactly one of M1–M5).
- **Error conditions (fail-open):** if the ISR cannot resolve (ticket fetch fails, spec
  unreadable), it returns `ResolvedIntent::unresolved{reason}`. **FRONT auto-proceeds**
  (never block work on resolver failure — matches review's fail-open `ContextSource`
  contract and the autonomy engine's `Unavailable`/`Unknown` tolerance). **BACK emits no
  conformance finding** (a missing intent source never manufactures a false positive). The
  failure is logged (stderr) with the reason.

### 4.3 Rationale (WHY)

- **Ticket > spec** because the ticket is the most recent, most specific statement of intent
  for *this* unit of work; specs drift and a stale spec must not override a deliberate
  ticket decision. Treating a conflicting spec as *advisory stale* (M4) rather than an error
  keeps the gate from blocking on documentation lag.
- **FRONT escalate-only-on-divergence** realizes the ~80% auto / ~20% escalate operating
  model: the common case (ticket and spec agree, or the ticket is unambiguous) costs no
  human attention; only genuine ambiguity (mismatch / material gap) buys a human decision —
  and it buys it **before** code is written, when redirection is cheapest.
- **BACK conservative** because a method-conformance false positive is expensive: it erodes
  trust in the reviewer and trains authors to ignore it. Requiring an **explicit**
  contradicted method (M5), and downgrading gaps/conflicts to advisory (M3/M4), keeps
  precision high, mirroring review's existing bias toward high-confidence floors
  (`FLOOR_MIN_CONFIDENCE = 0.80`, `grade.rs:129`) and the High-effort retention carve-out
  (`is_substantive`, `grade.rs:268`).
- **Fail-open everywhere** because conformance is an *advisory overlay* on top of existing
  correctness review and existing autonomy gating — it must never be the reason work stalls
  or a PR is wrongly blocked.

### 4.4 Worked examples (non-normative, illustrative)

| Scenario | Matrix row | FRONT | BACK |
|---|---|---|---|
| Ticket: "use cursor-based pagination"; plan/impl uses offset | M1/M5 | ESCALATE before coding | `method-conformance` → REQUEST_CHANGES |
| Ticket silent; changed file links `SPEC-SEARCH-04` ("no blocking I/O in handler"); impl adds a blocking read | M2/M5 | ESCALATE | REQUEST_CHANGES |
| Ticket: "add a flag"; no method constraint stated | M3 | AUTO-PROCEED | advisory/none |
| Ticket: "add dep X"; spec says "no new deps" (stale) | M4 | follow ticket, AUTO-PROCEED, advisory note | conform to ticket; spec conflict advisory |
| Spec says "use trait T"; ticket silent; impl ignores T | M2/M5 | ESCALATE | REQUEST_CHANGES |

---

## 5. Gate semantics (per gate) {#SPEC-CONFORMANCE-02~draft}

**ID:** SPEC-CONFORMANCE-02~draft
**Status:** Draft

### 5.1 FRONT gate — verdict/escalation semantics (trusty-mpm)

**Behavior Contract (WHAT):**

- **Inputs:** the ticket-id + task prompt available at `spawn_managed`
  (`lifecycle.rs:66`); the `ResolvedIntent` from the ISR; the planned method derived from the
  task. An `ActionContext` is built from these (`policy.rs:159`).
- **Outputs:** `Disposition` (`policy.rs:247`). The gate maps the matrix outcome onto the
  existing autonomy decision:
  - **M1/M2 agree, M3 gap, M4 ticket-wins → `AutoAccept{reason}`** → Step 3
    `adapter.spawn` (`lifecycle.rs:153`) proceeds normally.
  - **M1/M2/M5 divergence → `Escalate{reason}`** → `adapter.spawn` is **withheld**;
    `SessionRecord.pending_decision` is set to the divergence reason and `proposed_default`
    to the conformant method (`record.rs:158`); the session sits `AwaitingApproval`
    (`core/session.rs:41`). The human resolves via `POST …/answer` → `answer_decision`
    (`manager.rs:280`), which un-blocks the spawn.
- **Composition with existing autonomy:** the conformance disposition is **combined** with
  the standard `evaluate_autonomy_tier` decision by taking the **stricter** outcome (escalate
  wins). The gate never *lowers* a tier escalation to auto-accept. A T4/destructive action
  still escalates regardless of conformance (`classify_tier`, `policy.rs:319`).
- **Escalation reason text** reuses the `first_failing_signal` style (`policy.rs:472`): a
  one-line human-readable cause ("ticket specifies cursor pagination; plan uses offset").
  The disposition may also map onto `OverseerDecision::FlagForHuman{summary}`
  (`core/overseer.rs:43`) for vocabulary consistency.
- **Error/fail-open:** ISR unresolved or non-ticketed → `AutoAccept{reason:"no intent
  source"}`; the gate never blocks work on its own failure (G2/§4.2).

**Rationale (WHY):** inserting at `spawn_managed` between record-creation and spawn is the
only seam that owns *both* the task text (intent) and the spawn trigger; reusing
`pending_decision` means escalations surface through every existing channel (activity API,
MCP `session_status`, supervisor, CLI) with zero new UI. Soft-blocked by **#1269** for the
fully-headless auto-spawn path — until #1269 lands, escalation degrades to the CLI-owned
launch / operator-confirm path.

### 5.2 BACK gate — verdict/finding semantics (trusty-review)

**Behavior Contract (WHAT):**

- **Inputs:** the PR (owner/repo/pr or diff) at `run_review` (`runner.rs:121`); the
  `ResolvedIntent` surfaced as a **new `ContextSource`** (§7.1) gathered at Step 5
  (`runner.rs:249`); the diff/changed files as the implementation subject.
- **Outputs:** zero or more `Finding`s with the **new** `category = MethodConformance`
  (§7.2). The reviewer LLM is instructed (prompt addition, §7.2) to emit a conformance
  finding **only** when an *explicit* ticket/spec method is *contradicted* by the diff (M5);
  otherwise none (M3) or advisory (M4).
- **Verdict mapping:** a `MethodConformance` finding carries a `severity → Effort` like any
  finding, so it flows through the existing floor (`severity_floor`, `grade.rs:313`;
  `derive_verdict`, `grade.rs:178`). A contradicted **explicit** method is emitted at an
  `Effort` that floors the verdict to **`REQUEST_CHANGES`** (not BLOCK — conformance is
  REQUEST_CHANGES-grade per the design decision, reserving BLOCK for correctness/safety).
  Advisory conformance notes (M3/M4) are emitted at low `Effort`/confidence so they do
  **not** raise the floor.
- **Conservative confidence floor:** a conformance finding must clear the existing
  `FLOOR_MIN_CONFIDENCE = 0.80` (`grade.rs:129`) to affect the verdict. Below that it is
  advisory. This is the primary false-positive guard (G3).
- **Error/fail-open:** ISR unresolved / no ticket / no spec link → the conformance
  `ContextSource` returns an empty section (fail-open, like every existing source,
  `orchestrator.rs:56`); **no conformance finding is manufactured**.

**Rationale (WHY):** modeling the back gate as a `ContextSource` + a finding category reuses
the entire existing pipeline (gather → prompt → parse → grade) with the documented
"one-line registration, zero runner changes" seam (`runner_context.rs:203`); routing
conformance through the *existing* severity floor (rather than a parallel verdict path)
keeps one verdict authority and inherits the high-confidence bias that minimizes false
positives.

**Rationale (WHY) — precedence wiring:** the ISR has already applied ticket > spec before the
finding stage, so the reviewer is told to check against `ResolvedIntent.precedence_winner`;
a stale-spec conflict (M4) is surfaced to the reviewer as advisory context, never as the
thing to fail against.

### 5.3 Implementing Modules

| Module | Role |
|--------|------|
| `trusty-mpm::daemon::managed_routes::lifecycle::spawn_managed` | FRONT gate insertion point (between record-create and `adapter.spawn`). |
| `trusty-mpm::driver::policy` (`evaluate_autonomy_tier`, `Disposition`) | Autonomy decision the FRONT disposition composes with (stricter-wins). |
| `trusty-mpm::session_manager::manager::answer_decision` | Human resolution of a FRONT escalation. |
| `trusty-review::integrations::context` (new `ConformanceSource`) | BACK gate: surfaces `ResolvedIntent` as review context. |
| `trusty-review::pipeline::{parser,grade}` (new `category` field) | BACK gate: distinguishes conformance findings; floors verdict. |
| `trusty-common::intent_source` (new, ISR) | Shared resolution + precedence (§6). |

---

## 6. Intent-source-resolver (ISR) contract {#SPEC-CONFORMANCE-03~draft}

**ID:** SPEC-CONFORMANCE-03~draft
**Status:** Draft

The ISR is the shared component in **trusty-common** (`intent-source` feature) that both
gates call. It resolves the ticket method and the spec method and applies precedence.

### 6.1 Behavior Contract (WHAT)

- **Inputs (one of):**
  - `IntentQuery::Pr { owner, repo, pr_number, diff, changed_files }` — used by BACK. The ISR
    extracts the ticket-id(s) from the PR (§6.3) and resolves spec links from `changed_files`.
  - `IntentQuery::Ticket { ticket_id, changed_files? }` — used by FRONT (it already has the
    ticket-id; `changed_files` may be empty pre-work, so spec resolution may key off the
    ticket body's spec references instead).
- **Outputs:** `ResolvedIntent`:
  ```
  ResolvedIntent {
    ticket: Option<TicketRef>,        // number, title, url, backend
    ticket_method: Option<Method>,    // extracted prescribed method, if any
    spec_section: Option<SpecRef>,    // SPEC-{SUBSYSTEM}-{NN}~{rev} + file + anchor
    spec_method: Option<Method>,      // extracted prescribed method, if any
    precedence_winner: Precedence,    // Ticket | Spec | None
    conflict: bool,                   // ticket and spec both specify and disagree
    stale_spec: bool,                 // spec downgraded to advisory under ticket precedence
    unresolved: Option<String>,       // fail-open reason, if resolution failed
  }
  Method { text: String, kind: MethodKind, source_excerpt: String }
  ```
  where `Method` is an extracted statement of approach/constraint (M-class), not free prose.
- **Precedence resolution (NORMATIVE):**
  1. both present + agree → `precedence_winner = Ticket` (or `Spec`; equivalent), `conflict=false`.
  2. both present + disagree → `precedence_winner = Ticket`, `conflict=true`, `stale_spec=true`.
  3. ticket only → `Ticket`. spec only → `Spec`. neither → `None` (gap).
- **Preconditions:** for `Pr`, the diff/changed_files are available; for `Ticket`, the
  ticket-id is well-formed.
- **Postconditions:** the result is deterministic given the same ticket content + spec files;
  the precedence rule is applied exactly once, centrally (no caller re-derives it).
- **Error conditions (fail-open):** any fetch/parse failure → `ResolvedIntent { unresolved:
  Some(reason), .. }` with all method fields `None`; callers treat this as "no intent" (§4.2).

### 6.2 Method extraction

- **Ticket method** is extracted from `Issue.body` (`trusty_common::tickets` `Issue`,
  `tickets/api/models.rs:72`). Extraction MAY use the `chat` `ChatProvider`
  (`trusty_common::chat`, `lib.rs:31`) with a fixed classification prompt ("does this ticket
  prescribe a specific method/approach/constraint? extract it verbatim or return none"), or a
  deterministic heuristic pass for obvious imperative-method phrasing. Extraction is
  **conservative**: ambiguous text → `None` (no method), never a hallucinated constraint.
- **Spec method** is extracted from the resolved spec section's **Behavior Contract /
  Rationale** prose (the `{#SPEC-…}` section the changed code links to, §6.4).

### 6.3 PR → ticket linkage (NORMATIVE)

- Extraction reuses the tga regexes **lifted into trusty-common**:
  `is_ticketed`/`extract_ticket_id` (`crates/trusty-git-analytics/src/collect/ticket.rs:120,155`)
  — matching `Closes/Fixes/Resolves #N`, JIRA/Linear `[A-Z]+-\d+`, ADO `AB#N`, and bare `#N`.
- **Sources, in precedence order:** (a) PR body `Closes #N` trailers; (b) commit-message
  trailers in the diff's commits; (c) the branch name (e.g. `fix/1325-…` → `#1325`). First
  explicit match wins; multiple tickets → resolve each and pick the one whose changed-file
  overlap is highest (documented heuristic; ties → lowest issue number).
- owner/repo derive from `trusty_common::github_path::parse_github_path` (`lib.rs:370`).

### 6.4 Spec resolution (changed file/symbol → spec section) (NORMATIVE)

- For each `changed_file`, the ISR parses **SLD docstring spec references** (per
  `spec-linked-docs`): module-level `//! # Spec References` and function-level
  `/// # Spec References` blocks containing `SPEC-{SUBSYSTEM}-{NN}~v{rev}` pointing at
  `docs/specs/{file}.md#SPEC-…`.
- The referenced anchor is resolved to the governed section in `docs/specs/` (the
  `{#SPEC-{SUBSYSTEM}-{NN}}` heading). That section's Behavior Contract + Rationale is the
  spec-method source (§6.2).
- **The ISR does not invent linkage.** A changed file with no SLD reference yields no spec
  method (gap on the spec axis). This is greenfield — no spec-resolution mechanism exists
  today (§2.3); the ISR introduces it but only as a *reader* of declared links, not a
  traceability enforcer.
- **Revision awareness:** if code references `~v1` but the section is `~v2`, the spec method
  is still resolved (from the current section) and the ISR flags `stale_spec`-adjacent
  metadata; OUTDATED enforcement is out of scope (non-goal §1.3).

### 6.5 Rationale (WHY)

A single resolver in the one crate both gates already depend on (trusty-common) is the only
placement that avoids a new cross-crate edge (trusty-agents-common and tga are both
disqualified, §2.3). Centralizing precedence guarantees FRONT and BACK can never disagree
about which intent source wins. Reusing the in-crate `tickets` backend, `github_path`, and
`chat` surfaces means the ISR composes existing, already-authed primitives rather than a new
GitHub client. Feature-gating (`intent-source`) keeps the 11 non-consumer trusty-common
dependents paying nothing.

### 6.6 Implementing Modules

| Module | Role |
|--------|------|
| `trusty-common::intent_source::resolve` | Entry point: `IntentQuery -> ResolvedIntent`. |
| `trusty-common::intent_source::linkage` | PR→ticket extraction (lifted tga regexes). |
| `trusty-common::intent_source::spec_resolve` | SLD docstring-ref → spec section resolution. |
| `trusty-common::tickets::api::backends::Backend::get_issue` | Ticket fetch (reused). |
| `trusty-common::github_path::parse_github_path` | owner/repo derivation (reused). |
| `trusty-common::chat::ChatProvider` | Optional method extraction (reused). |

---

## 7. Integration points (concrete seams)

### 7.1 trusty-review back-gate wiring

| Step | Seam (file:symbol) | Change |
|---|---|---|
| Register source | `pipeline/runner_context.rs:203` | add `Box::new(ConformanceSource::from_config(&cs.conformance, …))` to the `Vec<Box<dyn ContextSource>>`. |
| New source | `integrations/context/conformance.rs` (new, sibling of `github_issues.rs`) | implements `ContextSource` (`context/mod.rs:324`): `gather` calls `trusty_common::intent_source::resolve(IntentQuery::Pr{…})` and renders `ResolvedIntent` as a `ContextSection` heading "Intended method (ticket/spec)". |
| Config | `integrations/context/config.rs` (`ContextSourcesConfig`) | add a `conformance: SourceConfig` field. |
| Subject | `ReviewSubject` (`context/mod.rs:95`) | already carries owner/repo/title/body/changed_files/identifiers — sufficient for `IntentQuery::Pr`. |
| Finding category | `models/mod.rs:156` (`Finding`), `parser.rs:67` (`LlmFinding`), `parser.rs:277` (`convert_llm_finding`) | add `category: FindingCategory { Correctness, MethodConformance }` (`#[serde(default)]` = Correctness for back-compat). |
| Prompt | `pipeline/prompt.rs` (schema), `prompt_templates.rs` (prose) | add the `category` field to the finding JSON schema + a conservative instruction: emit `method-conformance` only on explicit contradiction. |
| Verdict | `grade.rs:313` (`severity_floor`), `grade.rs:268` (`is_substantive`) | conformance findings flow through unchanged (severity→Effort); a category-keyed branch MAY cap conformance at REQUEST_CHANGES (never BLOCK) — mirrors the High-effort carve-out precedent. |

### 7.2 trusty-mpm front-gate wiring

| Step | Seam (file:symbol) | Change |
|---|---|---|
| Insertion point | `daemon/managed_routes/lifecycle.rs:spawn_managed` (between Step 2 `lifecycle.rs:135` and Step 3 `adapter.spawn` `lifecycle.rs:153`) | call the FRONT gate; on `Escalate`, set `pending_decision`/`proposed_default` and skip spawn. |
| Intent source | `bin/tm/commands/ticket/mod.rs:build_task` / `Issue` (`ticket/system.rs:50`) | the ticket-id + body feeding `build_task` is the `IntentQuery::Ticket` input. |
| Decision engine | `driver/policy.rs:evaluate_autonomy_tier` (`policy.rs:357`), `Disposition` (`policy.rs:247`) | conformance disposition composed stricter-wins with the autonomy decision. |
| Escalation channel | `SessionRecord.pending_decision`/`proposed_default` (`record.rs:158`); resolve via `manager.rs:answer_decision` (`manager.rs:280`) | reused as-is. |
| Vocabulary | `core/overseer.rs:OverseerDecision::FlagForHuman` (`overseer.rs:43`) | optional mapping for consistency. |
| Blocker | #1269 | until headless-spawn auto-accept lands, escalation degrades to operator-confirm. |

### 7.3 trusty-common ISR

| Step | Seam (file:symbol) | Change |
|---|---|---|
| New module | `src/lib.rs` (add `#[cfg(feature = "intent-source")] pub mod intent_source;`) | the resolver (§6). |
| Feature | `Cargo.toml [features]` | `intent-source = ["tickets", "chat"-equiv, "dep:regex", …]`. |
| Ticket fetch | `tickets::api::backends::Backend::get_issue` (`tickets/api/backends/mod.rs:106`) | reused. |
| Linkage | lift `tga::collect::ticket::{is_ticketed,extract_ticket_id}` (`collect/ticket.rs:120,155`) into `intent_source::linkage` | avoids pulling tga's git2/rusqlite stack. |
| owner/repo | `github_path::parse_github_path` (`lib.rs:370`) | reused. |
| LLM extract | `chat::ChatProvider` (`lib.rs:31`) | optional, conservative method extraction. |

### 7.4 Auth caveat (call out for implementers)

trusty-review's serve mode uses a richer GitHub **App / JWT** auth
(`integrations/github/auth/`) than trusty-common's `tickets` backend (PAT-only,
`tickets/api/backends/github/client.rs:22`). The ISR must expose a **pluggable
token-resolver seam** (mirroring review's `IssueTokenResolver`,
`context/github_issues.rs:14`) so it works in review's webhook/serve mode, not just under a
`GITHUB_TOKEN` PAT.

---

## 8. Acceptance criteria (testable)

**ISR (trusty-common):**
- **AC-1** Given a ticket whose body prescribes a method, `resolve(IntentQuery::Ticket)`
  returns `ticket_method = Some`, `precedence_winner = Ticket`.
- **AC-2** Given a ticket and a spec section that **agree**, `conflict = false`,
  `stale_spec = false`.
- **AC-3** Given a ticket and a spec that **disagree**, `precedence_winner = Ticket`,
  `conflict = true`, `stale_spec = true` (ticket > spec).
- **AC-4** Given neither a ticket method nor a spec method, all method fields `None`,
  `precedence_winner = None` (gap).
- **AC-5** PR→ticket: a PR body `Closes #1325`, a commit trailer, and a branch `fix/1325-x`
  each resolve to ticket `#1325`; a PR with no linkage resolves to `ticket = None`.
- **AC-6** Spec resolution: a changed file with a `# Spec References` rustdoc block pointing
  at `SPEC-X-01` resolves `spec_section = Some(SPEC-X-01)`; a file with no SLD ref resolves
  `spec_section = None`.
- **AC-7** Fail-open: ticket fetch error → `unresolved = Some(reason)`, all methods `None`.

**BACK gate (trusty-review):**
- **AC-8** A diff that contradicts an explicit ticket method yields ≥1 finding with
  `category = MethodConformance` and the verdict floors to `REQUEST_CHANGES` (not BLOCK).
- **AC-9** A diff that conforms (or where intent is a gap) yields **no** `MethodConformance`
  finding (false-positive guard) and the verdict is unchanged by conformance.
- **AC-10** M4 conflict: code matching a stale spec but violating the ticket → a
  `MethodConformance` finding (ticket wins); a spec-only conflict surfaces as advisory, not a
  blocking finding.
- **AC-11** ISR unresolved → the `ConformanceSource` returns an empty section and emits no
  finding (fail-open; review proceeds normally).
- **AC-12** A `MethodConformance` finding below `FLOOR_MIN_CONFIDENCE = 0.80` is advisory and
  does not raise the verdict floor.

**FRONT gate (trusty-mpm):**
- **AC-13** Ticket+spec agree (or unambiguous ticket) → `Disposition::AutoAccept` and
  `adapter.spawn` is called.
- **AC-14** Planned method diverges from an explicit ticket/spec method →
  `Disposition::Escalate`, `adapter.spawn` is **not** called, `pending_decision` is set, and
  the session is `AwaitingApproval`.
- **AC-15** A human `POST …/answer` clears `pending_decision` and unblocks the spawn.
- **AC-16** Stricter-wins composition: a T4/destructive action still escalates even when
  conformance would auto-accept; conformance never lowers a tier escalation.
- **AC-17** Non-ticketed work → `AutoAccept` (no gate); ISR failure → `AutoAccept` (fail-open).

**Cross-gate:**
- **AC-18** The same `ResolvedIntent` fixture drives both gates to consistent matrix outcomes
  (a golden test asserting FRONT escalate ⇔ BACK would-flag for M5 inputs).

---

## 9. Open Questions / Future Work

- **OQ-1 — Method extraction engine.** LLM (`ChatProvider`) vs. deterministic heuristic vs.
  hybrid for §6.2. LLM is higher-recall but adds cost/latency and a false-method risk; a
  heuristic is cheaper but misses prose. Recommendation leans hybrid (heuristic first, LLM
  fallback) — to be settled in C1.
- **OQ-2 — Multi-ticket PRs.** §6.3 picks the highest changed-file-overlap ticket; is that
  the right rule, or should the ISR return *all* linked tickets and let BACK check each?
- **OQ-3 — FRONT planned-method derivation.** Pre-work, the "planned method" is implicit in
  the task prompt/decomposition. Does the FRONT gate need an explicit plan artifact to
  compare, or is comparing the ticket-vs-spec method sufficient (escalate only when *those*
  diverge, independent of any plan)? The latter is simpler and matches "escalate on ticket/
  spec mismatch"; the former catches a divergent *plan* even when ticket/spec agree.
- **OQ-4 — Escalation surface (inherits SM-driver OQ-2).** Beyond `pending_decision`, should
  FRONT escalations also post a GitHub comment / Slack / trusty-memory note?
- **OQ-5 — Conformance verdict ceiling.** Confirm REQUEST_CHANGES is the hard ceiling for
  conformance (never BLOCK), and whether a *repeated* ignored conformance finding should
  escalate.
- **OQ-6 — SLD revision drift.** Should `~v1`-references-but-`~v2`-section (OUTDATED) feed
  `stale_spec`, or stay out of scope as declared?
- **OQ-7 — Caching.** ISR resolution per PR/ticket — cache TTL and invalidation (ticket
  bodies and specs change).

---

## 12. Non-goals

- Implementing any gate or the ISR in Rust (this PR is doc-only).
- A from-scratch LLM judge — gates reuse the review LLM / a small classification call.
- SLD four-status CI traceability enforcement (the ISR *reads* links; it does not enforce
  COVERED/UNCOVERED/ORPHANED/OUTDATED).
- Per-symbol spec coverage metrics or a traceability matrix.
- Auto-fixing or auto-rewriting a divergent implementation.
- Gating non-ticketed work, throwaway/spike work, or correctness review (conformance is an
  overlay, not a replacement for the existing correctness gate).
- Retiring or replacing the existing fuzzy `GithubIssuesSource` keyword context.

---

## 13. Child-issue breakdown (for the PM to file against the epic)

Umbrella: **epic: intent/method conformance** (issue # to be linked by the PM).

| ID | Title | Scope | Deps | Effort |
|----|-------|-------|------|--------|
| **C1** | Shared intent-source resolver (ISR) in trusty-common | `intent_source` module (§6): `resolve`, `ResolvedIntent`, precedence (ticket>spec), method extraction (§6.2, OQ-1), PR→ticket linkage (lift tga regexes, §6.3), SLD spec-resolution (§6.4), pluggable token-resolver (§7.4); `intent-source` feature flag. AC-1..AC-7. | tickets/chat/github_path already in tc | **L** (greenfield resolver + lifted linkage + spec parsing) |
| **C2** | trusty-review back gate | `ConformanceSource` (`ContextSource`) + register at `runner_context.rs:203`; `FindingCategory` field through `LlmFinding`/`Finding`/`convert_llm_finding`; prompt schema + conservative instruction; verdict floor wiring (REQUEST_CHANGES ceiling). AC-8..AC-12. | C1 | **M** (reuses pipeline seams) |
| **C3** | trusty-mpm front gate | FRONT gate at `spawn_managed` (between Step 2 and Step 3); compose with `evaluate_autonomy_tier` (stricter-wins); escalate via `pending_decision`/`proposed_default`; degrade under #1269. AC-13..AC-17. | C1; soft-blocked by #1269 for headless auto-spawn | **M** (policy engine + escalation channel already exist) |
| **C4** | SLD spec-resolver hardening | Robust changed-file/symbol → spec-section resolution (rustdoc `# Spec References` parsing, anchor resolution, revision awareness); reusable by C1; the greenfield half of §6.4. AC-6, AC-18. | C1 | **M** (no prior art in-repo) |
| **C5** | Docs + cross-gate golden tests | Wire DOC-15 into any `docs/specs` index; add the cross-gate golden test (AC-18); document config (`conformance` source, `intent-source` feature); SLD-link the new modules. | C1, C2, C3 | **S** |

**Suggested order:** C1 → (C2 ∥ C3) → C4 (can overlap C1) → C5. C4 may be folded into C1 if
the spec-resolution scope stays small; kept separate here because no in-repo prior art exists
(§2.3) and it carries its own risk.

---

## 14. Change log

- **2026-06-16** — Initial draft (DOC-15, `SPEC-CONFORMANCE-01~draft`). Defined the two-gate
  model (FRONT escalate-only-on-divergence in trusty-mpm; BACK conservative in trusty-review),
  the normative decision matrix (precedence ticket > spec), the shared intent-source-resolver
  contract (trusty-common, `intent-source` feature), per-gate verdict/escalation semantics,
  concrete integration seams (file:symbol), testable acceptance criteria, open questions, and a
  C1–C5 child-issue breakdown for the umbrella epic. Investigation grounded in trusty-review
  pipeline (`parser.rs`/`runner.rs`/`grade.rs`/`integrations/context/`), trusty-mpm
  (`driver/policy.rs`, `managed_routes/lifecycle.rs`, `record.rs`/`manager.rs`), and
  trusty-common (`tickets`, `github_path`, `chat`).
