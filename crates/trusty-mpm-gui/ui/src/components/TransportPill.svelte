<script lang="ts">
  // Why: Users (and bug reports) benefit from knowing instantly whether they
  // are in the native desktop app or the browser build, since transport and
  // capabilities differ.
  // What: A small pill — rust-accent "Desktop" under Tauri, amber "Web"
  // otherwise — decided once at mount via runtime detection.
  // Test: Stub `window.__TAURI_INTERNALS__` and assert the pill reads
  // "Desktop"; remove it and assert "Web".
  import { isTauri } from '../lib/api-config';

  const desktop = isTauri();
</script>

<span
  class={`rounded-full px-2 py-0.5 text-xs font-medium ${
    desktop
      ? 'bg-trusty-primary/15 text-trusty-primary'
      : 'bg-status-paused/15 text-status-paused'
  }`}
>
  {desktop ? 'Desktop' : 'Web'}
</span>
