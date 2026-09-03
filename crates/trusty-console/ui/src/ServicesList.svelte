<!--
  The home page's one Services section (#6642).

  Why: the owner replaced the "Installed Services" card grid and the
  machine-status rollup table with a single alphabetical list — name, version,
  status, %CPU, and a graph — whose rows open the service's dashboard. Two
  service sections on one screen was the problem; this is the replacement, and
  the grid and the rollup are both deleted rather than hidden.
  What: a CSS-grid list rather than a `<table>`, because a row that navigates
  must BE a `<button>` and a button cannot wrap a `<tr>`. `servicesList.js`
  computes every cell; this file is markup, the click handler, and the styling.
  Rows without a dashboard render as a plain `<div>`: not focusable, no pointer
  cursor, and carrying the reason on `title` plus a visually-hidden span so it
  is available to a screen reader too. A clickable row's `aria-label` replaces
  the name its cells would compute, so `rowAriaLabel` restates every column in
  it rather than announcing the service and the action alone.
  Test: `servicesList.test.js` covers the sort, the dash-not-zero rule, the
  live-sample overlay and which rows are clickable; `barGraph.test.js` covers
  the row graph's geometry.
-->
<script>
  import BarGraph from './BarGraph.svelte';
  import { NO_DASHBOARD_HINT, serviceRows } from './servicesList.js';
  import { windowMax } from './barGraph.js';

  /**
   * @type {{
   *   services: object[],
   *   serviceSamples?: Record<string, object[]>,
   *   dashboards?: Set<string>,
   *   onOpen?: (id: string) => void,
   *   loading?: boolean,
   *   error?: string | null,
   * }}
   */
  let {
    services = [],
    serviceSamples = {},
    dashboards = new Set(),
    onOpen,
    loading = false,
    error = null,
  } = $props();

  let rows = $derived(serviceRows(services, serviceSamples, dashboards));

  /**
   * A row's bars are scaled to the busiest second in its own window.
   *
   * A shared scale would flatten every row against whichever service happens to
   * be compiling; per-row scaling answers "is this daemon busier than it was a
   * minute ago", which is the question a row-height graph can actually answer.
   * The percentage in the %CPU column carries the absolute figure.
   */
  function scaleFor(series) {
    return windowMax(series, 5);
  }
</script>

