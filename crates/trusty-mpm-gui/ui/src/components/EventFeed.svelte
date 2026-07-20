<script lang="ts">
  // Why: Operators need a live view of what the fleet is doing; an SSE-backed
  // feed surfaces hook events as they happen without polling.
  // What: Subscribes to the daemon SSE stream (global or session-scoped),
  // pushes events into the `events` store, and renders a filterable list.
  // Test: With the daemon emitting events, mount this and assert rows appear;
  // type into the filter and assert only matching event types remain; unmount
  // and assert the EventSource is closed.
  import { onDestroy } from 'svelte';
  import { subscribeEvents } from '../lib/transport';
  import { events, pushEvent, type HookEvent } from '../stores/app';

  /** When set, scope the stream to a single session. */
  export let sessionId: string | null = null;

  /** Free-text filter applied to event type and session id. */
  let filter = '';

  let unsubscribe: (() => void) | null = null;

  /** (Re)open the SSE stream whenever the scope changes. */
  $: {
    unsubscribe?.();
    unsubscribe = subscribeEvents(sessionId, (ev: HookEvent) => pushEvent(ev));
  }

  onDestroy(() => unsubscribe?.());

  /** Format a unix-ms or ISO timestamp as a wall-clock time. */
  function fmtTime(ts: number | string): string {
    const d = typeof ts === 'number' ? new Date(ts) : new Date(ts);
    return Number.isNaN(d.getTime()) ? String(ts) : d.toLocaleTimeString();
  }

  // Why: Cheap client-side filtering keeps the feed scannable during bursts.
  // What: Matches the filter substring against event type and session id.
  // Test: With filter "tool", assert only events whose type contains "tool"
  // show.
  $: visible = $events.filter((e) => {
    if (sessionId && e.session_id && e.session_id !== sessionId) return false;
    if (!filter) return true;
    const haystack = `${e.event_type} ${e.session_id ?? ''}`.toLowerCase();
    return haystack.includes(filter.toLowerCase());
  });
</script>

<div class="flex h-full flex-col">
  <div class="flex items-center gap-2 px-1 pb-2">
    <span class="text-xs font-semibold uppercase tracking-wide opacity-70">
      Events
    </span>
    <input
      type="text"
      bind:value={filter}
      placeholder="filter…"
      class="ml-auto w-40 rounded border border-trusty-border bg-transparent px-2 py-0.5 text-xs"
    />
  </div>

  <div class="flex-1 overflow-y-auto font-mono text-[11px]">
    {#if visible.length === 0}
      <p class="px-1 py-2 opacity-50">No events.</p>
    {/if}
    {#each visible as event, i (i)}
      <div
        class="flex items-center gap-2 border-b border-trusty-border/50 py-1"
      >
        <span class="rounded bg-trusty-primary/15 px-1.5 py-0.5 text-trusty-primary">
          {event.event_type}
        </span>
        <span class="opacity-60">{fmtTime(event.timestamp)}</span>
        {#if event.session_id}
          <span class="ml-auto truncate opacity-50">{event.session_id}</span>
        {/if}
      </div>
    {/each}
  </div>
</div>
