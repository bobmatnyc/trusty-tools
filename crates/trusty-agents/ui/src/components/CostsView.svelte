<script lang="ts">
  /**
   * Why (#4098, COST-09 formerly #4108): the Costs tab — what this project has
   * spent, by agent, by model, and by day. It renders `GET /api/costs`, which
   * folds `.trusty-agents/state/usage.jsonl` read-time through the single
   * pricing table (see the Rust modules `usage::aggregate` and `perf::pricing`).
   *
   * The pane's whole posture is that a cost figure the operator cannot trust is
   * worse than no figure, so it never collapses a state it can distinguish:
   *  - LOADING is its own state — never an empty chart that looks like $0.
   *  - NO LOG YET says so, and names the file it looked for. A confident
   *    `$0.00` over a missing usage log is the failure this tab exists to
   *    avoid; it is the reason `lib/costs.ts` refuses the sibling clients'
   *    return-`[]`-on-failure convention.
   *  - AN EMPTY BUT PRESENT LOG is a different, real answer — the project has
   *    state and simply has not dispatched.
   *  - MALFORMED LINES raise a visible warning, because totals that skipped
   *    rows are incomplete and presenting them as whole is the same lie in a
   *    quieter voice.
   *  - AN ERROR shows the server's message, not an empty dataset.
   *
   * Charting: rendered with plain CSS bars, NOT a charting library. The UI has
   * no charting dependency today (`package.json` carries only `lucide-svelte`
   * for icons), and COST-09's "reuse existing (likely Recharts)" is mistaken —
   * there is no Recharts here, and this is a Svelte app, not React. A relative
   * horizontal bar per row needs one `<div>` and a width percentage; adding a
   * charting dependency to the shipped bundle to draw it would be a poor trade.
   * Stacked bars (COST-09's literal ask) are NOT implemented — see the PR body.
   *
   * Colors come from the Foundry token scale only (`foundry-*` classes, both
   * light and dark variants), never hardcoded hex.
   * Test: `CostsView.test.ts`.
   */
  import { onMount } from 'svelte';
  import { AlertCircle, AlertTriangle, Loader2, RefreshCw } from 'lucide-svelte';
  import {
    barPercent,
    fetchCosts,
    formatCount,
    formatUsd,
    type CostRow,
    type CostsState,
  } from '../lib/costs';

  /**
   * Injected by tests to render a fixed state without a network round-trip.
   * `null` in the app, which triggers the `onMount` load.
   */
  export let state: CostsState | null = null;

  type GroupBy = 'agent' | 'model' | 'date';
  const GROUPS: { id: GroupBy; label: string }[] = [
    { id: 'agent', label: 'By agent' },
    { id: 'model', label: 'By model' },
    { id: 'date', label: 'By day' },
  ];
  const WINDOWS: { days: number; label: string }[] = [
    { days: 0, label: 'All' },
    { days: 30, label: '30d' },
    { days: 7, label: '7d' },
    { days: 1, label: '1d' },
  ];

  let groupBy: GroupBy = 'agent';
  let windowDays = 0;
  let loading = false;

  async function load() {
    loading = true;
    state = await fetchCosts(windowDays);
    loading = false;
  }

  function selectWindow(days: number) {
    windowDays = days;
    void load();
  }

  onMount(() => {
    // A test-supplied state stands in for the fetch; the app always loads.
    if (state === null) void load();
  });

  $: summary = state?.kind === 'ok' ? state.summary : null;
  $: rows =
    summary === null
      ? []
      : groupBy === 'agent'
        ? summary.by_agent
        : groupBy === 'model'
          ? summary.by_model
          : summary.by_date;
  $: maxCost = rows.reduce((m: number, r: CostRow) => Math.max(m, r.cost_usd), 0);
</script>

