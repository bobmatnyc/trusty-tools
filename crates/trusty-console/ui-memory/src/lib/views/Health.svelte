<script>
  /*
   * Why: Operators need an at-a-glance view of the store and the daemon's
   * resource usage plus the dream-cycle health — a stalled background dream
   * loop is invisible without surfacing its last-run timestamp. #6928: the
   * owner's ruling put RAM and Disk in the hero row beside the four store
   * totals and the room count, as aggregate figures in byte units.
   * What: Auto-refreshing (5s) stat cards. The hero row comes from
   * `GET /api/console/metrics/memory`; CPU and uptime from `GET /health`; the
   * dream card from `GET /api/v1/dream/status`.
   * Test: `heroTiles.test.js` pins the row's shape; open #/health and confirm
   * the cards populate and the badge turns green once /health responds.
   */
  import { onMount, onDestroy } from 'svelte';
  import { api } from '../api.js';
  // #6928: the hero row's seven figures come from the console's cached
  // metrics report, not `memory.status` — that payload has no room count and
  // counts only cache-resident palaces, so on a host with 94 palaces and 2
  // resident it reports two palaces' worth of drawers.
  import { consoleMetrics } from '../consoleApi.js';
  import { heroTiles } from '../heroTiles.js';
  import { UNREACHABLE_AFTER_FAILURES } from '../state.svelte.js';

  let health = $state(null);
  let status = $state(null);
  let dream = $state(null);
  let metrics = $state(null);
  let error = $state(null);
  let lastUpdated = $state(null);
  let timer = null;
  // #6155: one starved poll is not an outage — see `state.svelte.js`.
  let consecutiveFailures = 0;

  async function refresh() {
    try {
      const [h, s, d, m] = await Promise.all([
        api.health(),
        api.status().catch(() => null),
        api.dreamStatus().catch(() => null),
        consoleMetrics().catch(() => null)
      ]);
      health = h;
      status = s;
      dream = d;
      metrics = m?.metrics ?? null;
      error = null;
      consecutiveFailures = 0;
      lastUpdated = new Date();
    } catch (e) {
      consecutiveFailures += 1;
      error = e.message || String(e);
      // Hold the last good snapshot until a second poll agrees. The error text
      // shows immediately either way, so a real outage is still visible at
      // once — what waits is discarding the numbers on screen.
      if (consecutiveFailures >= UNREACHABLE_AFTER_FAILURES) health = null;
    }
  }

  // #6928: seven tiles, always — a payload the console has not cached yet
  // renders them as unknown rather than shrinking the row.
  let tiles = $derived(heroTiles(metrics));

  onMount(() => {
    refresh();
    timer = setInterval(refresh, 5000);
  });
  onDestroy(() => {
    if (timer) clearInterval(timer);
  });

  /**
   * Why: raw seconds are unreadable past a few minutes.
   * What: humanise to "Xs / Xm / Xh / Xd".
   * Test: humanUptime(7200) === "2h".
   */
  function humanUptime(secs) {
    if (typeof secs !== 'number' || secs < 0) return '—';
    if (secs < 60) return `${secs}s`;
    const m = Math.floor(secs / 60);
    if (m < 60) return `${m}m`;
    const h = Math.floor(m / 60);
    if (h < 24) return `${h}h`;
    return `${Math.floor(h / 24)}d`;
  }

  /**
   * Why: a dream's `last_run_at` is an ISO timestamp; operators want a
   * glanceable local time, or a clear "never" when no cycle has run.
   * What: localised date-time string, or "never".
   * Test: humanTime(null) === "never".
   */
  function humanTime(iso) {
    if (!iso) return 'never';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString();
  }

  let online = $derived(!!health && health.status === 'ok');
</script>

<div class="page-head">
  <h1 class="page-title">Health</h1>
  <div class="head-meta">
    {#if online}
      <span class="badge badge-success">online</span>
    {:else}
      <span class="badge badge-danger">offline</span>
    {/if}
    {#if lastUpdated}
      <span class="text-xs text-muted">updated {lastUpdated.toLocaleTimeString()}</span>
    {/if}
  </div>
</div>

{#if error}
  <div class="card" style="border-color: var(--trusty-danger)">
    <div class="card-body" style="color: var(--trusty-danger)">{error}</div>
  </div>
{/if}

<!-- #6928: the hero row. Seven tiles, RAM and Disk among them, both in byte
     units. `hero-row` is what `heroTiles.test.js` names in its assertions. -->
<div class="stat-grid hero-row" data-testid="hero-row">
  {#each tiles as tile (tile.id)}
    <div class="stat" data-tile={tile.id}>
      <div class="stat-label">{tile.label}</div>
      <div class="stat-value">{tile.value}</div>
      {#if tile.sub}<div class="stat-meta">{tile.sub}</div>{/if}
    </div>
  {/each}
</div>

<div class="stat-grid mt-4">
  <div class="stat">
    <div class="stat-label">CPU</div>
    <div class="stat-value">{(health?.cpu_pct ?? 0).toFixed(1)}%</div>
    <div class="stat-meta">100% = one full core</div>
  </div>
  <div class="stat">
    <div class="stat-label">Uptime</div>
    <div class="stat-value">{humanUptime(health?.uptime_secs)}</div>
    <div class="stat-meta">daemon v{health?.version ?? '—'}</div>
  </div>
</div>

<div class="card mt-4">
  <div class="card-header">Dream cycle</div>
  <div class="card-body" style="padding: 0">
    <table class="table">
      <tbody>
        <tr>
          <th style="width: 240px">Last run</th>
          <td>{humanTime(dream?.last_run_at)}</td>
        </tr>
        <tr>
          <th>Merged</th>
          <td>{(dream?.merged ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Pruned</th>
          <td>{(dream?.pruned ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Compacted</th>
          <td>{(dream?.compacted ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Closets updated</th>
          <td>{(dream?.closets_updated ?? 0).toLocaleString()}</td>
        </tr>
        <tr>
          <th>Total duration</th>
          <td>{(dream?.duration_ms ?? 0).toLocaleString()} ms</td>
        </tr>
      </tbody>
    </table>
  </div>
</div>

<!-- #6928: the four totals this card carried are hero tiles now, and the row
     above them counts every palace on disk rather than only the resident ones
     (#6372). What is left is the one fact the hero row cannot state: where the
     store that Disk tile measured actually lives. -->
<div class="card mt-4">
  <div class="card-header">Store</div>
  <div class="card-body" style="padding: 0">
    <table class="table">
      <tbody>
        <tr>
          <th style="width: 240px">Data root</th>
          <td class="text-mono text-xs text-muted">
            {metrics?.data_root ?? status?.data_root ?? '—'}
          </td>
        </tr>
      </tbody>
    </table>
  </div>
</div>

<style>
  .page-head {
    display: flex;
    align-items: center;
    justify-content: space-between;
    margin-bottom: var(--trusty-space-5);
    flex-wrap: wrap;
    gap: var(--trusty-space-3);
  }
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0;
    font-weight: 600;
  }
  .head-meta {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
  }
  .table th {
    background: var(--trusty-content-bg);
    text-transform: none;
    letter-spacing: 0;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
  }
</style>
