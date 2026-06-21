# DOC-23 — Learned-Autonomy Auto-Answer for Session Decision Prompts

**Status:** Draft
**Subsystem:** trusty-mpm — decision-adjudication / autonomy-learning
**Owner:** Engineering (trusty-mpm)
**Last-updated:** 2026-06-21
**Spec ID:** `SPEC-AUTONOMY-AUTO-01~draft` (DOC-23)
**Builds on:** DOC-14 — Session Manager (SM) Agent (`docs/specs/session-manager-agent.md`,
the `SessionRecord` and decision lifecycle); AUTONOMY_POLICY.md (`docs/trusty-mpm/spec/AUTONOMY_POLICY.md`,
T1–T4 tiers and `SessionCorrelation`); DOC-17 — Harness Runner Vision
(`docs/specs/harness-runner-vision.md`, the north-star for autonomous operation); DOC-20 —
Chat-Core (`docs/specs/chat-core.md`, the command catalog and LLM resolver).
**Cross-ref:** the session-manager APIs (`crates/trusty-mpm/src/session_manager/`,
`set_pending_decision` / `answer_decision`); decision escalation endpoints
(`crates/trusty-mpm/src/daemon/managed_routes/`); the SM memory module
(`crates/trusty-mpm/src/core/sm/memory.rs`, `recall` / `recall_deep`); trusty-memory
(the decision corpus storage); the action audit surface (`crates/trusty-mpm/src/core/sm/agent/chat_action.rs`,
`actions_taken`); and issues **#1526** (autonomy learning epic), **#1525** (actions-taken audit PR).

> **Scope note.** This is a **behavior contract** for the **learned-autonomy auto-answer layer** —
> a new optional decision-adjudication gate inserted between the existing decision-prompt lifecycle
> (DOC-14 / AUTONOMY_POLICY) and human-escalation surfaces (Telegram / TUI / Web). It specifies
> what the layer must do (learn from past user answers, adjudicate new decisions, gate + override
> flow, undo model), not how to implement it. The adjudicator is **strictly additive**: any failed
> gate, disabled feature, unavailable credentials, unknown user, or missing undo path falls through
> to the existing escalation path unchanged. The layer does not re-implement the decision lifecycle,
> tier evaluation, or the LLM resolver; it consumes those as-is.

---

## 1. Summary & Goal

**User story:** As an operator, I want high-confidence decisions (commits, pushes, file overwrites)
to be auto-answered based on *my* historical pattern, so I avoid interruption fatigue while retaining
full undo capability and audit visibility.

**Why:** The T2 auto-accept gate (AUTONOMY_POLICY §2) is driven by guardrails alone. A person with
consistent answer patterns (e.g., always approves certain style fixes, always stages before pushing)
can safely auto-answer more decisions *if* the system learns from their prior answers and only
auto-answers decisions within their known precedent. Personalized auto-answer removes the remaining
20% escalations when confidence is high and undo is guaranteed.

**What this spec defines:** A `DecisionAdjudicator` that, for each pending decision, (1) retrieves
the owning user's positive examples and corrections from the decision corpus, (2) embeds the new
decision prompt and resolves it against the corpus via one LLM adjudication call, (3) applies three
gates (undo path exists, undo cost ≤ tier cap, confidence ≥ tier threshold), and (4) either
auto-answers (with audit + one-tap undo) or escalates (fallthrough to human). Every auto-answer is
undoable, reversible, and surfaced to the user—never silent. Learning is keyed by user identity,
action category, and undo-cost class.

---

## 2. Scope & Non-Goals

### In scope

