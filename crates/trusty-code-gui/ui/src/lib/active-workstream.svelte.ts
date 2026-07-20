// Why: code-critic PR #3460 review, HIGH 2 — `StartWorkingForm.svelte`'s
// chat-continuation state (`lastSessionId`/`continuationWorkstreamId`) used
// to reset ONLY when the selected-project store changed, never when the
// daemon's ACTUAL active workstream changed. `WorkstreamSwitcher.svelte`
// switches workstreams without touching the project selection at all, so
// the sequence "converse in workstream A -> switch to B via the header
// switcher -> type a follow-up" silently appended the follow-up to the
// now-hidden workstream A (or, on a project mismatch, fell into the
// fallback-minting path under stale workstream A). The two components that
// KNOW the daemon's active workstream (`WorkstreamActivity.svelte` and
// `WorkstreamSwitcher.svelte`, each with its own independent poll — the
// established house convention) had no way to tell the input bar about it.
//
// This module is the same cross-module Svelte 5 rune-store pattern
// `pending-workstream.svelte.ts` and `selected-project.svelte.ts` already
// established (a `.svelte.ts` module exporting a `$state` OBJECT whose
// properties are mutated via setters — the exported binding itself is never
// reassigned, which is what makes cross-module reactivity work): both
// pollers write the resolved active-workstream id here; `StartWorkingForm`
// watches it and re-targets continuation whenever it genuinely changes.
//
// **Writers MUST write the RESOLVED id, not the raw daemon pointer.**
// "Resolved" = the daemon's real `active_workstream_id`, falling back to
// `pending-workstream.svelte.ts`'s marker when the real pointer is absent
// but the marker names a workstream that exists in the same poll's list
// (the activation-failed-after-successful-task-run case — see that
// module's docs). If one writer resolved and another wrote the raw
// pointer, the two pollers would flip-flop this value every few seconds in
// exactly that case, and every flip would spuriously reset the form's
// continuation state. One resolution rule, applied by every writer
// ([`resolveActiveWorkstreamId`] is that rule, exported so no writer
// re-implements it).
//
// What: [`activeWorkstreamState`] holds the resolved id (`null` = no
// active workstream known); [`setActiveWorkstreamId`] mutates it;
// [`resolveActiveWorkstreamId`] computes the resolved id from a
// `GET /workstreams` response + the pending marker.
// Test: `active-workstream.test.ts`; the form-side reset behavior is
// covered in `StartWorkingForm.test.ts`; the writer sides in
// `WorkstreamActivity.test.ts` / `WorkstreamSwitcher.test.ts`.

import { pendingWorkstream } from './pending-workstream.svelte';

/** The single shared "daemon's resolved active workstream id" value.
 * Read `activeWorkstreamState.id`; never reassign the object itself. */
export const activeWorkstreamState = $state<{ id: string | null }>({
  id: null,
});

/**
 * Record the resolved active-workstream id.
 *
 * Why/What: see the module doc — called by whichever poller
 * (`WorkstreamActivity`/`WorkstreamSwitcher`) just resolved a fresh
 * `GET /workstreams` response. `null` means "the daemon reports no active
 * workstream (and no trustworthy pending fallback exists)".
 * Test: `active-workstream.test.ts`.
 */
export function setActiveWorkstreamId(id: string | null): void {
  activeWorkstreamState.id = id;
}

/**
 * The one shared resolution rule: real active pointer first, then the
 * pending-workstream fallback marker — but only when the marker names a
 * workstream that genuinely exists in THIS response's list.
 *
 * Why: both writers must apply the identical rule or they flip-flop the
 * shared value (module doc). This mirrors the resolution
 * `WorkstreamActivity.svelte::refresh` already performed inline for its own
 * display (code-critic PR #3392 review, MEDIUM); extracting it here makes
 * the switcher apply the same rule without duplicating it.
 * What: pure function of the poll response — no store writes.
 * Test: `active-workstream.test.ts`.
 */
export function resolveActiveWorkstreamId(list: {
  active_workstream_id: string | null;
  workstreams: Array<{ id: string }>;
}): string | null {
  const real = list.workstreams.find((w) => w.id === list.active_workstream_id);
  if (real) return real.id;
  const pending = list.workstreams.find((w) => w.id === pendingWorkstream.id);
  return pending?.id ?? null;
}
