<script>
  /*
   * Why: Both trusty-search and trusty-memory expose a `/logs/tail` ring
   * buffer; a single reusable viewer avoids duplicating the polling, level
   * filter, search, and auto-scroll logic in two places.
   * What: Polls a caller-supplied `fetchLogs()` every 2s, parses the
   * `[LEVEL]` prefix for client-side level filtering, supports a free-text
   * filter and an auto-scroll toggle. Monospace, newest-at-bottom.
   * Test: pass a stub `fetchLogs` returning lines with mixed levels, toggle
   * the ERROR filter, confirm only ERROR lines render.
   */
  import { onMount, onDestroy } from 'svelte';

  /** @type {{ fetchLogs: () => Promise<{lines: string[], total: number}> }} */
  let { fetchLogs } = $props();

  let lines = $state([]);
  let total = $state(0);
  let error = $state(null);
  let levelFilter = $state('ALL');
  let textFilter = $state('');
  let autoScroll = $state(true);
  let timer = null;
  let scrollEl;

  const LEVELS = ['ALL', 'INFO', 'WARN', 'ERROR'];

  /**
   * Why: tracing lines embed the level as `LEVEL` or `[LEVEL]`; we need it for
   * client-side filtering and row colouring.
   * What: returns the uppercased level token found in the line, or 'INFO'.
   * Test: lineLevel("2024 WARN foo") === "WARN".
   */
  function lineLevel(line) {
    const m = line.match(/\b(ERROR|WARN|INFO|DEBUG|TRACE)\b/);
    return m ? m[1] : 'INFO';
  }

  async function refresh() {
    try {
      const body = await fetchLogs();
      lines = body?.lines || [];
      total = body?.total ?? lines.length;
      error = null;
    } catch (e) {
      error = e.message || String(e);
    }
  }

  let visibleLines = $derived.by(() => {
    const needle = textFilter.trim().toLowerCase();
    return lines.filter((l) => {
      if (levelFilter !== 'ALL' && lineLevel(l) !== levelFilter) return false;
      if (needle && !l.toLowerCase().includes(needle)) return false;
      return true;
    });
  });

  // Auto-scroll to bottom whenever the visible set changes (if enabled).
  $effect(() => {
    void visibleLines;
    if (autoScroll && scrollEl) {
      queueMicrotask(() => {
        if (scrollEl) scrollEl.scrollTop = scrollEl.scrollHeight;
      });
    }
  });

  onMount(() => {
    refresh();
    timer = setInterval(refresh, 2000);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });
</script>

<div class="card">
  <div class="card-header log-toolbar">
    <div class="level-buttons">
      {#each LEVELS as lvl}
        <button
          class="btn btn-sm"
          class:btn-primary={levelFilter === lvl}
          onclick={() => (levelFilter = lvl)}
        >
          {lvl}
        </button>
      {/each}
    </div>
    <input
      type="text"
      class="input log-search"
      placeholder="Filter lines…"
      bind:value={textFilter}
    />
    <label class="autoscroll">
      <input type="checkbox" bind:checked={autoScroll} />
      <span>Auto-scroll</span>
    </label>
  </div>
  <div class="card-body" style="padding: 0">
    {#if error}
      <div class="log-error">{error}</div>
    {/if}
    <div class="log-pane" bind:this={scrollEl}>
      {#if visibleLines.length === 0}
        <div class="empty">
          {lines.length === 0 ? 'No log lines buffered.' : 'No lines match the current filter.'}
        </div>
      {:else}
        {#each visibleLines as line, i (i)}
          <div class="log-line lvl-{lineLevel(line).toLowerCase()}">{line}</div>
        {/each}
      {/if}
    </div>
    <div class="log-foot">
      <span class="text-xs text-muted">
        showing {visibleLines.length} of {lines.length} buffered ({total} total)
      </span>
    </div>
  </div>
</div>

<style>
  .log-toolbar {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-3);
    flex-wrap: wrap;
  }
  .level-buttons {
    display: flex;
    gap: var(--trusty-space-1);
  }
  .log-search {
    flex: 1;
    min-width: 160px;
  }
  .autoscroll {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-1);
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
    cursor: pointer;
    white-space: nowrap;
  }
  .log-pane {
    max-height: 60vh;
    overflow: auto;
    background: #1e1e2e;
    padding: var(--trusty-space-3);
  }
  .log-line {
    font-family: var(--trusty-mono);
    font-size: var(--trusty-fs-xs);
    line-height: 1.6;
    color: #cdd6f4;
    white-space: pre-wrap;
    word-break: break-word;
  }
  .log-line.lvl-error {
    color: #f38ba8;
  }
  .log-line.lvl-warn {
    color: #f9e2af;
  }
  .log-line.lvl-debug,
  .log-line.lvl-trace {
    color: #7f849c;
  }
  .log-error {
    padding: var(--trusty-space-3) var(--trusty-space-5);
    color: var(--trusty-danger);
    background: var(--trusty-danger-soft);
    font-size: var(--trusty-fs-sm);
  }
  .log-foot {
    padding: var(--trusty-space-2) var(--trusty-space-4);
    border-top: 1px solid var(--trusty-border);
  }
  @media (max-width: 480px) {
    .log-search {
      order: 3;
      width: 100%;
      flex-basis: 100%;
    }
  }
</style>