<section class="foundry services">
  <h2 class="section-title">Services</h2>

  {#if loading}
    <div class="notice">Detecting services…</div>
  {:else if error}
    <div class="notice failed">Failed to load services: {error}</div>
  {:else if rows.length === 0}
    <div class="notice">No services detected.</div>
  {:else}
    <div class="list">
      <div class="row head" aria-hidden="true">
        <span>Service</span>
        <span>Version</span>
        <span>Status</span>
        <span class="num">%CPU</span>
        <span class="graph-head">CPU · last 10 min</span>
      </div>

      {#each rows as row (row.id)}
        {#if row.hasDashboard}
          <!-- #6642: the whole row opens the dashboard, so the row IS the
               button — same one-action-means-whole-card rule the card grid
               used before it was replaced. -->
          <button
            type="button"
            class="row link"
            aria-label={row.ariaLabel}
            onclick={() => onOpen?.(row.id)}
          >
            <span class="name">{row.displayName}</span>
            <span class="mono">{row.version}</span>
            <span class="badge-cell">
              <span class="badge" style="--_s: {row.statusVar};">
                <span class="dot"></span>{row.statusLabel}
              </span>
            </span>
            <span class="mono num">{row.cpuLabel}</span>
            <span class="graph">
              <BarGraph
                values={row.series}
                max={scaleFor(row.series)}
                height="1.6rem"
                label="{row.displayName} CPU, one bar per second"
              />
            </span>
          </button>
        {:else}
          <!-- #6642: no dashboard exists for this service, so the row is inert:
               no pointer, not focusable, and it says why. -->
          <div class="row inert" title={NO_DASHBOARD_HINT}>
            <span class="name">
              {row.displayName}
              <span class="sr-only">— {NO_DASHBOARD_HINT}</span>
            </span>
            <span class="mono">{row.version}</span>
            <span class="badge-cell">
              <span class="badge" style="--_s: {row.statusVar};">
                <span class="dot"></span>{row.statusLabel}
              </span>
            </span>
            <span class="mono num">{row.cpuLabel}</span>
            <span class="graph">
              <BarGraph
                values={row.series}
                max={scaleFor(row.series)}
                height="1.6rem"
                label="{row.displayName} CPU, one bar per second"
              />
            </span>
          </div>
        {/if}
      {/each}
    </div>
  {/if}
</section>

<style>
  .services { margin-bottom: 2rem; }

  .section-title {
    margin: 0 0 1rem;
    font-family: var(--trusty-display);
    font-size: 1.25rem;
    font-weight: 700;
    letter-spacing: 0.04em;
    text-transform: uppercase;
    color: var(--trusty-text-primary);
  }

  .notice {
    background: var(--trusty-card-bg);
    border: 1.5px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    padding: 0.9rem 1.1rem;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
  }
  .notice.failed { color: var(--trusty-danger); border-color: var(--trusty-danger); }

  .list {
    background: var(--trusty-card-bg);
    border: 1.5px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    overflow: hidden;
  }

  /* One track per column. The graph takes the remaining width so it is the
     widest cell — a 600-bar window needs the room. */
  .row {
    display: grid;
    grid-template-columns: minmax(9rem, 1.2fr) minmax(4rem, auto) 6.5rem 4.5rem minmax(8rem, 2fr);
    align-items: center;
    gap: var(--trusty-space-4);
    width: 100%;
    padding: 0.55rem var(--trusty-space-5);
    border-bottom: 1px solid var(--trusty-surface-raised);
    font-size: var(--trusty-fs-sm);
    text-align: left;
    color: var(--trusty-text-secondary);
  }
  .row:last-child { border-bottom: none; }

  .row.head {
    font: 600 10px var(--trusty-mono);
    letter-spacing: 0.14em;
    text-transform: uppercase;
    color: var(--trusty-text-muted);
    border-bottom: 1px solid var(--trusty-border);
    background: transparent;
  }

  .row.link {
    background: none;
    border-left: none;
    border-right: none;
    border-top: none;
    font-family: inherit;
    cursor: pointer;
  }
  .row.link:hover { background: var(--trusty-surface-hover); }
  .row.link:focus-visible {
    outline: 2px solid var(--trusty-accent);
    outline-offset: -2px;
  }

  /* No dashboard → no affordance. Default cursor, no hover, not focusable. */
  .row.inert { cursor: default; color: var(--trusty-text-muted); }

  .name { font-weight: 600; color: var(--trusty-text-primary); }
  .mono { font-family: var(--trusty-mono); font-size: var(--trusty-fs-xs); }
  .num { text-align: right; font-variant-numeric: tabular-nums; }
  .graph { display: block; min-width: 0; }

  /* The same theme-adaptive stamp ServiceCard used, kept because it is the one
     badge in this crate whose colour is chosen per status by
     `statusPresentation.js` rather than by a Foundry tone class. */
  .badge {
    display: inline-flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.7rem;
    font-weight: 600;
    padding: 0.15rem 0.5rem;
    border-radius: var(--trusty-radius-sm);
    border: 1px solid;
    white-space: nowrap;
    --_s: var(--trusty-text-muted);
    color: var(--_s);
    background: rgba(0, 0, 0, 0.08);
    background: color-mix(in srgb, var(--_s) 13%, transparent);
    border-color: rgba(0, 0, 0, 0.18);
    border-color: color-mix(in srgb, var(--_s) 27%, transparent);
  }
  .dot { width: 6px; height: 6px; border-radius: 50%; background: var(--_s); }

  .sr-only {
    position: absolute;
    width: 1px;
    height: 1px;
    padding: 0;
    margin: -1px;
    overflow: hidden;
    clip: rect(0, 0, 0, 0);
    white-space: nowrap;
    border: 0;
  }

  @media (max-width: 720px) {
    /* Below this width five columns crush; the graph moves to its own row. */
    .row {
      grid-template-columns: minmax(7rem, 1fr) auto 5.5rem 3.5rem;
    }
    .graph { grid-column: 1 / -1; }
    .row.head .graph-head { display: none; }
  }
</style>
