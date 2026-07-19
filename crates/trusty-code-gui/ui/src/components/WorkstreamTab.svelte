<script lang="ts">
  // Why: DOC-39 shell rebuild (issue #3153) — the Workstream tab's active
  // pane. The brief's component migration map sends `CreateSessionForm`
  // (the 7a empty-state picker+prompt) and `SessionMonitor` (the 7b
  // hydrated session-status strip) both here. A literal thread+docked-rail
  // ~16/37/28 split (§4.6, mockup 8a) is NOT built in this pass — there is
  // no thread/chat surface anywhere in this codebase yet (a separate,
  // unbuilt feature), and 8a's docked rail specifically needs the SSE
  // consumer tracked as #3251, explicitly deferred by the build brief. This
  // pane instead stacks the two existing, already-shipped cards full-width,
  // identical to their prior placement in `App.svelte` — a structural
  // no-op for their own behavior, just a new host.
  // What: Renders `CreateSessionForm` then `SessionMonitor`, unconditionally
  // (both already degrade to their own honest empty/no-session states).
  // Test: `App.test.ts` pins that both mount here by default (the
  // Workstream tab is the shell's initial `activeTab`).
  import CreateSessionForm from './CreateSessionForm.svelte';
  import SessionMonitor from './SessionMonitor.svelte';
</script>

<div class="space-y-4">
  <CreateSessionForm />
  <SessionMonitor />
</div>
