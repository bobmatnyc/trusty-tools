<script lang="ts">
  // Why: DOC-39 shell rebuild (issue #3153) — the Workstream tab's active
  // pane, since renamed twice as the entry flow evolved: `CreateSessionForm`
  // (7a picker+prompt) -> `NewWorkstreamForm` (issue #3365/PR #3375,
  // workstream-first, explicit "create workstream" step) -> `StartWorkingForm`
  // (issue #3384, explicit creation step dropped — picking a project and
  // typing a task IS the whole flow). The sibling card renamed the same
  // pass: `SessionMonitor` -> `WorkstreamActivity` (issue #3384 Scope B).
  //
  // **Issue #3446 makes this pane chat-shaped, TUI-style.** Bob: "'Start
  // Working' should be at the bottom of the pane, and the workstream
  // activity above it. Refactor the main chat pane similar to a TUI. Chat
  // at the bottom, enter as a button to the right (or enter), and the
  // stream builds up." The two children swap roles from a stacked pair of
  // independent cards to a fixed layout: `WorkstreamActivity` FILLS the
  // remaining pane height and owns its own internal scrolling (it renders
  // the full turn stream now, not a bounded/truncated tail — see that
  // component's own doc), while `StartWorkingForm` becomes a `shrink-0`
  // bottom-docked input bar, always visible, never scrolled out of view.
  // `App.svelte`'s `.actbody` (`flex-1 overflow-y-auto p-6`) is UNCHANGED —
  // this component alone opts into `h-full flex flex-col` so only ITS
  // internal split is fixed-height; every other tab keeps `.actbody`'s
  // plain document-flow scrolling untouched.
  // What: `h-full flex flex-col` host; `WorkstreamActivity` as `flex-1
  // min-h-0` (self-scrolling, see its own root class), `StartWorkingForm`
  // as `shrink-0` beneath it.
  // Test: `App.test.ts` pins that both mount here by default (the
  // Workstream tab is the shell's initial `activeTab`).
  import StartWorkingForm from './StartWorkingForm.svelte';
  import WorkstreamActivity from './WorkstreamActivity.svelte';
</script>

<div class="flex h-full min-h-0 flex-col">
  <div class="min-h-0 flex-1">
    <WorkstreamActivity />
  </div>
  <StartWorkingForm />
</div>