- Per-decision flow: decision raise → user resolution → corpus recall → LLM adjudication → gate evaluation → auto-answer or escalate.
- `DecisionAdjudicator` module: placement in the session-manager API, interface contract, and error fallthrough.
- Decision data model (`DecisionRecord`) and corpus schema (prompt, action category, undo-cost class, outcome, weighted signal).
- Personalized recall: embedding-based + typed action-category + undo-cost class keying.
- Three mandatory gates: undo path exists, undo cost ≤ per-tier cap, confidence ≥ per-tier threshold.
- Corpus learning: positive examples (human answers, un-overridden auto-answers *if* override occurred zero times), weighted corrections (overridden answers).
- Override flow: split by proximity to tier ceiling (within-tier fires immediately + notifies; near-ceiling uses grace window).
- Undo UX: one-tap undo on surface (Telegram, TUI, Web), recorded undo handle, rollback via trusty-search / git handle + user notification.
- Audit trail: decision record (who, when, LLM confidence, disposition, undo cost), override log (when + rationale), user feedback loop for corpus improvement.
- Config: per-tier thresholds (`confidence_threshold`, `max_undo_cost`), global enable/disable, optional grace-window seconds, corpus room name.

### NOT in scope (reference only, do not re-spec)

- The decision-prompt lifecycle (`SessionRecord::pending_decision`, `set_pending_decision` / `answer_decision`).
- AUTONOMY_POLICY.md tier classification (T1–T4), guardrails, and non-LLM auto-accept rules.
- The LLM resolver (`SmModelTier::Orchestration`, SM chat-action loop) — the adjudicator *consumes* the same resolver, not re-implements it.
- trusty-memory architecture (the palace, storage, API) — this spec consumes `recall` / `recall_deep` as black boxes.
- Telegram / TUI / Web surface implementations — each adapter wires the undo UX independently.
- The full harness runner (DOC-17) — the adjudicator is one autonomous gate within it.

---

## 3. Current State & Integration Points

### 3.1 Existing decision lifecycle (reference)

1. **Raise:** session-manager raises a `pending_decision` on `SessionRecord` via `set_pending_decision(id, decision_text)` (`crates/trusty-mpm/src/session_manager/manager.rs:918`).
2. **Escalate to human:** daemon surfaces the decision via alert loop (Telegram bot, TUI, Web) with `proposed_default` and prompts for human answer.
3. **Answer:** human calls `POST /sessions/{id}/answer` → `answer_decision(id, answer)` → clears decision, injects answer into tmux pane.

**Field location:** `SessionRecord { pending_decision: Option<String>, proposed_default: Option<String>, … }` (`crates/trusty-mpm/src/session_manager/record.rs:158`).

### 3.2 T1–T4 tier model (reference)

AUTONOMY_POLICY.md §2 defines tiers:
- **T1** (observe / style-only) — auto-accept without full guardrails.
- **T2** (guarded auto-accept) — auto-accept when **all** guardrails pass (review, CI, scope, search, memory consistency).
- **T3** (fallback-escalate) — auto-accept only with explicit trusty-review APPROVE + scope; otherwise escalate.
- **T4** (human-escalate) — always escalate.

The adjudicator operates **after** AUTONOMY_POLICY decides a tier. If the policy dispatches the decision to a human, the adjudicator may intercede *only if* the decision's confidence + undo safety clear the per-tier gate.

### 3.3 Session correlation & undo model

`SessionCorrelation` anchors a session to worktree / branch / PR / issue (AUTONOMY_POLICY.md §3):
```rust
pub struct SessionCorrelation {
    pub worktree: Option<PathBuf>,
    pub branch:   Option<String>,
    pub pr_id:    Option<u64>,
    pub issue_id: Option<u64>,
}
```
Every decision must carry a concrete undo handle (git commit sha, file path, line number range) and an undo-cost class: `uncommitted_edit` < `local_commit` < `pushed_commit` < `open_pr` < `merged_release` < `irreversible`. The adjudicator refuses to auto-answer if the undo handle is missing or undo cost exceeds the tier cap.

### 3.4 Memory & LLM resolver integration

