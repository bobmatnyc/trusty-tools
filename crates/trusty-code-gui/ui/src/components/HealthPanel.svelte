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
  // The payload's `<pre>` block uses the themed `trusty-border` token at
  // 20% opacity rather than the raw, non-themed Tailwind near-black it
  // previously hardcoded at 30% opacity (issue #3133 theming audit) — that
  // built-in shade never changes with `prefers-color-scheme`, so on a
  // light theme the code block would have stayed a dark box regardless of
  // the surrounding page; the themed border token tints appropriately in
  // both schemes.
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

<section class="rounded border-1.5 border-trusty-border bg-trusty-card">
  <div
    class="flex items-center justify-between border-b border-trusty-border bg-trusty-raised px-4 py-2.5"
  >
    <h2 class="font-display text-xs font-bold uppercase tracking-wide text-trusty-text">
      tcode daemon
    </h2>
    <span
      class={`rounded-sm px-2 py-0.5 font-mono text-[10px] font-semibold uppercase tracking-wide ${
        connected ? 'bg-status-ok/15 text-status-ok' : 'bg-status-error/15 text-status-error'
      }`}
    >
      {connected ? 'connected' : 'disconnected'}
    </span>
  </div>

  <div class="p-4">
    <p class="font-mono text-xs text-trusty-text-muted">{base}</p>

    {#if connected}
      <pre
        class="mt-3 overflow-x-auto rounded-sm border border-trusty-border bg-trusty-raised p-2 font-mono text-xs text-trusty-text">{JSON.stringify(
          payload,
          null,
          2,
        )}</pre>
    {:else if error}
      <p class="mt-3 text-xs text-status-error">{error}</p>
    {/if}
  </div>
</section>
