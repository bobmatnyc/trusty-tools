// Why: `StatusBar.svelte` needs the wire shape for `GET /sessions/{id}/budget`
// (issue #3015, PR #3042) plus the small formatting/threshold-classification
// rules DOC-39 §4.5 attaches to it (AC-5.6 render the real ratio, AC-5.7 make
// `within_budget == false` visually distinct, AC-5.8's compaction cue).
// Extracted into its own module — rather than inlined in the component, the
// way `readinessLabel`/`readinessDotClass` currently are — because it is
// pure/testable logic independent of DOM/polling concerns, mirroring the
// split `session-status.ts` already established for `pickActiveSession`.
//
// CRITICAL naming note (issue #3043): DOC-39 §5.6's prose uses
// `working_pct`/`fired`, but the shipped Rust types
// (`crate::events::ContextBudgetSnapshot`,
// `crate::session::registry_events::ContextBudgetQuery`) use
// `working_context_pct`/`compaction_fired`. This file mirrors the CODE
// verbatim — the spec prose is stale, not the source of truth.
//
// What: `ContextBudgetSnapshot`/`ContextBudgetQuery` wire types, plus three
// pure functions: `classifyBudget` (visual-state threshold classification),
// `budgetLabel` (status-bar text), `budgetTitle` (hover detail). All three
// accept `null` for "not fetched yet" and treat it identically to
// `never_recorded` — the status bar has nothing honest to say about a budget
// it hasn't queried, so the two states render the same "no data yet" cue
// rather than a silently-different third label.
//
// Known gap (issue #3050, see `classifyBudget`/`budgetLabel` docstrings):
// DOC-39 §4.5 AC-5.9 requires a distinct "not applicable" state for non-PM
// sessions (cadence is PM-only). `ContextBudgetQuery` carries no PM/cadence
// discriminant on the wire today, so non-PM sessions render as "no data
// yet" rather than "not applicable" until #3050 adds that discriminant.
// Test: `context-budget.test.ts`.

/** Mirrors `crate::events::ContextBudgetSnapshot` field-for-field. */
export interface ContextBudgetSnapshot {
  context_window_tokens: number;
  overhead_tokens: number;
  overhead_cap_tokens: number;
  working_context_pct: number;
  overhead_pct: number;
  within_budget: boolean;
  compaction_fired: boolean;
  compaction_rounds: number;
}

/** Mirrors `crate::session::registry_events::ContextBudgetQuery`
 * (`#[serde(tag = "status", rename_all = "snake_case")]`). */
export type ContextBudgetQuery =
  | ({ status: 'recorded' } & ContextBudgetSnapshot)
  | { status: 'never_recorded' };

/** The status bar's visual treatment for the budget slot. `'warn'` is the
 * DOC-39 §4.5 AC-5.7 floor breach (`within_budget === false`); `'no-data'`
 * covers both `never_recorded` and "not fetched yet" (`null`) — genuinely
 * indistinguishable states from the client's point of view, and both must
 * render as an honest gap, never a fabricated ok/warn guess. */
export type BudgetVisualState = 'ok' | 'warn' | 'no-data';

/**
 * Classify a budget query into its status-bar visual treatment.
 *
 * Why: DOC-39 §4.5 AC-5.7 requires `within_budget === false` to be visually
 * distinguishable (red, the spec's `--gap` token) from the healthy case —
 * this is the one threshold decision the status bar makes.
 * What: `null` or `{status: 'never_recorded'}` -> `'no-data'`;
 * `within_budget === false` -> `'warn'`; otherwise `'ok'`.
 *
 * **Known gap (issue #3050):** AC-5.9 requires a distinct "not applicable"
 * treatment for non-PM sessions (cadence is PM-only —
 * `AgentLoopConfig.cadence` defaults `None`; only `task/executor.rs:327`
 * sets `Some`), rather than implying an unmanaged context. `ContextBudgetQuery`
 * carries no PM/cadence discriminant on the wire today, so this function
 * cannot tell "non-PM session, will never record" from "PM session, hasn't
 * completed a turn yet" — both collapse to `'no-data'` until #3050 adds the
 * discriminant. Do not paper over this with client-side PM heuristics.
 * Test: `context-budget.test.ts::classifyBudget`.
 */
export function classifyBudget(query: ContextBudgetQuery | null): BudgetVisualState {
  if (!query || query.status === 'never_recorded') return 'no-data';
  return query.within_budget ? 'ok' : 'warn';
}

/**
 * Status-dot Tailwind class for a budget query, reusing the same
 * `bg-status-*` token set `readinessDotClass` (`StatusBar.svelte`) already
 * establishes — `'warn'` maps to `status-error` (red), matching DOC-39 §4.5
 * AC-5.7's "red `--gap` per the token map" (this codebase has no separate
 * `--gap` token; `status-error` is the red already in use for the
 * daemon-unreachable state).
 * Test: `context-budget.test.ts::budgetDotClass`.
 */
export function budgetDotClass(query: ContextBudgetQuery | null): string {
  switch (classifyBudget(query)) {
    case 'ok':
      return 'bg-status-ok';
    case 'warn':
      return 'bg-status-error';
    default:
      return 'bg-status-neutral';
  }
}

/**
 * One-line status-bar label.
 *
 * Why: AC-5.6 the status bar must render the real working-context ratio,
 * not an estimate; AC-5.8 a fired compaction must be observable — this is
 * the lightweight status-bar cue for that (the fuller thread-level marker
 * §4.7 describes is a separate, not-yet-built surface). `never_recorded`
 * renders "no data yet", never a fake `0%` — DOC-39's own framing: "an
 * invisible budget is not a budget", and a fabricated zero is worse than
 * invisible because it looks like real data.
 * What: `null`/`never_recorded` -> `"no data yet"`;
 * `recorded` -> `"NN% working"`, `", compacted"` appended when
 * `compaction_fired`.
 *
 * **Known gap (issue #3050):** AC-5.9's "not applicable" state for non-PM
 * sessions requires a wire discriminant that doesn't exist yet — tracked
 * #3050; until then non-PM sessions render as `"no data yet"`, same as a PM
 * session that simply hasn't completed a turn.
 * Test: `context-budget.test.ts::budgetLabel`.
 */
export function budgetLabel(query: ContextBudgetQuery | null): string {
  if (!query || query.status === 'never_recorded') return 'no data yet';
  const compactionNote = query.compaction_fired ? ', compacted' : '';
  return `${query.working_context_pct}% working${compactionNote}`;
}

/**
 * Hover-detail (`title` attribute) text for the budget slot — the
 * per-token-count/overhead detail that doesn't fit the one-line label.
 * Test: `context-budget.test.ts::budgetTitle`.
 */
export function budgetTitle(query: ContextBudgetQuery | null): string {
  if (!query || query.status === 'never_recorded') {
    return 'No context-budget measurement recorded yet for this session';
  }
  const roundsNote = query.compaction_fired
    ? `, compacted ${query.compaction_rounds}x`
    : '';
  return (
    `context window ${query.context_window_tokens} tokens — ` +
    `overhead ${query.overhead_pct}% (cap from ${query.overhead_cap_tokens} tokens)` +
    `${roundsNote}`
  );
}