<div class="flex flex-1 flex-col overflow-hidden">
  <div
    class="flex flex-col gap-2 border-b border-foundry-light-border dark:border-foundry-border px-4 py-2.5"
  >
    <div class="flex flex-wrap items-center gap-2">
      <div class="flex items-center gap-1" role="group" aria-label="Group by">
        {#each GROUPS as g (g.id)}
          <button
            type="button"
            aria-pressed={groupBy === g.id}
            class="rounded-md border px-2 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide transition-colors {groupBy ===
            g.id
              ? 'border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
              : 'border-foundry-light-border dark:border-foundry-border text-foundry-light-muted dark:text-foundry-text/50'}"
            on:click={() => (groupBy = g.id)}
          >
            {g.label}
          </button>
        {/each}
      </div>

      <div class="flex items-center gap-1" role="group" aria-label="Time window">
        {#each WINDOWS as w (w.days)}
          <button
            type="button"
            aria-pressed={windowDays === w.days}
            class="rounded-md border px-2 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide transition-colors {windowDays ===
            w.days
              ? 'border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
              : 'border-foundry-light-border dark:border-foundry-border text-foundry-light-muted dark:text-foundry-text/50'}"
            on:click={() => selectWindow(w.days)}
          >
            {w.label}
          </button>
        {/each}
      </div>

      <button
        type="button"
        class="ml-auto flex items-center gap-1.5 rounded-md border border-foundry-light-border dark:border-foundry-border px-2 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50 hover:text-foundry-light-primary dark:hover:text-foundry-primary"
        on:click={() => void load()}
      >
        <RefreshCw class="h-3 w-3 {loading ? 'animate-spin' : ''}" aria-hidden="true" />
        Refresh
      </button>
    </div>

    {#if summary}
      <div class="flex flex-wrap items-baseline gap-x-5 gap-y-1">
        <span class="font-mono text-lg font-semibold text-foundry-light-primary dark:text-foundry-primary">
          {formatUsd(summary.totals.cost_usd)}
        </span>
        <span class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
          {summary.totals.dispatch_count} dispatches · {formatCount(summary.totals.input_tokens)} in
          · {formatCount(summary.totals.output_tokens)} out
        </span>
        {#if summary.first_ts && summary.last_ts}
          <span class="font-mono text-[10px] text-foundry-light-muted dark:text-foundry-text/40">
            {summary.first_ts.slice(0, 10)} → {summary.last_ts.slice(0, 10)}
          </span>
        {/if}
      </div>
      <p class="text-[10px] text-foundry-light-muted dark:text-foundry-text/40">
        Estimated from recorded token counts at published list rates — not a billed invoice. Cache
        tokens are not yet recorded (#4101), so cached turns are priced as uncached.
      </p>
    {/if}
  </div>

  <div class="flex-1 overflow-y-auto px-4 py-3">
    {#if state === null || (loading && summary === null)}
      <p
        class="flex items-center justify-center gap-2 py-8 text-sm text-foundry-light-muted dark:text-foundry-text/50"
      >
        <Loader2 class="h-4 w-4 animate-spin" aria-hidden="true" /> Loading cost data…
      </p>
    {:else if state.kind === 'error'}
      <p
        class="flex items-start gap-1.5 rounded-md border border-red-500/40 bg-red-500/10 px-3 py-2 text-xs text-red-500 dark:text-red-400"
      >
        <AlertCircle class="mt-0.5 h-3.5 w-3.5 shrink-0" aria-hidden="true" />
        Could not read cost data: {state.message}
      </p>
    {:else if state.kind === 'no-data'}
      <div
        class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-4 py-6 text-center"
      >
        <p class="text-sm text-foundry-light-text dark:text-foundry-text">
          No usage has been recorded for this project yet.
        </p>
        <p class="mt-1.5 text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
          This is not $0.00 — nothing has been written to the usage log. It appears after the first
          dispatch.
        </p>
        {#if state.source}
          <p class="mt-1 font-mono text-[10px] text-foundry-light-muted dark:text-foundry-text/40">
            {state.source}
          </p>
        {/if}
      </div>
    {:else}
      {#if state.summary.malformed_lines > 0}
        <p
          class="mb-3 flex items-start gap-1.5 rounded-md border border-foundry-amber/40 bg-foundry-amber/10 px-3 py-2 text-[11px] text-foundry-amber"
        >
          <AlertTriangle class="mt-0.5 h-3 w-3 shrink-0" aria-hidden="true" />
          {state.summary.malformed_lines} line{state.summary.malformed_lines === 1 ? '' : 's'} of the
          usage log could not be parsed and {state.summary.malformed_lines === 1 ? 'is' : 'are'} not
          included below — these totals are incomplete.
        </p>
      {/if}

      {#if rows.length === 0}
        <p
          class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-4 py-6 text-center text-sm text-foundry-light-muted dark:text-foundry-text/50"
        >
          No cost data for this period. The usage log exists but holds no dispatches in the selected
          window.
        </p>
      {:else}
        <table class="w-full border-collapse text-left text-xs">
          <thead>
            <tr
              class="border-b border-foundry-light-border dark:border-foundry-border text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50"
            >
              <th scope="col" class="py-1.5 pr-3 font-mono font-semibold">
                {groupBy === 'date' ? 'Day' : groupBy}
              </th>
              <th scope="col" class="py-1.5 pr-3 font-mono font-semibold">Share</th>
              <th scope="col" class="py-1.5 pr-3 text-right font-mono font-semibold">Cost</th>
              <th scope="col" class="py-1.5 pr-3 text-right font-mono font-semibold">Calls</th>
              <th scope="col" class="py-1.5 pr-3 text-right font-mono font-semibold">In</th>
              <th scope="col" class="py-1.5 text-right font-mono font-semibold">Out</th>
            </tr>
          </thead>
          <tbody>
            {#each rows as row (row.key)}
              <tr class="border-b border-foundry-light-border/50 dark:border-foundry-border/50">
                <td
                  class="max-w-[16rem] truncate py-1.5 pr-3 font-mono text-[11px] text-foundry-light-text dark:text-foundry-text"
                  title={row.key}
                >
                  {row.key}
                </td>
                <td class="w-1/3 py-1.5 pr-3">
                  <div
                    class="h-2 w-full overflow-hidden rounded-sm bg-foundry-light-border/50 dark:bg-foundry-border/50"
                  >
                    <div
                      class="h-full rounded-sm bg-foundry-light-primary dark:bg-foundry-primary"
                      style="width: {barPercent(row.cost_usd, maxCost)}%"
                    ></div>
                  </div>
                </td>
                <td
                  class="py-1.5 pr-3 text-right font-mono text-[11px] text-foundry-light-text dark:text-foundry-text"
                >
                  {formatUsd(row.cost_usd)}
                </td>
                <td
                  class="py-1.5 pr-3 text-right font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50"
                >
                  {row.dispatch_count}
                </td>
                <td
                  class="py-1.5 pr-3 text-right font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50"
                >
                  {formatCount(row.input_tokens)}
                </td>
                <td
                  class="py-1.5 text-right font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50"
                >
                  {formatCount(row.output_tokens)}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      {/if}
    {/if}
  </div>
</div>