- **Recall:** `SmMemory::recall(query)` / `recall_deep(query)` (`crates/trusty-mpm/src/core/sm/memory.rs`) returns embedding-based corpus hits (top-K results, user-scoped).
- **LLM resolver:** same `SmModelTier::Orchestration` resolver used by the SM chat-action loop (`crates/trusty-mpm/src/core/sm/agent/chat_action.rs`) — one shot per decision.
- **Audit surface:** `ChatActionOutcome::actions_taken` (`crates/trusty-mpm/src/core/sm/agent/chat_action.rs`) logs each invoked verb for traceability; the adjudicator extends this to log adjudication calls.

### 3.5 User identity & owning-user resolution

Each `SessionRecord` carries optional `owning_user: Option<String>` (resolves at session creation or from session-manager call context). The adjudicator **only** operates if an owning user is known; if unknown, escalates to human (fail-safe).

---

## 4. Requirements

All numbered requirements use the stable ID format `SPEC-AUTONOMY-AUTO-NN~draft`.

### 4.1 Adjudicator & Flow {#SPEC-AUTONOMY-AUTO-01}

**Requirement: DecisionAdjudicator placement and API contract.**

The `DecisionAdjudicator` is a new module at `crates/trusty-mpm/src/decision_adjudication/mod.rs` (or similar) with a public async `adjudicate(SessionRecord, pending_decision: &str) → AdjudicationResult` method. It is wired into the session-manager API as an optional pre-gate **before** escalation to human surfaces.

- **Placement:** invoked in the decision-answer flow, after AUTONOMY_POLICY evaluation, before alert-loop escalation (i.e., in the daemon's managed-session routes or the session-manager's answer path).
- **API:** `pub async fn adjudicate(&self, record: &SessionRecord, decision: &str) -> Result<AdjudicationResult, AdjudicationError>`.
- **Return type:** `AdjudicationResult { disposition: Disposition, confidence: f64, undo_cost_class: UndoCostClass, undo_handle: Option<String>, rationale: String }` where `Disposition = { AutoAnswer, Escalate }`.
- **Fallthrough:** any error (no memory, LLM unavailable, user unknown, undo path missing) returns `Escalate` → proceeds to human escalation unchanged.
- **Invariant:** if `disposition = AutoAnswer`, then undo_path must exist and undo_cost ≤ tier cap and confidence ≥ tier threshold.

**Acceptance criteria:**
- Adjudicator is invoked in the session-manager decision path and returns structured result.
- All error modes fall through to escalation (not fail-closed, not fail-open — unchanged escalation path).
- Result is logged to audit trail with user identity, decision prompt, LLM confidence, and undo metadata.

---

### 4.2 Per-Tier Policy & Thresholds {#SPEC-AUTONOMY-AUTO-02}

**Requirement: Per-tier confidence + undo-cost gates.**

Each tier (T1–T4 from AUTONOMY_POLICY) has a configuration:
```rust
pub struct TierPolicy {
    pub tier: AutonomyTier,
    pub confidence_threshold: f64,  // 0.0..1.0
    pub max_undo_cost: UndoCostClass,
}
```

- **T1 (observe/style-only):** `confidence_threshold ≥ 0.85`, `max_undo_cost = uncommitted_edit`.
- **T2 (guarded auto-accept):** `confidence_threshold ≥ 0.92`, `max_undo_cost = local_commit`.
- **T3 (fallback-escalate):** `confidence_threshold ≥ 0.96`, `max_undo_cost = pushed_commit`.
- **T4 (human-escalate):** **never auto-answers** (confidence/undo gates do not apply; always escalates).

Thresholds are configurable via daemon config file / environment; defaults are above.

**Acceptance criteria:**
- Config is loaded at daemon startup; per-tier thresholds are immutable per session.
- Gate function `passes_gate(confidence, undo_cost_class, tier_policy) → bool` correctly rejects confidence < threshold or undo_cost > max.
- T4 decisions always escalate, bypassing gates entirely.

---

### 4.3 Reversibility & Undo Model {#SPEC-AUTONOMY-AUTO-03}

**Requirement: Every auto-answer is undoable and undo cost is computable.**

A decision auto-answer includes a concrete **undo handle** — a representation of how to reverse the action. Undo handles are action-category-specific:

