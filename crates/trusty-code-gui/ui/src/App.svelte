<script lang="ts">
  // Why: Minimal scaffold shell (issue #2983, DOC-39) — proves the desktop
  // app connects to the tcode daemon and gives the next UI slice a layout to
  // extend. Phase 1 adds the status bar (DOC-39 §6.2); the rest is still
  // scaffold, not the full DOC-39 screen set.
  // What: Renders a header, a `.body` region (the daemon HealthPanel smoke
  // connection + the session/activity placeholder), and `StatusBar` — the
  // readiness+budget chrome — as a DIRECT SIBLING of `.body`, never nested
  // inside it. DOC-39 §8.1 / AC-18.1 calls this out explicitly: nesting
  // `.statusbar` inside `.body` has regressed the wireframe twice ("This bit
  // us twice… assert it in a test") because it steals the body's row width.
  // `App.test.ts` asserts the DOM invariant this markup encodes.
  // Test: Launch under Tauri or `pnpm dev` in a browser — both show the same
  // connected/disconnected state (DOC-39 §2.1 web/Tauri parity) plus the
  // status bar's readiness indicator. `App.test.ts::statusbar-is-sibling-of-body`
  // pins the structural invariant.
  import HealthPanel from './components/HealthPanel.svelte';
  import ActivityPlaceholder from './components/ActivityPlaceholder.svelte';
  import StatusBar from './components/StatusBar.svelte';
</script>

<main class="app flex min-h-screen flex-col bg-trusty-surface text-trusty-text">
  <header class="p-6 pb-0">
    <h1 class="text-lg font-semibold">trusty-code</h1>
    <p class="text-xs text-trusty-text/60">Desktop shell — thin client over the tcode daemon</p>
  </header>

  <div class="body flex flex-1 flex-col gap-4 p-6">
    <HealthPanel />
    <ActivityPlaceholder />
  </div>

  <StatusBar />
</main>
