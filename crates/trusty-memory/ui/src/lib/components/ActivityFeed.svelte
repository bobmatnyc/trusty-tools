<script>
  /*
   * Why: The daemon broadcasts DrawerAdded / PalaceCreated / DreamCompleted
   * / StatusChanged over `/sse`. Issue #96 adds a persistent activity log
   * (redb) plus `GET /api/v1/activity?limit=&offset=&palace=&source=` so
   * the feed can hydrate with history on page load instead of waiting for
   * the next live event. The same endpoint tags every row with its origin
   * (Http/Mcp/Hook) so the UI can render a source badge per row — MCP
   * writes used to be invisible because only the HTTP path emitted.
   * What: On mount we fetch the first page, render it, and connect to
   * `/sse` for live-tail updates. Scrolling near the bottom of the feed
   * fetches the next page. Filters by palace name substring; "Active only"
   * hides palaces with no activity this session. Auto-reconnects SSE with
   * exponential backoff on error.
   * Test: open the SPA, see historical entries on first paint; create a
   * drawer via MCP (`mcp__trusty-memory__memory_remember`) and confirm a
   * new row appears with the `mcp` badge. Scroll near the bottom and watch
   * the network tab for `/api/v1/activity?offset=…` requests.
   */
  import { onMount, onDestroy } from 'svelte';
  import { api } from '../api.js';
  import { apiUrl } from '../base.js';

  // Live in-memory buffer for SSE-pushed events. Caps prevent unbounded
  // growth in long-running sessions; the persistent history endpoint
  // owns the real archive.
  const MAX_EVENTS = 500;
  const PAGE_SIZE = 50;
  // Exponential backoff: 1s, 2s, 4s, 8s, ..., capped at 30s.
  const BACKOFF_MIN_MS = 1000;
  const BACKOFF_MAX_MS = 30_000;
  // Pixel buffer from the bottom of the feed that triggers a page fetch.
  const SCROLL_BUFFER_PX = 100;

  // `events` holds the merged history + live tail, newest-first.
  let events = $state([]);
  // Smallest entry id seen so far — used to dedupe live SSE pushes against
  // already-loaded history and to compute the next paging offset.
  let smallestSeenId = $state(null);
  let connected = $state(false);
  let backoffMs = BACKOFF_MIN_MS;
  let collapsed = $state(false);
  let source = null;
  let reconnectTimer = null;

  // Paging state for the persistent history.
  let historyOffset = $state(0);
  let historyTotal = $state(0);
  let loadingPage = $state(false);
  let endReached = $state(false);

  // Filter UI state.
  let filterText = $state('');
  let activeOnly = $state(false);

  // id -> name lookup, populated at mount from /api/v1/palaces. Used for
  // events whose `palace_name` field is missing (older daemons).
  let palaceNames = $state({});

  // Set of palace ids that have emitted at least one event this session.
  // Used by the "Active only" toggle.
  let activeIds = $state(new Set());

  // Ref to the scrolling container so we can attach a scroll handler.
  let feedBodyEl = $state(null);

  /**
   * Why: history rows from `/api/v1/activity` carry a real id; live SSE
   * frames do not. We synthesise a stable id for live frames so Svelte's
   * keyed `{#each}` loop dedupes correctly when the same write produces
   * both a live frame and a later history hit (small race window).
   * What: returns the row's persisted id if present; otherwise a
   * synthetic string keyed on the event timestamp + random suffix.
   */
  function rowKey(evt) {
    if (typeof evt.id === 'number') return `db-${evt.id}`;
    return evt._id;
  }

  /**
   * Why: Normalise an activity row (from either the history endpoint or
   * the live SSE stream) to a common shape the renderer expects. The
   * history row nests the SSE payload under `payload`; we flatten it so
   * the existing `describe()` logic — written for the SSE shape — still
   * works without per-source branches.
   * What: returns `{ ...payload, type, source, palace_id, _ts, _id, id }`
   * with sensible defaults. The `source` field is preserved verbatim
   * (lower-case 'http' | 'mcp' | 'hook').
   */
  function normaliseHistoryRow(row) {
    const payload = row.payload || {};
    return {
      ...payload,
      type: row.event_type,
      source: row.source,
      palace_id: row.palace_id ?? payload.palace_id ?? null,
      timestamp: row.timestamp ?? payload.timestamp,
      _ts: new Date(row.timestamp).getTime(),
      _id: `db-${row.id}`,
      id: row.id
    };
  }

  /**
   * Why: New events should appear at the top; old events fall off when we
   * exceed the cap. Records palace id as "session-active" so the Active
   * toggle has data to filter on.
   */
  function prependLive(evt) {
    const stamped = { ...evt, _ts: Date.now(), _id: `live-${Date.now()}-${Math.random()}` };
    events = [stamped, ...events].slice(0, MAX_EVENTS);
    const pid = evt?.palace_id;
    if (pid && !activeIds.has(pid)) {
      const next = new Set(activeIds);
      next.add(pid);
      activeIds = next;
    }
  }

  /**
   * Why: Replace or append a batch of history rows. Used at mount for
   * page 1 and on scroll for later pages. Tracks the smallest-id seen so
   * paging can compute the right offset.
   */
  function appendHistory(rows) {
    if (!rows || rows.length === 0) {
      endReached = true;
      return;
    }
    const normalised = rows.map(normaliseHistoryRow);
    events = [...events, ...normalised];
    for (const r of normalised) {
      if (r.palace_id && !activeIds.has(r.palace_id)) {
        const next = new Set(activeIds);
        next.add(r.palace_id);
        activeIds = next;
      }
      if (smallestSeenId === null || (typeof r.id === 'number' && r.id < smallestSeenId)) {
        smallestSeenId = r.id;
      }
    }
  }

  async function loadHistoryPage(offset = 0) {
    if (loadingPage || endReached) return;
    loadingPage = true;
    try {
      const data = await api.listActivity({ limit: PAGE_SIZE, offset });
      historyTotal = data?.total ?? 0;
      appendHistory(data?.entries || []);
      historyOffset = offset + (data?.entries?.length || 0);
      // If the server returned fewer than requested, we are at the end.
      if (!data?.entries || data.entries.length < PAGE_SIZE) {
        endReached = true;
      }
      // If we've loaded everything reported, mark done.
      if (historyOffset >= historyTotal) {
        endReached = true;
      }
    } catch (e) {
      // Surface in the console; the feed still works in live-tail mode.
      console.warn('activity history fetch failed', e);
    } finally {
      loadingPage = false;
    }
  }

  function onFeedScroll() {
    if (!feedBodyEl) return;
    const remaining =
      feedBodyEl.scrollHeight - feedBodyEl.scrollTop - feedBodyEl.clientHeight;
    if (remaining < SCROLL_BUFFER_PX) {
      loadHistoryPage(historyOffset);
    }
  }

  function connect() {
    if (source) {
      try {
        source.close();
      } catch {
        /* ignore */
      }
      source = null;
    }

    try {
      source = new EventSource(apiUrl('/sse'));
    } catch (e) {
      scheduleReconnect();
      return;
    }

    source.onopen = () => {
      connected = true;
      backoffMs = BACKOFF_MIN_MS;
    };

    source.onmessage = (msg) => {
      let parsed;
      try {
        parsed = JSON.parse(msg.data);
      } catch {
        return;
      }
      // Initial connect frame from the daemon — informational only.
      if (parsed?.type === 'connected') return;
      prependLive(parsed);
    };

    source.onerror = () => {
      connected = false;
      try {
        source?.close();
      } catch {
        /* ignore */
      }
      source = null;
      scheduleReconnect();
    };
  }

  function scheduleReconnect() {
    if (reconnectTimer) return;
    reconnectTimer = setTimeout(() => {
      reconnectTimer = null;
      connect();
    }, backoffMs);
    backoffMs = Math.min(backoffMs * 2, BACKOFF_MAX_MS);
  }

  /**
   * Why: Older daemons don't include `palace_name` in DrawerAdded frames;
   * the feed needs a fallback id→name table so the description stays
   * readable.
   * What: Fetches `/api/v1/palaces` once at mount, builds a id→name map.
   */
  async function loadPalaceNames() {
    try {
      const list = await api.listPalaces();
      const map = {};
      for (const p of list || []) {
        if (p?.id) map[p.id] = p.name || p.id;
      }
      palaceNames = map;
    } catch {
      // Best-effort; feed still works with raw palace ids.
    }
  }

  onMount(() => {
    loadPalaceNames();
    // Kick off history hydration before connecting to SSE so the first
    // paint of the feed has rows. Live frames that arrive while the
    // history is loading will simply prepend on top.
    loadHistoryPage(0);
    connect();
  });

  onDestroy(() => {
    if (reconnectTimer) {
      clearTimeout(reconnectTimer);
      reconnectTimer = null;
    }
    if (source) {
      try {
        source.close();
      } catch {
        /* ignore */
      }
      source = null;
    }
  });

  /**
   * Why: Raw epoch ms is unreadable; operators want "2s ago" / "3m ago".
   * What: Returns a short relative-time string. Accepts either a numeric
   * epoch ms or an ISO-8601 string (for `timestamp` field on DrawerAdded).
   */
  function relTime(ts) {
    let t;
    if (typeof ts === 'string') {
      t = new Date(ts).getTime();
    } else {
      t = ts;
    }
    if (!Number.isFinite(t)) return '—';
    const diff = Math.max(0, Date.now() - t);
    const s = Math.floor(diff / 1000);
    if (s < 60) return `${s}s ago`;
    const m = Math.floor(s / 60);
    if (m < 60) return `${m}m ago`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h ago`;
    return `${Math.floor(h / 24)}d ago`;
  }

  function labelFor(evt) {
    return evt?.palace_name || palaceNames[evt?.palace_id] || evt?.palace_id || '';
  }

  /**
   * Why: Per-event-type rendering keeps the feed scannable; emoji + short
   * description matches the spec.
   */
  function describe(evt) {
    switch (evt.type) {
      case 'drawer_added': {
        const name = labelFor(evt);
        return {
          icon: '🧠',
          label: 'drawer',
          description: `${name} — ${evt.drawer_count} drawers`,
          link: evt?.palace_id ? `#/palaces/${evt.palace_id}` : null
        };
      }
      case 'drawer_deleted': {
        const name = labelFor(evt);
        return {
          icon: '🗑',
          label: 'drawer',
          description: `${name} — ${evt.drawer_count} drawers`,
          link: evt?.palace_id ? `#/palaces/${evt.palace_id}` : null
        };
      }
      case 'palace_created':
        return {
          icon: '🏛',
          label: 'palace',
          description: `${evt.name ?? evt.id} created`,
          link: evt?.id ? `#/palaces/${evt.id}` : null
        };
      case 'dream_completed':
        return {
          icon: '💭',
          label: 'dream',
          description: `merged ${evt.merged ?? 0}, pruned ${evt.pruned ?? 0}, compacted ${evt.compacted ?? 0}`,
          link: null
        };
      case 'status_changed':
        return {
          icon: '⚡',
          label: 'status',
          description: `${evt.total_drawers ?? 0} drawers · ${evt.total_vectors ?? 0} vectors · ${evt.total_kg_triples ?? 0} triples`,
          link: null
        };
      case 'lag':
        return {
          icon: '⏱',
          label: 'lag',
          description: `dropped ${evt.skipped ?? 0} events (slow consumer)`,
          link: null
        };
      default:
        return {
          icon: '·',
          label: evt.type ?? 'event',
          description: JSON.stringify(evt).slice(0, 120),
          link: null
        };
    }
  }

  // Periodically force re-render so relative timestamps tick.
  let _tick = $state(0);
  let tickTimer = null;
  onMount(() => {
    tickTimer = setInterval(() => {
      _tick += 1;
    }, 1000);
  });
  onDestroy(() => {
    if (tickTimer) clearInterval(tickTimer);
  });

  // Derived list applying filter + activeOnly.
  let visibleEvents = $derived.by(() => {
    const f = filterText.trim().toLowerCase();
    return events.filter((evt) => {
      if (activeOnly) {
        const pid = evt?.palace_id;
        if (pid && !activeIds.has(pid)) return false;
      }
      if (!f) return true;
      const haystack = `${evt?.palace_id ?? ''} ${labelFor(evt)} ${evt?.name ?? ''}`.toLowerCase();
      return haystack.includes(f);
    });
  });

  function bestTime(evt) {
    return evt?.timestamp || evt?._ts;
  }

  /**
   * Why (issue #96): the source badge needs a deterministic CSS class so
   * each origin renders in a distinct color (HTTP=blue, MCP=purple,
   * HOOK=amber). Defaulting to 'http' matches the previous behaviour where
   * every event was implicitly from the HTTP path.
   * What: returns the canonical lower-case origin string.
   */
  function sourceLabel(evt) {
    const s = (evt?.source || '').toLowerCase();
    if (s === 'http' || s === 'mcp' || s === 'hook') return s;
    return null;
  }
