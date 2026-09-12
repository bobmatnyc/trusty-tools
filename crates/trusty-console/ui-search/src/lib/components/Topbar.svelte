<script>
  /*
   * Why: Breadcrumb + daemon-status header that mirrors trusty-memory's
   * Topbar. The redesign (issue #38) adds service controls — a Stop button
   * wired to `POST /admin/stop` plus restart guidance — and keeps the
   * version badge doubling as a health indicator.
   * #6439: owner directive — every service dashboard the console can serve
   * links back to it. `resolveConsoleUrl` (vendored `consoleLink.js`)
   * resolves a same-origin relative link when this SPA is served through the
   * console's `/tools/search/` mount, else the well-known standalone default
   * (owner ruling 2026-08-31) — no config knob. The status badge is now the
   * shared Foundry `Badge` (`dot` prop) instead of a hand-rolled span, so its
   * status glyph matches the console's own (`crates/trusty-console/ui/src/
   * Badge.svelte`) rather than being a third divergent copy.
   * #7589: the header now opens with the Foundry brand lockup — the canonical
   * robot mark beside this tool's name (`ToolLockup`, vendored from
   * docs/design/UI/design-system/icons/). Before it, a dashboard served at
   * `/tools/search/` showed a bare breadcrumb and nothing identifying the
   * product family. The lockup reads its colour and type from Foundry tokens
   * only, so it inverts with `data-theme` and adds no hue.
   * What: Renders the brand lockup and the crumbs derived from the current
   * route, then a right-side cluster with the console link-back, the
   * version/online badge, and a Stop control.
   * Test: navigate to /search, confirm crumb reads "Search"; click Stop,
   * confirm the dialog appears. `consoleLink.test.js` covers the link
   * address; `Topbar.test.js` mounts this header and asserts the lockup's
   * mark and wordmark are both in it.
   */
  import { getHealth } from '../state.svelte.js';
  import { getRoute } from '../router.svelte.js';
  import { api } from '../api.js';
  import { resolveConsoleUrl } from '../consoleLink.js';
  import ActionIcon from './ActionIcon.svelte';
  import Badge from './Badge.svelte';
  import ToolLockup from './ToolLockup.svelte';

  const consoleHref = resolveConsoleUrl();

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
  <div class="lead">
    <ToolLockup name="Trusty Search" />
    <span class="lead-divider" aria-hidden="true"></span>
    <div class="crumbs">
      {#each crumbs as crumb, i}
        {#if i > 0}<span class="sep">/</span>{/if}
        <span class="crumb">{crumb}</span>
      {/each}
    </div>
  </div>
  <div class="actions">
    <a class="console-link" href={consoleHref} title="Back to the Trusty Console">
      <ActionIcon name="pm" size={14} />
      Console
    </a>
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
      <Badge tone="success" dot>v{health.version || '?'}</Badge>
    {:else if health}
      <Badge tone="danger" dot>offline</Badge>
    {:else}
      <Badge tone="muted" dot>connecting…</Badge>
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
  /* #7589: the lockup and the crumbs are one left-hand cluster, separated by a
     hairline rather than by spacing alone, so "which product" and "where in it"
     read as two fields of the same header. */
  .lead {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-3);
    min-width: 0;
  }
  .lead-divider {
    width: 1px;
    height: 20px;
    flex: none;
    background: var(--trusty-border);
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
  .console-link {
    display: inline-flex;
    align-items: center;
    gap: 6px;
    padding: 4px 10px;
    border: 1px solid var(--trusty-border);
    border-radius: 6px;
    color: var(--trusty-text-secondary);
    font-size: var(--trusty-fs-sm);
    font-weight: 500;
    text-decoration: none;
    transition: background 0.15s, color 0.15s, border-color 0.15s;
  }
  .console-link:hover,
  .console-link:focus-visible {
    color: var(--trusty-text-primary);
    border-color: var(--trusty-accent);
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
