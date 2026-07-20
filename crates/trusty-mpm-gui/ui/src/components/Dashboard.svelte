<script lang="ts">
  // Why: The Tauri shell (App.svelte) and the browser shell (WebApp.svelte)
  // render an identical component tree; factoring it here keeps the two
  // entrypoints to a one-line difference and avoids layout duplication.
  // What: Composes Header + a dismissable SessionList sidebar + the
  // CoordinatorChat as the permanent main panel, and runs a `refreshSessions`
  // poll loop while mounted. SessionDetail / standalone EventFeed are retired
  // from the main panel — the coordinator chat is always shown.
  // Test: Mount with the daemon up → sessions populate within one poll tick
  // and the coordinator chat panel is visible; hide the sidebar → only the
  // toggle rail remains, the chat panel still fills the rest.
  import { onDestroy, onMount } from 'svelte';
  import { refreshSessions, sidebarVisible } from '../stores/app';
  import { Menu } from 'lucide-svelte';
  import Header from './Header.svelte';
  import SessionList from './SessionList.svelte';
  import CoordinatorChat from './CoordinatorChat.svelte';

  /** Session poll interval in ms. */
  const POLL_MS = 3000;

  let timer: ReturnType<typeof setInterval> | undefined;

  onMount(() => {
    refreshSessions();
    timer = setInterval(refreshSessions, POLL_MS);
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
  });
</script>

<div class="flex h-screen flex-col">
  <Header />

  <div class="flex min-h-0 flex-1 overflow-hidden">
    <!-- Sidebar toggle rail (shown when the sidebar is hidden) -->
    {#if !$sidebarVisible}
      <button
        type="button"
        on:click={() => sidebarVisible.set(true)}
        aria-label="Show sidebar"
        class="flex w-8 flex-none items-center justify-center border-r border-trusty-border opacity-60 hover:opacity-100"
      >
        <Menu size={16} />
      </button>
    {/if}

    <!-- Dismissable sidebar -->
    {#if $sidebarVisible}
      <aside
        class="w-64 flex-none overflow-y-auto border-r border-trusty-border"
      >
        <SessionList />
      </aside>
    {/if}

    <!-- Coordinator chat — the permanent main panel -->
    <main class="min-h-0 flex-1 overflow-hidden">
      <CoordinatorChat />
    </main>
  </div>
</div>
