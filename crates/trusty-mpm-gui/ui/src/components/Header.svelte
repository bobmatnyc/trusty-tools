<script lang="ts">
  // Why: A persistent top bar gives the user an anchor for branding, daemon
  // liveness, the sidebar toggle, and global controls (transport mode, theme)
  // regardless of which session is selected.
  // What: Renders a hamburger sidebar toggle, the "trusty-mpm" wordmark, a
  // colored daemon-health dot bound to the `daemonHealth` store, the
  // TransportPill, and the ThemeToggle.
  // Test: Set `daemonHealth` to each of ok/connecting/error and assert the dot
  // color updates; click the hamburger and assert `sidebarVisible` flips.
  import { Menu } from 'lucide-svelte';
  import { daemonHealth, sidebarVisible } from '../stores/app';
  import TransportPill from './TransportPill.svelte';
  import ThemeToggle from './ThemeToggle.svelte';

  $: dotTone =
    $daemonHealth === 'ok'
      ? 'bg-status-running'
      : $daemonHealth === 'connecting'
        ? 'bg-status-paused'
        : 'bg-status-error';
</script>

<header
  class="flex items-center gap-3 border-b border-trusty-border bg-trusty-surface px-4 py-2"
>
  <button
    type="button"
    on:click={() => sidebarVisible.update((v) => !v)}
    aria-label="Toggle sidebar"
    class="-ml-1 rounded p-1 opacity-70 hover:opacity-100"
  >
    <Menu size={16} />
  </button>
  <span class="text-sm font-semibold tracking-tight">trusty-mpm</span>
  <span
    class={`h-2.5 w-2.5 rounded-full ${dotTone}`}
    title={`daemon: ${$daemonHealth}`}
  ></span>

  <div class="flex-1"></div>

  <TransportPill />
  <ThemeToggle />
</header>