</script>

<aside class="feed" class:collapsed>
  <div class="feed-head">
    <div class="title">
      <span class="dot" class:online={connected} class:offline={!connected}></span>
      Activity
    </div>
    <div class="head-right">
      <span class="status-text">
        {#if connected}live{:else}reconnecting…{/if}
      </span>
      <button
        type="button"
        class="toggle"
        aria-label="Toggle activity feed"
        onclick={() => (collapsed = !collapsed)}
      >
        {collapsed ? '▸' : '▾'}
      </button>
    </div>
  </div>

  {#if !collapsed}
    <div class="feed-controls">
      <input
        type="search"
        class="filter-input"
        placeholder="🔍 Filter by palace…"
        bind:value={filterText}
      />
      <label class="toggle-row" title="Hide palaces with no activity this session">
        <input type="checkbox" bind:checked={activeOnly} />
        <span>Active only</span>
      </label>
    </div>

    <div class="feed-body" bind:this={feedBodyEl} onscroll={onFeedScroll}>
      {#if visibleEvents.length === 0}
        <div class="empty">
          {#if events.length === 0}
            {#if loadingPage}
              Loading history…
            {:else if connected}
              Waiting for events…
            {:else}
              Disconnected — retrying.
            {/if}
          {:else}
            No events match the filter.
          {/if}
        </div>
      {:else}
        <ul class="event-list">
          {#each visibleEvents as evt (rowKey(evt))}
            {@const d = describe(evt)}
            {@const src = sourceLabel(evt)}
            <li class="event-row">
              <span class="icon" aria-hidden="true">{d.icon}</span>
              <div class="event-main">
                <div class="event-line">
                  <span class="badge">{d.label}</span>
                  {#if src}
                    <span class="src-badge src-{src}" title="Origin: {src}">
                      {src}
                    </span>
                  {/if}
                  {#if d.link}
                    <a class="desc link" href={d.link}>{d.description}</a>
                  {:else}
                    <span class="desc">{d.description}</span>
                  {/if}
                </div>
                <!-- _tick is referenced so Svelte re-renders this row each second -->
                <div class="event-time">{relTime(bestTime(evt))}{_tick > -1 ? '' : ''}</div>
              </div>
            </li>
          {/each}
          {#if loadingPage}
            <li class="loading-row">Loading more…</li>
          {:else if endReached && historyTotal > 0}
            <li class="loading-row dim">{historyTotal} total events</li>
          {/if}
        </ul>
      {/if}
    </div>
  {/if}
</aside>

<style>
  .feed {
    position: fixed;
    right: 0;
    top: 0;
    bottom: 0;
    width: 320px;
    background: var(--trusty-content-bg, #fff);
    border-left: 1px solid var(--trusty-border, #e5e7eb);
    display: flex;
    flex-direction: column;
    z-index: 10;
    transition: width 0.2s ease;
  }
  .feed.collapsed {
    width: 200px;
  }
  .feed-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 12px 16px;
    border-bottom: 1px solid var(--trusty-border, #e5e7eb);
    font-size: 13px;
    font-weight: 600;
    background: var(--trusty-bg-subtle, #fafafa);
  }
  .feed-controls {
    display: flex;
    flex-direction: column;
    gap: 6px;
    padding: 8px 12px;
    border-bottom: 1px solid var(--trusty-border, #e5e7eb);
    background: var(--trusty-bg-subtle, #fafafa);
  }
  .filter-input {
    width: 100%;
    padding: 4px 8px;
    border-radius: 4px;
    border: 1px solid var(--trusty-border, #e5e7eb);
    font-size: 12px;
  }
  .toggle-row {
    display: flex;
    align-items: center;
    gap: 6px;
    font-size: 11px;
    color: var(--trusty-text-secondary, #6b7280);
    cursor: pointer;
  }
  .title {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .head-right {
    display: flex;
    align-items: center;
    gap: 8px;
  }
  .status-text {
    font-size: 11px;
    color: var(--trusty-text-secondary, #6b7280);
    font-weight: 400;
  }
  .toggle {
    background: transparent;
    border: none;
    color: var(--trusty-text-secondary, #6b7280);
    cursor: pointer;
    font-size: 12px;
    padding: 2px 4px;
  }
  .dot {
    width: 8px;
    height: 8px;
    border-radius: 50%;
  }
  .dot.online {
    background: var(--trusty-success, #10b981);
    box-shadow: 0 0 4px rgba(16, 185, 129, 0.5);
  }
  .dot.offline {
    background: var(--trusty-warning, #f59e0b);
  }
  .feed-body {
    flex: 1;
    overflow-y: auto;
    padding: 8px 0;
  }
  .empty {
    text-align: center;
    color: var(--trusty-text-muted, #9ca3af);
    font-size: 12px;
    padding: 24px 16px;
  }
  .event-list {
    list-style: none;
    margin: 0;
    padding: 0;
  }
  .event-row {
    display: flex;
    align-items: flex-start;
    gap: 10px;
    padding: 8px 16px;
    border-bottom: 1px solid var(--trusty-border-light, #f3f4f6);
    font-size: 12px;
  }
  .event-row:last-child {
    border-bottom: none;
  }
  .icon {
    flex: 0 0 18px;
    font-size: 14px;
    line-height: 18px;
  }
  .event-main {
    flex: 1;
    min-width: 0;
  }
  .event-line {
    display: flex;
    align-items: center;
    gap: 6px;
    flex-wrap: wrap;
  }
  .badge {
    display: inline-block;
    padding: 1px 6px;
    border-radius: 4px;
    background: var(--trusty-bg-subtle, #f3f4f6);
    color: var(--trusty-text-secondary, #6b7280);
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  /* Origin badges — small coloured pills next to the event-type badge.
     Why: lets operators tell at a glance whether a write came from
     the HTTP API, an MCP tool call, or a future hook integration. */
  .src-badge {
    display: inline-block;
    padding: 1px 6px;
    border-radius: 4px;
    font-size: 10px;
    text-transform: uppercase;
    letter-spacing: 0.04em;
    border: 1px solid transparent;
  }
  .src-http {
    color: #1d4ed8;
    background: #dbeafe;
    border-color: #bfdbfe;
  }
  .src-mcp {
    color: #6d28d9;
    background: #ede9fe;
    border-color: #ddd6fe;
  }
  .src-hook {
    color: #92400e;
    background: #fef3c7;
    border-color: #fde68a;
  }
  .desc {
    color: var(--trusty-text, #111827);
    overflow: hidden;
    text-overflow: ellipsis;
    word-break: break-word;
  }
  .desc.link {
    color: var(--trusty-accent, #4f46e5);
    text-decoration: none;
  }
  .desc.link:hover {
    text-decoration: underline;
  }
  .event-time {
    color: var(--trusty-text-muted, #9ca3af);
    font-size: 10px;
    margin-top: 2px;
  }
  .loading-row {
    list-style: none;
    padding: 12px 16px;
    text-align: center;
    color: var(--trusty-text-muted, #9ca3af);
    font-size: 11px;
  }
  .loading-row.dim {
    color: var(--trusty-text-muted, #9ca3af);
    font-style: italic;
  }
</style>