- **Commit:** git commit sha (rollback via `git reset --soft <sha>` or `git revert <sha>`).
- **Push:** git push remote + branch + prior commit sha (rollback via `git push --force-with-lease` or force-push to prior).
- **File overwrite:** file path + line-number range + backup hash (rollback via `git restore` or `trusty-search` file-history).
- **Dependency add:** lock-file hash + prior lock-file diff (rollback via `git restore Cargo.lock` or equivalent).
- **Permission grant:** service + affected principal (rollback via revoke call, time-bounded).

**Undo cost class (ordinal, monotonic):**
1. `uncommitted_edit` — file not yet staged; undo via `git checkout <file>` or discard.
2. `local_commit` — committed locally but not pushed; undo via `git reset / git revert`.
3. `pushed_commit` — pushed to branch but PR not merged; undo via `git push --force-with-lease` or revert commit.
4. `open_pr` — PR is open but not yet merged; undo via close/revert PR + optional revert commit.
5. `merged_release` — merged to main or released; undo via release revert / hotfix (manual, high-effort).
6. `irreversible` — external side-effects (email sent, payment processed, external API call); no automatic undo.

**Determination:** Undo cost is computed from `SessionCorrelation` (branch, PR state, release tags) + git history + action metadata. If the session has no correlation, undo cost is `unknown` → escalate (cautious default).

**Acceptance criteria:**
- Every `AdjudicationResult::auto_answer` includes a non-empty undo_handle.
- Undo cost is deterministically computable from action category + correlation + git state.
- Missing undo handle or irreversible action → disposition = Escalate.
- Undo handles are stored in `DecisionRecord` for later rollback.

---

### 4.4 Personalization & User-Scoped Corpus {#SPEC-AUTONOMY-AUTO-04}

**Requirement: Learning is per-user; corpus is keyed by user identity, action category, and undo-cost class.**

The adjudicator does **not** learn globally; it learns per-user. A decision corpus record is a tuple:
```rust
pub struct DecisionRecord {
    pub user_id: String,                              // owning user identity
    pub action_category: ActionCategory,              // commit / push / file_overwrite / dependency_add / permission_grant
    pub undo_cost_class: UndoCostClass,               // ordered enum
    pub prompt_embedding: Vec<f32>,                   // semantic embedding of the decision prompt
    pub prompt_text: String,                          // original prompt
    pub human_answer: String,                         // what the human / prior user decided
    pub answer_source: AnswerSource,                  // human | auto_answer_unadjudicated | auto_answer_learned
    pub override_count: u32,                          // how many times the user overrode this auto-answer
    pub confidence_at_time: f64,                      // LLM confidence when decided
    pub disposition: Disposition,                     // what was decided
    pub outcome: DecisionOutcome,                     // stood_unadjudicated | auto_answer | overridden | rolled_back
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
}
```

**Recall:** `adjudicate` calls `memory.recall_deep(prompt_embedding, user_id, action_category, undo_cost_class)` to fetch top-K user-scoped, category-matched, undo-class-adjacent corpus hits. The LLM sees the prompt + matching examples + overrides as weighted negative examples.

**Positive examples:** decisions where the human accepted (or an auto-answer went un-overridden for N days, pending user-feedback design).

**Corrections (weighted negative examples):** decisions where the user overrode an auto-answer — weighted by override_count.

**Acceptance criteria:**
- Recall is scoped to the owning user and filters by action category + undo-cost class.
- Positive examples are unambiguous (human answered or auto-answer stood for audit window).
- Corrections are weighted by repeat-override count.
- Unknown user → escalate (safe default).
- Unknown action category → escalate.

---

### 4.5 Learned Adjudication & LLM Call {#SPEC-AUTONOMY-AUTO-05}

**Requirement: Single LLM adjudication call per decision, consuming learned corpus, returning confidence + reasoning.**

The adjudicator constructs a **single prompt** to the orchestration-tier resolver:

