// Picker-catalog refetch trigger (agent-picker cold-start race, owner report
// 2026-07-23).
//
// Why: `AgentSwitcher`/`ModelSwitcher` fetch their catalog once in `onMount`,
// but `<Header>` — and therefore both pickers — renders before `apiReady`. On
// a cold start the sidecar isn't listening yet, so that first fetch fails, the
// catalog stores stay empty, and the pickers show only their built-in default
// for the whole session with no retry. `App.svelte` re-drives the catalog
// loads when the API becomes healthy — but the trigger must be EDGE-triggered
// (fire exactly on the false→true transition), not level-triggered, so it runs
// at most once and never on an unrelated reactive re-evaluation. Extracted as a
// pure function so that contract is unit-testable without mounting `App.svelte`
// — mirroring `taskHistory.ts`/`roster.ts`.
// Test: `catalogRefetch.test.ts`.

/**
 * Why: the reactive block in `App.svelte` re-evaluates whenever any of its
 * dependencies change; we only want to (re)load the picker catalogs on the
 * moment the API goes from not-ready to ready, not on every re-evaluation.
 * What: Returns `true` iff `apiReady` just transitioned from `false` to
 * `true` (i.e. `apiReady && !prevApiReady`). Pure — the caller owns the
 * `prevApiReady` bookkeeping. As `apiReady` is set `true` exactly once per app
 * lifetime and never reset to `false`, this fires at most once; if a future
 * change ever resets it (e.g. an explicit disconnect), the same edge logic
 * would correctly fire again on the next reconnect.
 * Test: `catalogRefetch.test.ts`.
 */
export function shouldRefetchCatalogs(
  prevApiReady: boolean,
  apiReady: boolean,
): boolean {
  return apiReady && !prevApiReady;
}
