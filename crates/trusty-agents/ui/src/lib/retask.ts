// Pure, framework-free helpers backing InputArea.svelte's Stop/Retask flow
// (#3063).
//
// Why: Extracted out of the Svelte component so this logic is unit-testable
// with vitest without mounting Svelte or touching the reactive stores — both
// functions here are plain string/id logic with no DOM or store dependency.
// What: `buildRetaskPayload` renders the client-side conversation history
// into a capped transcript for a retask submission; `isPendingTaskId`
// recognizes InputArea's client-generated placeholder task ids (before the
// backend's real id has been reconciled via the first `task-progress`
// event) so the Stop button can tell "no real id yet" apart from "no task
// running at all".
// Test: `retask.test.ts`.

export interface HistoryTurn {
  role: 'user' | 'assistant' | 'system' | 'pm' | 'recap';
  content: string;
}

/**
 * Why: A client-generated placeholder id (`pending-<ts>`, set in
 * InputArea's `submitTask` before the backend has returned/reconciled the
 * real task id) is never a valid argument to `DELETE /api/task/:id` — the
 * backend has never heard of it, so `cancelTask` would 404 and silently
 * no-op while the actual task keeps running (code-critic finding on #3259).
 * What: True iff `id` is non-null and starts with the `pending-` prefix
 * InputArea uses for placeholders.
 * Test: `isPendingTaskId` unit tests in `retask.test.ts`.
 */
export function isPendingTaskId(id: string | null): boolean {
  return !!id && id.startsWith('pending-');
}

/** Cap on how many prior turns are folded into a retask payload. */
export const RETASK_HISTORY_MAX_TURNS = 20;

/**
 * Cap on the total character budget of the folded transcript (the prior
 * turns; the new instruction and its wrapper text are appended on top of
 * this budget, not counted against it).
 */
export const RETASK_HISTORY_MAX_CHARS = 24_000;

/**
 * Why: Retasking (#3063) aborts the in-flight run and starts a brand-new
 * agent invocation — there is no mid-flight message-injection channel (see
 * `cancel.rs`'s design note), so continuity has to be rebuilt by hand. The
 * PM-locked design is "abort + resubmit with history": we fold the visible
 * conversation (already held client-side in the `messages` store) into the
 * new task text so the fresh invocation isn't starting from a blank slate.
 * An unbounded transcript risks blowing context/cost budgets on a long chat,
 * so history is capped two ways before being sent.
 * What: Renders prior `user`/`assistant` turns (other roles — `system`,
 * `pm`, `recap` — are dropped; they're UI-only banners, not conversational
 * turns) as `"User: <text>"` / `"Assistant: <text>"` lines, most-recent
 * last. Caps to the last `RETASK_HISTORY_MAX_TURNS` turns, then trims
 * further from the front until the joined transcript is within
 * `RETASK_HISTORY_MAX_CHARS`; a single turn that alone exceeds the char
 * budget is hard-truncated with a `…[truncated]` suffix. Whenever either cap
 * actually drops content, prefixes the transcript with a
 * `[…earlier turns omitted…]` marker so the resubmitted task's context makes
 * it clear it isn't seeing the full conversation. Appends
 * `[The previous task was stopped before completion.]` plus the new
 * instruction. Falls back to the bare instruction when there's no prior
 * history. `newContent` is taken verbatim and is never duplicated into the
 * transcript — callers pass `history` as captured BEFORE the new user turn
 * is added to the store, so `newContent` cannot already be present in it.
 *
 * Output format:
 * ```
 * […earlier turns omitted…]        <- only present when truncated
 * User: <turn 1>
 * Assistant: <turn 2>
 * ...
 *
 * [The previous task was stopped before completion.]
 * User: <newContent>
 * ```
 * Test: `buildRetaskPayload` unit tests in `retask.test.ts` — empty history,
 * normal case, turn-count truncation, char-budget truncation (both with the
 * omitted-marker), and confirming `newContent` is never duplicated.
 */
export function buildRetaskPayload(history: HistoryTurn[], newContent: string): string {
  const allTurns = history
    .filter((m) => m.role === 'user' || m.role === 'assistant')
    .filter((m) => m.content.trim().length > 0)
    .map((m) => `${m.role === 'user' ? 'User' : 'Assistant'}: ${m.content}`);

  if (allTurns.length === 0) return newContent;

  const turns =
    allTurns.length > RETASK_HISTORY_MAX_TURNS
      ? allTurns.slice(-RETASK_HISTORY_MAX_TURNS)
      : allTurns.slice();
  let truncated = turns.length < allTurns.length;

  let totalChars = turns.reduce((sum, t) => sum + t.length + 1, 0);
  while (totalChars > RETASK_HISTORY_MAX_CHARS && turns.length > 1) {
    const dropped = turns.shift();
    totalChars -= (dropped?.length ?? 0) + 1;
    truncated = true;
  }
  // Edge case: even the single remaining turn alone exceeds the budget —
  // hard-truncate it rather than send an oversized payload.
  if (turns.length === 1 && turns[0].length > RETASK_HISTORY_MAX_CHARS) {
    turns[0] = `${turns[0].slice(0, RETASK_HISTORY_MAX_CHARS)} …[truncated]`;
    truncated = true;
  }

  const transcript = truncated
    ? `[…earlier turns omitted…]\n${turns.join('\n')}`
    : turns.join('\n');

  return `${transcript}\n\n[The previous task was stopped before completion.]\nUser: ${newContent}`;
}
