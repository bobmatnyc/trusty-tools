<script lang="ts">
  // Why: The scaffold's smoke connection to the tcode daemon (issue #2983,
  // DOC-39 §2.1). Per the thin-client rule the Rust side does no computation
  // and no data fetching of its own — this component calls the daemon's
  // `GET /health` directly via `fetch()`, identically whether it runs inside
  // Tauri's webview or a plain browser tab. The only Tauri-native surface
  // used is `apiBase()`, which asks Rust for the configured base URL because
  // only the native process can read `TRUSTY_CODE_URL`.
  // What: On mount (and every `pollMs`), fetches `${base}/health` and renders
  // connected/disconnected plus the raw JSON payload
  // (`{server, version, status}` — see `crates/trusty-code/src/serve/methods.rs`).
  // Test: With `tcode serve --http` running, the panel shows "Connected" and
  // the payload; with it stopped, "Disconnected" and the error message.
  import { onDestroy, onMount } from 'svelte';
  import { apiBase } from '../lib/api-config';

  const pollMs = 5000;

  let base = $state('');
  let connected = $state(false);
  let payload = $state<unknown>(null);
  let error = $state<string | null>(null);

  async function checkHealth() {
    try {
      const resolvedBase = await apiBase();
      base = resolvedBase;
      const response = await fetch(`${resolvedBase}/health`);
      if (!response.ok) {
        throw new Error(`HTTP ${response.status}`);
      }
      payload = await response.json();
      connected = true;
      error = null;
    } catch (e) {
      connected = false;
      payload = null;
      error = e instanceof Error ? e.message : String(e);
    }
  }

  let timer: ReturnType<typeof setInterval> | undefined;

  onMount(() => {
    checkHealth();
    timer = setInterval(checkHealth, pollMs);
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
  });
</script>

<section class="rounded-lg border border-trusty-border bg-trusty-surface/60 p-4">
  <div class="flex items-center justify-between">
    <h2 class="text-sm font-semibold text-trusty-text">tcode daemon</h2>
    <span
      class={`flex items-center gap-1.5 rounded-full px-2 py-0.5 text-xs font-medium ${
        connected ? 'bg-status-ok/15 text-status-ok' : 'bg-status-error/15 text-status-error'
      }`}
    >
      <span class={`h-1.5 w-1.5 rounded-full ${connected ? 'bg-status-ok' : 'bg-status-error'}`}
      ></span>
      {connected ? 'Connected' : 'Disconnected'}
    </span>
  </div>

  <p class="mt-1 text-xs text-trusty-text/60">{base}</p>

  {#if connected}
    <pre class="mt-3 overflow-x-auto rounded bg-black/30 p-2 text-xs text-trusty-text">{JSON.stringify(
        payload,
        null,
        2,
      )}</pre>
  {:else if error}
    <p class="mt-3 text-xs text-status-error">{error}</p>
  {/if}
</section>
