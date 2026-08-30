<script>
  /*
   * Why: Breadcrumb + daemon-status header that mirrors trusty-memory's
   * Topbar. The redesign (issue #38) adds service controls — a Stop button
   * wired to `POST /admin/stop` plus restart guidance — and keeps the
   * version badge doubling as a health indicator.
   * What: Renders crumbs derived from the current route, then a right-side
   * cluster with the version/online badge and a Stop control.
   * Test: navigate to /search, confirm crumb reads "Search"; click Stop,
   * confirm the dialog appears.
   */
  import { getHealth } from '../state.svelte.js';
  import { getRoute } from '../router.svelte.js';
  import { api } from '../api.js';

  let health = $derived(getHealth());
  let route = $derived(getRoute());
  let stopping = $state(false);
  let actionNote = $state(null);

  let crumbs = $derived.by(() => {
    const segs = route.segments;
    if (segs.length === 0) return ['Dashboard'];
    if (segs[0] === 'search') return ['Search'];
    if (segs[0] === 'indexes' || segs[0] === 'index') {
      const parts = ['Indexes'];
      if (segs.length > 1) parts.push(segs[1]);
      return parts;
    }
    if (segs[0] === 'config') return ['Config'];
    if (segs[0] === 'health') return ['Health'];
    if (segs[0] === 'logs') return ['Logs'];
    return ['Dashboard'];
  });

  let healthy = $derived(health && health.status === 'ok');

  /**
   * Why: a one-click daemon stop saves operators from resolving the PID and
   * sending a signal. The daemon is localhost-only so no auth is needed.
   * What: confirms, then POSTs `/admin/stop`; the daemon exits ~200ms later.
   * Test: click Stop, accept the dialog, confirm the badge flips to offline.
   */
  async function stopDaemon() {
    if (!confirm('Stop the trusty-search daemon? Active searches will be interrupted.')) {
      return;
    }
    stopping = true;
    actionNote = null;
    try {
      await api.stopDaemon();
      actionNote = 'Daemon is shutting down…';
    } catch (e) {
      // A connection-reset is expected once the daemon exits mid-response.
      actionNote = 'Stop requested (daemon may already be down).';
    } finally {
      stopping = false;
    }
  }

  /**
   * Why: there is no remote start/restart endpoint — once the daemon stops it
   * cannot be revived over HTTP. Surface the CLI command instead of a button
   * that cannot work.
   * What: shows the restart instruction in a transient note.
   * Test: click Restart, confirm the CLI hint appears.
   */
  function restartHint() {
    actionNote = 'Restart from a terminal: `trusty-search stop && trusty-search start`.';
  }
</script>

<header class="topbar">
  <div class="crumbs">
    {#each crumbs as crumb, i}
      {#if i > 0}<span class="sep">/</span>{/if}
      <span class="crumb">{crumb}</span>
    {/each}
  </div>
  <div class="actions">
    {#if actionNote}
      <span class="text-xs text-muted note">{actionNote}</span>
    {/if}
    <div class="controls">
      <button
        class="btn btn-sm btn-danger"
        onclick={stopDaemon}
        disabled={stopping || !healthy}
        title="POST /admin/stop"
      >
        {stopping ? 'Stopping…' : 'Stop'}
      </button>
      <button class="btn btn-sm" onclick={restartHint} title="Restart instructions">
        Restart
      </button>
    </div>
    {#if health && healthy}
      <span class="badge badge-success">v{health.version || '?'}</span>
    {:else if health}
      <span class="badge badge-danger">offline</span>
    {:else}
      <span class="badge badge-muted">connecting…</span>
    {/if}
  </div>
</header>

<style>
  .topbar {
    height: var(--trusty-topbar-height);
    background: var(--trusty-card-bg);
    border-bottom: 1px solid var(--trusty-border);
    display: flex;
    align-items: center;
    justify-content: space-between;
    padding: 0 var(--trusty-space-6);
    position: sticky;
    top: 0;
    z-index: 10;
    gap: var(--trusty-space-3);
  }
  .crumbs {
    display: flex;
    align-items: center;
    gap: 8px;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-secondary);
    min-width: 0;
  }
  .crumb {
    font-weight: 500;
  }
  .crumb:last-child {
    color: var(--trusty-text-primary);
    font-weight: 600;
  }
  .sep {
    color: var(--trusty-text-muted);
  }
  .actions {
    display: flex;
    align-items: center;
    gap: 12px;
  }
  .controls {
    display: flex;
    gap: var(--trusty-space-1);
  }
  .note {
    max-width: 260px;
    text-align: right;
  }
  @media (max-width: 600px) {
    .note {
      display: none;
    }
  }
</style>