```
Given the session context (branch, PR, issue):
  [SessionCorrelation]

The pending decision is:
  [decision_text]

The owning user [user_id] has historically:
  [Top-K positive examples from corpus]

The user overrode these similar decisions:
  [Weighted corrections / negative examples]

Should this decision be auto-answered with [proposed_default], or escalated to the user?
Respond with JSON: { "answer": "auto" | "escalate", "confidence": 0.0..1.0, "reasoning": "…" }
```

The resolver is the **same** `SmModelTier::Orchestration` used by the SM agent (DOC-20 §3, `crates/trusty-mpm/src/core/sm/agent/chat_action.rs`). The response is a structured `AdjudicationResponse { answer, confidence, reasoning }`.

**Invariant:** The adjudicator does **not** decide on its own; it always defers to the human's preferred action if confidence is ambiguous. The three gates (undo path, undo cost, confidence threshold) are the final arbiter.

**Acceptance criteria:**
- LLM call includes corpus context (positive examples, corrections).
- Response includes structured confidence (0.0..1.0).
- Low confidence or LLM error → disposition = Escalate.
- LLM reasoning is logged for audit.

---

### 4.6 Audit Trail & Undo UX {#SPEC-AUTONOMY-AUTO-06}

**Requirement: Every auto-answer is surfaced to the user with one-tap undo + rationale; overrides are logged.**

When the adjudicator auto-answers, the disposition includes:

```rust
pub struct AutoAnswerDisposition {
    pub answer: String,                    // the auto-answer to inject
    pub confidence: f64,                   // LLM confidence
    pub reasoning: String,                 // LLM + gate rationale
    pub undo_handle: String,               // concrete undo representation
    pub undo_cost_class: UndoCostClass,
    pub session_id: ManagedSessionId,
}
```

**Surfacing:** Each adapter (Telegram, TUI, Web) renders the auto-answer as:
```
[✓ Auto-Approved — your pattern]
  Confidence: 92% | Undo: git reset --soft <sha>
  [⏮ Undo] [👍 Thumbs up (learn)] [👎 Thumbs down (correct)]
```

- **Undo button:** calls `POST /sessions/{id}/undo` → `adjudicator.undo(id)` → resolves undo_handle → calls git / trusty-search / revoke → logs override + correction.
- **Thumbs up:** positive feedback → logs `override_count = 0` (affirm the corpus entry).
- **Thumbs down:** negative feedback (user regrets) → increments `override_count`, marks as `rolled_back`, adds weighted correction to corpus.

**Override flow (split by confidence proximity to tier ceiling):**
- **Within-tier (confidence > ceiling - 0.05):** undo fires immediately, logs correction, notifies user inline.
- **Near-ceiling (ceiling - 0.10 ≤ confidence ≤ ceiling - 0.05):** optional grace window (configurable, default: 30 seconds); if user does not undo within window, logs as `stood` (uncontradicted); if user taps undo, rolls back + corrects.

**Acceptance criteria:**
- Auto-answer is always displayed with confidence + undo button.
- Undo button calls adjudicator undo method, which resolves handle + rolls back.
- Override / correction is logged to decision record with timestamp + user id.
- User feedback (thumbs up / down) updates corpus.
- Grace window is optional, configurable.

---

### 4.7 Degradation & Fallthrough {#SPEC-AUTONOMY-AUTO-07}

**Requirement: All uncertain cases escalate; feature only removes interruptions it is confident about.**

- **No memory:** `recall` returns empty → escalate.
- **LLM unavailable / error:** error is caught → escalate.
- **No owning user:** `SessionRecord::owning_user` is `None` → escalate.
- **Unknown action category:** action not in `ActionCategory` enum → escalate.
- **Missing undo handle:** action category logic fails to compute undo → escalate.
- **Undo cost exceeds tier cap:** `undo_cost > tier_policy.max_undo_cost` → escalate.
- **Confidence below threshold:** `confidence < tier_policy.confidence_threshold` → escalate.
- **T4 tier:** never auto-answers → always escalate.
- **Feature disabled:** config `autonomy_auto_answer_enabled = false` → always escalate.

