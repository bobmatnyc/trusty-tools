// Why: issue #3447 bug 1 — Bob picked a project via the modal and the
// selection "didn't stick." Root cause (diagnosed in
// `StartWorkingForm.svelte`'s prior `submit()`): `selectedProject` was
// component-LOCAL `$state`, unconditionally reset to `null` at the end of
// EVERY successful submit — so the very next glance at the form (or the
// next submit) already read back "projectless," even though nothing the
// operator did asked for that. Issue #3447 also asks for a SECOND access
// point — a persistent "Projects" section in `WorkstreamRail.svelte` — and
// the coordinator's own framing is exact: "selection persistence +
// workstream continuation are one state model." This module IS that model:
// a small cross-module Svelte 5 rune store (the same officially recommended
// pattern `pending-workstream.svelte.ts` already established in this
// codebase — export a `$state` OBJECT and mutate its properties, never
// reassign the exported binding itself, since only property writes on an
// already-shared object propagate to every importer), so `ProjectPickerModal`
// (via `StartWorkingForm`) and `WorkstreamRail`'s new Projects section write
// to and read from the SAME selection, and neither one silently resets it.
//
// What: [`selectedProjectState`] holds the current pick (or `null` for
// projectless); [`selectProject`] is the one write path both callers use.
// Persistence itself is enforced by ABSENCE — `StartWorkingForm.svelte`'s
// `submit()` no longer clears this store on success (issue #3447's actual
// fix); this module only owns the shared VALUE, not when it changes.
// Chat-continuation's own reset rule (issue #3446 — changing the selected
// project mid-conversation starts a new workstream on the next submit, since
// a session's project binding is immutable once set) lives in
// `StartWorkingForm.svelte` as a `$effect` that watches this store's `path`
// and resets its LOCAL continuation state (`lastSessionId`/
// `continuationWorkstreamId`) when it changes — kept out of this module
// because continuation state has exactly one reader/writer (the input bar),
// unlike the selection itself.
// Test: `selected-project.test.ts`.

import type { ProjectSelection } from './new-workstream';

export const selectedProjectState = $state<{ project: ProjectSelection | null }>({
  project: null,
});

/**
 * Set (or clear, via `null`) the shared project selection.
 *
 * Why: the one write path every caller (the picker modal, the rail's
 * Projects section, the input bar's own "clear" button) goes through —
 * kept as a function rather than direct property assignment at call sites
 * so a future invariant (e.g. logging, validation) has one place to live.
 * Test: `selected-project.test.ts::selectProject`.
 */
export function selectProject(project: ProjectSelection | null): void {
  selectedProjectState.project = project;
}
