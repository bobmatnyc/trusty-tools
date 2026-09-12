<script>
  /*
   * Why: Sticky header providing route breadcrumbs, the global index picker,
   * and the daemon-health pill (status dot + search-reachable + version).
   * #6439: owner directive — every service dashboard the console can serve
   * links back to it. `resolveConsoleUrl` (vendored `consoleLink.js`)
   * resolves a same-origin relative link when this SPA is served through the
   * console's `/tools/analyze/` mount, else the well-known standalone default
   * (owner ruling 2026-08-31) — no config knob. Both status pills are now the
   * shared Foundry `Badge` (`dot` prop) instead of hand-rolled spans, so
   * their status glyphs match the console's own (`crates/trusty-console/ui/
   * src/Badge.svelte`) rather than being a third divergent copy.
   * #7589: the header now opens with the Foundry brand lockup — the canonical
   * robot mark beside this tool's name (`ToolLockup`, vendored from
   * docs/design/UI/design-system/icons/). Before it, a dashboard served at
   * `/tools/analyze/` showed a bare breadcrumb and nothing identifying the
   * product family. The lockup reads its colour and type from Foundry tokens
   * only, so it inverts with `data-theme` and adds no hue.
   * What: Renders the brand lockup and the crumbs derived from the current
   * route on the left; on the right, the console link-back, a <select> for
   * choosing the active index (persisted via state), the search-reachable
   * pill, and the daemon health badge.
   * Test: Stop trusty-search, refresh /health, confirm pill turns red.
   * `consoleLink.test.js` covers the link address; `Topbar.test.js` mounts
   * this header and asserts the lockup's mark and wordmark are both in it.
   */
  import {
    getHealth,
    getIndexes,
    getSelectedIndex,
    setSelectedIndex,
    refreshQuality,
    refreshHotspots,
    refreshSmells,
    refreshRefactors,
    refreshClusters,
    getTheme,
    setTheme
  } from '../state.svelte.js';
  import { resolveConsoleUrl } from '../consoleLink.js';
  import ActionIcon from './ActionIcon.svelte';
  import Badge from './Badge.svelte';
  import ToolLockup from './ToolLockup.svelte';

  const consoleHref = resolveConsoleUrl();

  const themes = [
    { value: 'light', label: '☀', title: 'Light' },
    { value: 'system', label: '⬡', title: 'System' },
    { value: 'dark', label: '☽', title: 'Dark' }
  ];
  let theme = $derived(getTheme());
  import { getRoute } from '../router.svelte.js';

  let health = $derived(getHealth());
  let indexes = $derived(getIndexes());
  let selected = $derived(getSelectedIndex());
  let route = $derived(getRoute());

  let crumbs = $derived.by(() => {
    const segs = route.segments;
    if (segs.length === 0) return ['Dashboard'];
    const head = segs[0];
    const map = {
      complexity: 'Complexity',
      smells: 'Smells',
      refactors: 'Refactors',
      clusters: 'Clusters',
      facts: 'Facts'
    };
    return [map[head] || 'Dashboard'];
  });

  let healthy = $derived(!!health && health.status === 'ok');
  let searchReachable = $derived(!!health && health.search_reachable === true);

  function onPickIndex(e) {
    const id = e.target.value;
    setSelectedIndex(id);
    if (!id) return;
    // Eagerly refresh the slices most views care about.
    refreshQuality(id).catch(() => {});
    refreshHotspots(id).catch(() => {});
    refreshSmells(id).catch(() => {});
    refreshRefactors(id).catch(() => {});
    refreshClusters(id).catch(() => {});
  }
</script>

<header class="topbar">
  <div class="lead">
    <ToolLockup name="Trusty Analyzer" />
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
    <select
      class="select index-picker"
      value={selected}
      onchange={onPickIndex}
      disabled={indexes.length === 0}
      title={indexes.length === 0
        ? 'No indexes — run: trusty-search index <path>'
        : 'Select an index to analyze'}
    >
      {#if indexes.length === 0}
        <option value="">No indexes — run: trusty-search index &lt;path&gt;</option>
      {:else}
        <option value="" disabled>— select index —</option>
        {#each indexes as idx}
          {@const id = typeof idx === 'string' ? idx : idx.id}
          {@const label = typeof idx === 'string' ? idx : idx.name || idx.id}
          <option value={id}>{label}</option>
        {/each}
      {/if}
    </select>

    <div class="theme-switcher" role="group" aria-label="Theme">
      {#each themes as t}
        <button
          type="button"
          class:active={theme === t.value}
          title={t.title}
          aria-label={t.title}
          aria-pressed={theme === t.value}
          onclick={() => setTheme(t.value)}
        >{t.label}</button>
      {/each}
    </div>

    <!-- #6155: the `sse` pill is gone. #6287 deleted this daemon's event
         broadcast, so the badge reported a stream that no longer exists. -->
    <Badge tone={searchReachable ? 'success' : health ? 'danger' : 'muted'} dot>search</Badge>

    {#if health && healthy}
      <Badge tone="success" dot>v{health.version || 'ok'}</Badge>
    {:else if health}
      <Badge tone="danger" dot>unreachable</Badge>
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
  .index-picker {
    width: auto;
    min-width: 200px;
    max-width: 320px;
    padding: 6px 10px;
    font-size: var(--trusty-fs-sm);
  }
  .theme-switcher {
    display: inline-flex;
    align-items: center;
    gap: 0;
    padding: 2px;
    border: 1px solid var(--trusty-border);
    border-radius: 999px;
    background: var(--trusty-content-bg);
  }
  .theme-switcher button {
    appearance: none;
    border: none;
    background: transparent;
    color: var(--trusty-text-muted);
    width: 26px;
    height: 24px;
    padding: 0;
    line-height: 1;
    border-radius: 999px;
    font-size: 13px;
    display: inline-flex;
    align-items: center;
    justify-content: center;
    transition: background 0.15s ease, color 0.15s ease;
  }
  .theme-switcher button:hover {
    color: var(--trusty-text-primary);
  }
  .theme-switcher button.active {
    background: var(--trusty-accent);
    color: var(--trusty-text-inverse);
  }
</style>