In all cases, escalation path is unchanged: decision flows to alert loop (Telegram, TUI, Web) for human review.

**Invariant:** The feature is an optional gate. If it fails or is disabled, the session surfaces a human escalation, same as before.

**Acceptance criteria:**
- No fallback path modifies session state (safe).
- All error modes are logged with reason.
- Feature can be disabled globally or per-tier.

---

### 4.8 Config & Enablement {#SPEC-AUTONOMY-AUTO-08}

**Requirement: Per-tier thresholds, global enable/disable, corpus room, grace window.**

Daemon config (YAML / TOML, loaded at startup):
```yaml
autonomy:
  auto_answer_enabled: true
  tiers:
    T1:
      confidence_threshold: 0.85
      max_undo_cost: uncommitted_edit
      grace_window_seconds: null  # immediate override
    T2:
      confidence_threshold: 0.92
      max_undo_cost: local_commit
      grace_window_seconds: 30
    T3:
      confidence_threshold: 0.96
      max_undo_cost: pushed_commit
      grace_window_seconds: 60
    T4:
      # never auto-answers
  decision_corpus_room: decisions  # trusty-memory palace room name
```

All values are immutable per session; changes apply to new sessions only.

**Acceptance criteria:**
- Config loads at daemon startup.
- Invalid config fails gracefully (e.g., threshold not in [0.0, 1.0] → use default).
- Per-tier enable/disable is supported (e.g., `T2: { enabled: false }`).
- Grace window is optional; 0 = immediate override.

---

## 5. Architecture & Data Model

### 5.1 DecisionAdjudicator module placement

```
┌─────────────────────────────────────────────────────────────┐
│  Daemon / managed_routes (POST /sessions/{id}/answer)       │  entry point
└───────────────┬─────────────────────────────────────────────┘
                │
                ▼
┌─────────────────────────────────────────────────────────────┐
│  DecisionAdjudicator::adjudicate                             │  new gate
│    (1) resolve owning user                                  │
│    (2) recall corpus by embedding + category + undo-class   │
│    (3) call LLM resolver (SmModelTier::Orchestration)       │
│    (4) apply gates (undo path, undo cost, confidence)       │
│    (5) return { disposition, undo_handle, rationale }       │
└───────────────┬─────────────────────────────────────────────┘
                │
        ┌───────┴────────┐
        ▼                ▼
   AutoAnswer         Escalate
   (send to tmux)    (alert loop)
   (log to corpus)   (human decides)
```

### 5.2 DecisionRecord schema

Stored in trusty-memory under a dedicated `decisions` palace room, per user:

```rust
pub struct DecisionRecord {
    // Corpus key
    pub id: String,                                     // UUID
    pub user_id: String,                               // owning user
    pub action_category: ActionCategory,               // enum: commit, push, file_overwrite, …
    pub undo_cost_class: UndoCostClass,                // enum: uncommitted_edit, local_commit, …

    // Decision context
    pub session_id: ManagedSessionId,                  // correlated session
    pub session_correlation: SessionCorrelation,       // worktree, branch, pr, issue anchors
    pub decision_prompt: String,                       // full proposed decision text
    pub prompt_embedding: Vec<f32>,                    // embedding (768-dim or per-model)
    pub proposed_answer: String,                       // the default the harness proposed

    // What happened
    pub human_answer: Option<String>,                  // if human decided
    pub answer_source: AnswerSource,                   // human | auto_learned | auto_unadjudicated
    pub disposition: Disposition,                      // auto_answer | escalate
    pub adjudication_outcome: Option<AdjudicationOutcome>,  // { confidence, reasoning, undo_cost_class, override_count }

    // Audit & feedback
    pub created_at: DateTime<Utc>,
    pub updated_at: DateTime<Utc>,
    pub stood_at: Option<DateTime<Utc>>,              // timestamp user affirmed (thumbs up)
    pub overridden_at: Option<DateTime<Utc>>,         // timestamp user overrode (thumbs down / undo)
    pub override_count: u32,                           // cumulative overrides for this decision pattern
}

pub enum ActionCategory {
    Commit,
    Push,
    FileOverwrite,
    DependencyAdd,
    PermissionGrant,
    // extensible
}

pub enum AnswerSource {
    Human,
    AutoAnswerLearned,     // from DecisionAdjudicator, un-overridden
    AutoAnswerUnadjudicated, // from AUTONOMY_POLICY T1 / T2, pre-adjudicator
}

pub struct AdjudicationOutcome {
    pub confidence: f64,
    pub reasoning: String,
    pub undo_handle: String,
    pub undo_cost_class: UndoCostClass,
}
```

