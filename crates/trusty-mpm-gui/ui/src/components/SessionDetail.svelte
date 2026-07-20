<script lang="ts">
  // Why: When a session is selected the user needs its full state — identity,
  // status, current agent, circuit breakers, and a scoped event feed — in one
  // panel; this is the main content area driven by `activeSessionId`.
  // What: Resolves the active `Session` from the store, fetches `/breakers`
  // and filters them to this session, and embeds a session-scoped EventFeed.
  // Test: Set `activeSessionId` to a known session → its id/status/uptime
  // render; with the daemon serving breakers for that session, assert the
  // breaker rows appear.
  import { onMount } from 'svelte';
  import { invoke } from '../lib/transport';
  import { sessions, activeSessionId, type Session } from '../stores/app';
  import EventFeed from './EventFeed.svelte';

  /** The currently-selected session, or undefined if none/stale. */
  $: session = $sessions.find((s: Session) => s.id === $activeSessionId);

  /** Circuit breaker rows scoped to this session. */
  let breakers: Array<Record<string, unknown>> = [];

  /** Render uptime seconds compactly. */
  function fmtUptime(secs: number): string {
    const h = Math.floor(secs / 3600);
    const m = Math.floor((secs % 3600) / 60);
    const s = secs % 60;
    if (h > 0) return `${h}h ${m}m ${s}s`;
    if (m > 0) return `${m}m ${s}s`;
    return `${s}s`;
  }

  /**
   * Why: Breaker state is a key health signal; the daemon owns it so we fetch
   * `/breakers` and filter client-side to the active session.
   * What: Invokes `get_breakers` (REST `/breakers`), keeps only entries whose
   * `session_id` matches, and tolerates either an array or `{breakers: []}`.
   * Test: With the daemon returning breakers for two sessions, assert only the
   * active session's breakers are kept.
   */
  async function loadBreakers(id: string): Promise<void> {
    try {
      const raw = await invoke('get_breakers');
      const list: Array<Record<string, unknown>> = Array.isArray(raw)
        ? raw
        : Array.isArray((raw as { breakers?: unknown[] })?.breakers)
          ? ((raw as { breakers: Array<Record<string, unknown>> }).breakers)
          : [];
      breakers = list.filter((b) => b.session_id === id || !b.session_id);
    } catch {
      breakers = [];
    }
  }

  // Reload breakers whenever the selection changes.
  $: if ($activeSessionId) loadBreakers($activeSessionId);

  onMount(() => {
    if ($activeSessionId) loadBreakers($activeSessionId);
  });
</script>

<section class="flex h-full flex-1 flex-col overflow-y-auto p-4">
  {#if !session}
    <p class="opacity-50">Select a session to view its details.</p>
  {:else}
    <h2 class="font-mono text-sm font-semibold">{session.id}</h2>

    <dl class="mt-2 grid grid-cols-[auto_1fr] gap-x-4 gap-y-1 text-xs">
      <dt class="opacity-60">workdir</dt>
      <dd class="font-mono">{session.workdir}</dd>
      <dt class="opacity-60">status</dt>
      <dd>{session.status}</dd>
      <dt class="opacity-60">uptime</dt>
      <dd>{fmtUptime(session.uptime_secs)}</dd>
      {#if session.agent}
        <dt class="opacity-60">agent</dt>
        <dd>{session.agent}</dd>
      {/if}
    </dl>

    <div class="my-3 border-t border-trusty-border"></div>

    <h3 class="text-xs font-semibold uppercase tracking-wide opacity-70">
      Circuit breakers
    </h3>
    {#if breakers.length === 0}
      <p class="py-1 text-xs opacity-50">No breakers for this session.</p>
    {:else}
      <ul class="py-1 text-xs">
        {#each breakers as breaker, i (i)}
          <li class="flex items-center gap-2 py-0.5 font-mono">
            <span class="opacity-70">{breaker.name ?? `breaker-${i}`}</span>
            <span class="opacity-50">{breaker.state ?? 'unknown'}</span>
          </li>
        {/each}
      </ul>
    {/if}

    <div class="my-3 border-t border-trusty-border"></div>

    <div class="min-h-[200px] flex-1">
      <EventFeed sessionId={session.id} />
    </div>
  {/if}
</section>
