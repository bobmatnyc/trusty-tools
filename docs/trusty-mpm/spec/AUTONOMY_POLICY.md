# trusty-mpm — Driver Autonomy Policy (T1–T4) & Session↔Artifact Correlation

> **Status:** Canonical · Living Document
> **Implements:** issue #1204
> **Module:** `crates/trusty-mpm/src/driver/` (`policy.rs`, `correlation.rs`)
> **Related:** [SESSION_MANAGER_DRIVER_AGENT.md](./SESSION_MANAGER_DRIVER_AGENT.md) §4 (operating-model sketch)

## Purpose

The **driver** is the calling agentic process that operates trusty-mpm through
its HTTP API / `tm` CLI. When a managed session surfaces a `pending_decision`,
the driver must decide whether to **auto-accept** the proposed default or
**escalate** to a human. The target ratio is ~80% auto-accept / ~20% escalation.

This is only safe if the auto-accept gate is driven by **structured, non-LLM
guardrails** — not by reading the session's tmux pane with a classifier. A
subtly-wrong harness must never be able to auto-merge bad code. The
`driver::policy` module is the Rust home of that rule set: every decision is a
**pure function** over explicit signal structs, with zero I/O and zero LLM calls,
so the whole policy is unit-testable offline.

> **SAFETY RULE (non-negotiable):** the auto-accept gate MUST NOT be driven by
> pane state. The session `state` / activity classification are *observability*
> signals only. A session classified `working` does not imply its output is
> correct.

## The T1–T4 tier model

The tier governs how a proposed action is handled. Tiers are ordered
`T1 < T2 < T3 < T4` (more cautious = higher); the Rust enum derives `Ord`, so a
caller can `max()` two tiers to pick the more cautious one.

| Tier | Decision type | Policy |
|---|---|---|
| **T1 — observe / style-only** | Trivial / formatting / comment-only change | Auto-accept **without** the full guardrail battery — but still escalate if a guardrail *actively objects* (trusty-review `REJECT` or red CI). A formatter that breaks the build is not trivial. |
| **T2 — guarded auto-accept** | Standard feature / bugfix confined to one crate | Auto-accept **only when ALL** structured guardrails are green (review `APPROVE`, CI `Green`, search consistent, memory consistent, in-scope). This is the common case that carries the ~80% auto-accept ratio. |
| **T3 — fallback-escalate** | Architecture-touching / cross-crate change | Auto-accept **only with an explicit trusty-review `APPROVE` AND in-scope validation AND non-red CI**; otherwise escalate. Higher bar than T2 because the blast radius is larger. |
| **T4 — human-escalate** | Irreversible / security-sensitive / destructive operation | **Always escalate.** A human must accept. |

### Tier selection

1. The caller computes a **`ChangeClass`** (`StyleOnly` / `Standard` /
   `Architectural` / `Destructive`) from diff metadata (files touched, crates
   spanned). The policy itself does not parse diffs.
2. `ChangeClass` maps onto a base tier (`StyleOnly→T1`, `Standard→T2`,
   `Architectural→T3`, `Destructive→T4`).
3. **Defense-in-depth keyword override:** if the `pending_decision` text contains
   a destructive keyword (`delete`, `drop table`, `push --force`, `decommission`,
   `rm -rf`, `truncate`, `revoke`, `rotate secret`, `wipe`, …) the tier is forced
   to **T4** regardless of the caller's classification.
4. **Prior-rejection override:** if the same decision was already rejected in the
   session (`prior_rejections > 0`), the policy escalates at any tier — the human
   already pushed back once.

## Structured guardrails (pure functions, no LLM)

`GuardrailSignals` bundles the four hard signals plus the scope check:

| Signal | Source | Favorable value |
|---|---|---|
| `review: ReviewVerdict` | trusty-review (`review_diff` / `review_pr`) | `Approve` |
| `ci: CiStatus` | GitHub PR checks / harness test output | `Green` |
| `search_consistent: bool` | trusty-search (`search`) | `true` (no conflicting implementation) |
| `memory_consistent: bool` | trusty-memory (`memory_recall`) | `true` (no blocking prior decision) |
| `scope: ScopeCheck` | `SessionCorrelation::validate_in_scope` | `InScope` |

`GuardrailSignals::all_clear()` collapses the conjunction into one auditable
predicate used by the T2 gate. On escalation the policy names the **first failing
signal** so a human can triage instantly.

## Session ↔ artifact correlation

A session is meaningful only in relation to the artifacts it should produce.
`SessionCorrelation` is the optional scope anchor, persisted on `SessionRecord`
(`#[serde(default)]` keeps pre-existing records loadable):

```rust
pub struct SessionCorrelation {
    pub worktree: Option<PathBuf>,  // scope boundary — paths outside are out-of-scope
    pub branch:   Option<String>,   // ref the session commits onto
    pub pr_id:    Option<u64>,      // PR the work belongs to
    pub issue_id: Option<u64>,      // issue the session is meant to resolve
}
```

Every field is optional because correlation **accrues** over a session's life: a
session starts with a worktree + branch (seeded at creation) and gains `pr_id` /
`issue_id` once work is pushed and a PR is opened.

`validate_in_scope(touched_paths, referenced_issue_ids) -> ScopeCheck` is the
pure guardrail the policy consults before allowing an auto-accept:

- **`InScope`** — every touched path is inside the worktree and every referenced
  issue id matches the correlated `issue_id`.
- **`OutOfScope { stray_paths, foreign_issue_ids }`** — at least one path escaped
  the worktree or referenced a foreign issue.
- **`Uncorrelated`** — the session has no correlation; scope is unknowable, so
  the policy treats it as *not* in-scope (cautious default — it does not pass the
  T2/T3 gate).

## Entry point

```rust
use trusty_mpm::driver::{
    evaluate_autonomy_tier, ActionContext, GuardrailSignals,
    ChangeClass, ReviewVerdict, CiStatus, AutonomyTier, Disposition,
};

let decision = evaluate_autonomy_tier(&ctx, &signals)?; // pure, no I/O
match decision.disposition {
    Disposition::AutoAccept { reason } => { /* call POST .../answer */ }
    Disposition::Escalate   { reason } => { /* surface to human */ }
}
```

`AutonomyDecision { tier, disposition }` carries both the tier (for telemetry /
audit) and the disposition with a human-readable reason (for the escalation
message).

## Testing

The entire policy is exercised by pure unit tests with **no API key and no
network**: `cargo test -p trusty-mpm --lib driver`. Coverage includes every tier
path, every guardrail, the destructive-keyword override, the prior-rejection
override, the empty-decision error, and the scope validator.