### 5.3 DecisionAdjudicator API & contract

```rust
pub struct DecisionAdjudicator {
    memory: SmMemory,
    lmm_resolver: SmLlmResolver,  // reuse SM's Orchestration tier
    config: AutonomyConfig,
    store: SessionStore,
}

pub enum Disposition {
    AutoAnswer {
        confidence: f64,
        reasoning: String,
        undo_handle: String,
        undo_cost_class: UndoCostClass,
    },
    Escalate {
        reason: String,  // why escalated (missing corpus, low confidence, undo cost, …)
    },
}

pub struct AdjudicationResult {
    pub disposition: Disposition,
}

impl DecisionAdjudicator {
    pub async fn adjudicate(
        &self,
        record: &SessionRecord,
        decision_text: &str,
    ) -> Result<AdjudicationResult, AdjudicationError> {
        // 1. Resolve owning_user; if None, escalate.
        // 2. Determine action_category from decision_text (heuristic or ML).
        // 3. Call memory.recall_deep(embedding, user_id, action_category, undo_cost_class).
        // 4. Construct LLM prompt; call resolver with corpus context.
        // 5. Apply gates (undo path, undo cost ≤ cap, confidence ≥ threshold).
        // 6. If passed, return AutoAnswer; else Escalate.
        // 7. Log to audit trail.
    }

    pub async fn undo(
        &self,
        session_id: &ManagedSessionId,
        undo_handle: &str,
    ) -> Result<(), AdjudicationError> {
        // Resolve undo_handle to concrete git / file operation.
        // Execute rollback (git reset / git push / file restore).
        // Log override + correction to corpus.
        // Notify user.
    }
}
```

### 5.4 Recall flow (embedding-based + category filter)

```
User answer: "Let's merge this PR"
  ↓
Embed prompt (768-dim vector, same encoder as corpus)
  ↓
Call: memory.recall_deep(
    query=prompt_embedding,
    user_id="bob@example.com",
    filters={
        action_category: ActionCategory::Push,
        undo_cost_class: UndoCostClass::OpenPr,
    },
    top_k=5,
)
  ↓
trusty-memory returns: [DecisionRecord, DecisionRecord, …]
  ↓
LLM sees examples (positive) + corrections (weighted negative)
  ↓
LLM decides: { answer: "auto", confidence: 0.94, reasoning: "… matches user's pattern for PR merges" }
```

---

## 6. Phased Rollout (mapped to EPIC #1526 WI-2…WI-7)

| WI | Phase | Work | Status | Blocker |
|----|-------|------|--------|---------|
| WI-2 | Corpus + capture | Define `DecisionRecord` schema; wire decision capture to trusty-memory `decisions` room (all decisions, human + prior auto). | TBD | SPEC-AUTONOMY-AUTO-01 (this spec in Draft) |
| WI-3 | Reversibility classifier | Implement `compute_undo_handle()` + `classify_undo_cost()` per action category. Test determinism (same decision → same undo representation). | TBD | SessionCorrelation live; git history accessible. |
| WI-4 | DecisionAdjudicator core | Implement `adjudicate()` call: recall + LLM call + gates. Fallthrough on error (no corpus, LLM fail, low confidence). | TBD | WI-2 corpus live; WI-3 undo classifier live. |
| WI-5 | Gate + config | Wire per-tier thresholds (T1/T2/T3/T4). Config loader. Enable/disable flag. Audit logging. | TBD | WI-4 adjudicator live. |
| WI-6 | Undo UX + override | Undo button on each surface (Telegram, TUI, Web). Grace-window logic. Override logging + corpus correction. | TBD | WI-4 adjudicator live; per-surface adaptation. |
| WI-7 | Personalization + degradation | User-scoped recall filter. Fallthrough on unknown user / missing action category / no undo. Comprehensive tests. | TBD | WI-2 corpus live; WI-4 adjudicator live. |

