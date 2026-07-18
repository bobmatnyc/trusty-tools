<script lang="ts">
  // Why: Minimal scaffold shell (issue #2983, DOC-39) — proves the desktop
  // app connects to the tcode daemon and gives the next UI slice a layout to
  // extend. Not the full DOC-39 screens.
  // What: Renders a header, the daemon HealthPanel (GET /health smoke
  // connection), and a placeholder for the future session list.
  // Test: Launch under Tauri or `pnpm dev` in a browser — both show the same
  // connected/disconnected state (DOC-39 §2.1 web/Tauri parity).
  import HealthPanel from './components/HealthPanel.svelte';
  import ActivityPlaceholder from './components/ActivityPlaceholder.svelte';
</script>

<main class="flex min-h-screen flex-col gap-4 bg-trusty-surface p-6 text-trusty-text">
  <header>
    <h1 class="text-lg font-semibold">trusty-code</h1>
    <p class="text-xs text-trusty-text/60">Desktop shell — thin client over the tcode daemon</p>
  </header>

  <HealthPanel />
  <ActivityPlaceholder />
</main>
