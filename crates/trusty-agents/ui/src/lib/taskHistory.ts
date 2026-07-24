// Recent Tasks display filter (owner report 2026-07-23).
//
// Why: `GET /api/tasks` returns EVERY persisted `PmResponse` — including the
// per-turn envelope stored for ordinary chat replies. A Conversational or
// Research turn (e.g. "Hi Masa. How can I assist?") is finalized as a
// `type: "agent_response"` record with an EMPTY `task` field (the backend's
// `PmResponse` carries no request text — see
// `crates/trusty-agents/src/api/server/handlers.rs::submit_task`, which builds
// the chat result from `PmResponse::running` and only sets status/narrative).
// Rendered verbatim, those turns show up in the Recent Tasks panel as junk
// "(empty)" rows with status "success" — which the owner flagged live: "I
// shouldn't see these." The panel is workflow-oriented (phase stamps, score,
// cost); casual chat turns are not workflow runs and should not appear.
// What: `isDisplayableTask` — the single predicate the panel filters on:
//   - drop `agent_response` (chat) envelopes entirely, and
//   - keep the pre-existing guard that hides the contentless running
//     placeholder (a freshly-submitted task before it has any narrative)
//     until it has something to show.
// Extracted as a pure function (no Svelte/store deps) so the contract is
// unit-testable without mounting the component — matching `pollIntervalMs`
// (`transport.ts`) / `buildRoster` (`roster.ts`).
// Test: `taskHistory.test.ts`.

import type { TaskHistoryEntry } from '../stores/app';

/**
 * The wire `type` of a plain chat reply (Conversational/Research turn),
 * mirroring `PmResponseType::AgentResponse`'s snake_case serialization in
 * `crates/trusty-agents/src/api/types.rs`. These are not workflow runs.
 */
export const CHAT_RESPONSE_TYPE = 'agent_response';

/**
 * Why: Recent Tasks lists workflow runs, not chat. Chat turns are persisted
 * so the frontend can poll their result, but they must not clutter the panel.
 * What: Returns `false` for `agent_response` (chat) envelopes and for a
 * still-running placeholder that has no narrative yet; `true` otherwise.
 * Pure — depends only on the passed entry.
 * Test: `taskHistory.test.ts`.
 */
export function isDisplayableTask(
  entry: Pick<TaskHistoryEntry, 'type' | 'status' | 'narrative'> & {
    task?: string;
  },
): boolean {
  // A plain chat reply is not a task — never list it here.
  if (entry.type === CHAT_RESPONSE_TYPE) return false;
  // Hide the contentless in-flight placeholder until it has content to show.
  // `task` is unpopulated on the wire today (no `PmResponse` request-text
  // field), but this PR flags backend request-text population as a follow-up —
  // keeping the `task` disjunct means a future running task that has a title
  // but not yet a narrative still shows, matching the pre-fix guard.
  if (entry.status === 'running' && !entry.narrative?.trim() && !entry.task?.trim())
    return false;
  return true;
}

/**
 * Why: Convenience wrapper so the component can apply the predicate to the
 * whole `taskHistory` store in one call.
 * What: Returns the subset of `entries` for which `isDisplayableTask` holds,
 * order preserved.
 * Test: `taskHistory.test.ts`.
 */
export function visibleTaskHistory<
  T extends Pick<TaskHistoryEntry, 'type' | 'status' | 'narrative'> & {
    task?: string;
  },
>(entries: T[]): T[] {
  return entries.filter(isDisplayableTask);
}