**Critical path:** WI-2 → WI-3 → WI-4 → (WI-5 + WI-6 in parallel) → WI-7 → integration + testing.

**Parallel tracks:** WI-6 (undo UX) can start after WI-4 is unblocked (adjudicator exists, even if gates are incomplete).

---

## 7. Open Questions for Owner (Bob)

1. **Undo-handle representation per action category:** How should we represent undo handles for permission grants, external API calls, email sends? Do these even have undo paths, or do they **always** escalate (T4)?

2. **Undo-cost classification:** Should undo cost be computed deterministically (purely from git state + action metadata) or with LLM help (prompt the LLM: "what's the reversibility cost of this action")? Trade-off: deterministic is safer + testable; LLM is more nuanced but slower + non-deterministic.

3. **Numeric scales & per-tier defaults:** Are the suggested thresholds (T1: 0.85, T2: 0.92, T3: 0.96) the right starting point, or should we pilot with tighter margins? Do grace-window defaults (T1: none, T2: 30s, T3: 60s) make sense?

4. **Owning-user resolution:** When a session is created, how is `owning_user` determined? From the CLI caller (`--user bob`)? From the session-manager call context (auth token)? Can it change mid-session, or is it immutable?

5. **Correction weighting & decay:** Should overrides decay over time (e.g., an override from 90 days ago counts less than last week)? Should `override_count` be a simple integer, or should each override be timestamped separately?

6. **Cross-surface audit + undo parity:** If a user clicks undo in Telegram, is the correction visible in the TUI undo history? Does each surface maintain its own override log, or is there a shared audit table? How do we ensure consistency?

7. **Learning from unadjudicated auto-answers:** Can an unadjudicated auto-answer (from AUTONOMY_POLICY T1 / T2, before this spec) count as a positive example if it goes un-overridden? Or should we only learn from explicit human answers? (Risks confirmation bias if we're not careful.)

8. **Action category inference:** Should action category be inferred from the decision prompt via NLP / LLM, or hard-coded by the site that raises the decision (e.g., `set_pending_decision(…, category: ActionCategory::Commit)`)? Inference is more flexible; hard-coding is more auditable.

---

## 8. References

- **AUTONOMY_POLICY.md** — T1–T4 tiers, guardrails, `SessionCorrelation`, tier evaluation entry point.
- **docs/specs/session-manager-agent.md (DOC-14)** — `SessionRecord`, `set_pending_decision`, `answer_decision`, session lifecycle.
- **docs/specs/harness-runner-vision.md (DOC-17)** — north-star for autonomous operation; autonomy layer integration.
- **docs/specs/chat-core.md (DOC-20)** — LLM resolver, SM Orchestration tier, command catalog.
- **crates/trusty-mpm/src/session_manager/** — `SessionRecord` type, `set_pending_decision` / `answer_decision` APIs.
- **crates/trusty-mpm/src/core/sm/memory.rs** — `recall` / `recall_deep` methods for corpus retrieval.
- **crates/trusty-mpm/src/core/sm/agent/chat_action.rs** — `ChatActionOutcome::actions_taken`, SmLlmResolver usage.
- **EPIC #1526** — learned-autonomy epic; WI-2…WI-7 work items.
- **PR #1525** — actions-taken audit surface (merged or in-flight).
