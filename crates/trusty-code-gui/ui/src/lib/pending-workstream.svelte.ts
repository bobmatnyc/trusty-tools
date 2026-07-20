// Why: code-critic PR #3392 review, MEDIUM — a narrow repeat of the exact
// complaint issue #3384 itself was filed over. If `StartWorkingForm.svelte`
// mints a workstream and successfully runs the first task against it, but
// the (best-effort, deliberately-last) activation step then fails, the
// daemon's `active_workstream_id` pointer stays whatever it was before (or
// `null`) — so `WorkstreamActivity.svelte`'s next poll finds no active
// workstream and falls back to "no active workstream — pick a project to
// start", **while the operator's just-accepted task is actually running**.
// That is the same user-visible shape as the bug Bob filed #3384 over, just
// triggered by a different cause (a failed activation instead of a missing
// creation step).
//
// What: a tiny cross-module Svelte 5 rune store (the officially recommended
// pattern for sharing reactive state across components without a framework
// store library — export a `$state` OBJECT and mutate its properties,
// never reassign the exported binding itself, since only property writes on
// an already-shared object propagate to every importer). Holds the id/name
// of the most recently minted-and-run workstream, so
// `WorkstreamActivity.svelte` can fall back to displaying IT when the
// daemon's real `active_workstream_id` is absent or names a workstream not
// in the current list — never invented data, always an id
// `StartWorkingForm.svelte` itself just confirmed the daemon created.
// `StartWorkingForm.svelte` sets it right after a successful `POST
// /workstreams` (the same point `pendingWorkstreamId`, its own local retry-
// reuse state, is set) and clears it once activation SUCCEEDS (at that
// point the daemon's real pointer takes over on the next poll — no fallback
// needed). It is deliberately NOT cleared on an activation failure — that is
// exactly the case this store exists to cover.
// Test: `pending-workstream.test.ts`.

export const pendingWorkstream = $state<{ id: string | null; name: string | null }>({
  id: null,
  name: null,
});

/**
 * Record the most recently minted-and-run workstream.
 *
 * Why: the one write path `StartWorkingForm.svelte` calls right after `POST
 * /workstreams` succeeds — kept as a function (rather than direct property
 * assignment at call sites) so the "set both fields together, never one
 * without the other" invariant lives in one place.
 * Test: `pending-workstream.test.ts::setPendingWorkstream`.
 */
export function setPendingWorkstream(id: string, name: string): void {
  pendingWorkstream.id = id;
  pendingWorkstream.name = name;
}

/**
 * Clear the pending-workstream fallback.
 *
 * Why: called once activation SUCCEEDS — the daemon's real
 * `active_workstream_id` pointer will reflect this workstream on the very
 * next poll, so the fallback is no longer needed (and must not linger and
 * shadow a later, genuinely different active workstream — though even if it
 * did, `WorkstreamActivity.svelte` only ever consults it when the daemon
 * reports NO real active workstream, so a stale value is inert, not wrong).
 * Test: `pending-workstream.test.ts::clearPendingWorkstream`.
 */
export function clearPendingWorkstream(): void {
  pendingWorkstream.id = null;
  pendingWorkstream.name = null;
}
