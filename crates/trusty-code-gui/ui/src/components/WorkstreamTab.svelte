<script lang="ts">
  // Why: DOC-39 shell rebuild (issue #3153) — the Workstream tab's active
  // pane, since renamed twice as the entry flow evolved: `CreateSessionForm`
  // (7a picker+prompt) -> `NewWorkstreamForm` (issue #3365/PR #3375,
  // workstream-first, explicit "create workstream" step) -> `StartWorkingForm`
  // (issue #3384, THIS pass — the explicit creation step is gone; picking a
  // project and typing a task IS the whole flow, per Bob's "we shouldn't need
  // to 'create' a workstream, it should be inferred" direction). The sibling
  // card renamed the same pass: `SessionMonitor` -> `WorkstreamActivity`
  // (issue #3384 Scope B — reframed around the ACTIVE WORKSTREAM rather than
  // "whichever session happens to be running", per Bob's "'no active session
  // to monitor' — this doesn't make sense in the workstream context"). A
  // literal thread+docked-rail ~16/37/28 split (§4.6, mockup 8a) is still NOT
  // built in this pass — there is no thread/chat surface anywhere in this
  // codebase yet (a separate, unbuilt feature), and 8a's docked rail
  // specifically needs the SSE consumer tracked as #3251, explicitly
  // deferred by the build brief. This pane instead stacks the two cards
  // full-width, identical to their prior placement in `App.svelte` — a
  // structural no-op for their own behavior, just a new host.
  // What: Renders `StartWorkingForm` then `WorkstreamActivity`, unconditionally
  // (both already degrade to their own honest empty states).
  // Test: `App.test.ts` pins that both mount here by default (the
  // Workstream tab is the shell's initial `activeTab`).
  import StartWorkingForm from './StartWorkingForm.svelte';
  import WorkstreamActivity from './WorkstreamActivity.svelte';
</script>

<div class="space-y-4">
  <StartWorkingForm />
  <WorkstreamActivity />
</div>
