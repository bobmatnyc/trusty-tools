<script>
  /*
   * Why: The Palaces view is the operator's window into the memory hierarchy
   * — Palace → Wing → Room → Drawer. With many palaces (88+ on the author's
   * machine), browsing requires filtering, sorting, and project grouping so
   * operators can find recently-active palaces or jump to a specific
   * project's namespace.
   * What: A collapsible tree with a filter bar (name+project substring),
   * sort picker (Name | Drawers | Activity | Created), and an optional
   * "Group by project" toggle. A "Collection" dropdown filters by an
   * auto-detected name prefix shared by 2+ palaces. Clicking a palace
   * lazily fetches its drawers.
   * Test: open #/palaces, type a substring to filter, switch sort to
   * Drawers, toggle group-by-project, confirm groups render with totals.
   */
  import { onMount } from 'svelte';
  import { api } from '../api.js';
  import { navigate } from '../router.svelte.js';

  /*
   * Why: Issue #97 — clicking the "graph →" badge on a palace row should
   * jump to the per-palace visual graph view. The router uses hash-based
   * URLs so we just navigate to `#/palace/<id>/graph`.
   * What: Navigates the SPA without bubbling the click into the row toggle.
   * Test: click the badge in the dashboard, confirm `#/palace/<id>/graph`
   * is the new hash.
   */
  function navigateToGraph(id) {
    navigate(`/palace/${encodeURIComponent(id)}/graph`);
  }

  let palaces = $state([]);
  let error = $state(null);
  let loading = $state(true);

  // Filter / sort / group state.
  let filterText = $state('');
  let sortBy = $state('activity'); // 'name' | 'drawers' | 'activity' | 'created'
  let groupByProject = $state(false);
  let collection = $state(''); // '' = all collections

  // Per-palace expand state + lazily-loaded drawers, keyed by palace id.
  let expanded = $state({});
  let drawers = $state({}); // { [id]: { items, loading, error } }

  onMount(loadPalaces);

  async function loadPalaces() {
    loading = true;
    error = null;
    try {
      palaces = await api.listPalaces();
    } catch (e) {
      error = e.message || String(e);
      palaces = [];
    } finally {
      loading = false;
    }
  }

  /**
   * Why: drawers are the most expensive part of the tree, so we fetch them
   * lazily the first time a palace is expanded and cache the result.
   * What: toggles `expanded[id]`; on first expand, fetches the palace's
   * drawers and stores them in `drawers[id]`.
   * Test: click a palace, confirm its drawer list appears below the row.
   */
  async function togglePalace(id) {
    expanded = { ...expanded, [id]: !expanded[id] };
    if (expanded[id] && !drawers[id]) {
      drawers = { ...drawers, [id]: { items: [], loading: true, error: null } };
      try {
        const items = await api.listDrawers(id, { limit: 200 });
        drawers = {
          ...drawers,
          [id]: { items: Array.isArray(items) ? items : [], loading: false, error: null }
        };
      } catch (e) {
        drawers = {
          ...drawers,
          [id]: { items: [], loading: false, error: e.message || String(e) }
        };
      }
    }
  }

  /**
   * Why: a drawer's content can be long; the tree wants a one-line preview.
   * What: trims content to 140 chars with an ellipsis.
   * Test: preview("x".repeat(200)).length === 141.
   */
  function preview(text) {
    const t = (text || '').replace(/\s+/g, ' ').trim();
    return t.length <= 140 ? t : t.slice(0, 140) + '…';
  }

  /**
   * Why (issue #99): inter-project messages are stored as drawers carrying
   * a `msg:v1` marker tag plus `msg:purpose=…` and `msg:read=…` namespaced
   * tags. Rendering each of those as a raw tag bubble is noisy; we surface
   * a single coloured "Message" badge instead and hide the namespaced
   * tags from the regular tag list.
   * What: returns `{isMessage, purpose, read}` for a drawer; defaults to
   * `{isMessage: false}` when the marker tag is absent.
   * Test: badgeOf({tags:["msg:v1","msg:purpose=task","msg:read=false"]}) ===
   *       {isMessage:true, purpose:"task", read:false}.
   */
  function messageBadge(d) {
    const tags = d?.tags || [];
    if (!tags.some((t) => t === 'msg:v1')) return { isMessage: false };
    const get = (prefix) => {
      const m = tags.find((t) => t.startsWith(prefix));
      return m ? m.slice(prefix.length) : null;
    };
    return {
      isMessage: true,
      purpose: get('msg:purpose=') || 'message',
      from: get('msg:from=') || '',
      read: (get('msg:read=') || 'false').toLowerCase() === 'true',
    };
  }

  /**
   * Why: filter the namespaced `msg:*` tags out of the regular tag list so
   * the message badge is the single source of truth for message metadata.
   * What: returns the original tags minus anything matching the `msg:*`
   * convention from issue #99.
   * Test: visibleTags(["foo","msg:v1","msg:read=true"]) === ["foo"].
   */
  function visibleTags(tags) {
    return (tags || []).filter((t) => !t.startsWith('msg:'));
  }

  /**
   * Why: Auto-registered palaces store their source path in `description`
   * as "Auto-registered from <path>"; the basename of that path is the
   * project name, which is the natural grouping key for operators.
   * What: extracts basename from a description path; falls back to palace
   * name (kebab-case project name) for palaces not auto-registered.
   * Test: projectOf({description:"Auto-registered from /a/b/c"}) === "c".
   */
  function projectOf(p) {
    const desc = p?.description ?? '';
    const m = desc.match(/Auto-registered from (.+)$/);
    if (m) {
      const parts = m[1].split('/').filter(Boolean);
      if (parts.length > 0) return parts[parts.length - 1];
    }
    return p?.name || p?.id || '';
  }

  /**
   * Why: Operators want to filter the palace list to a "collection" of
   * related palaces sharing a name prefix (e.g. all `trusty-*`).
   * What: Splits each palace name on `-`, collects leading-segment groups
   * with 2+ members, returns sorted prefix strings.
   * Test: detectCollections([{name:"a-1"},{name:"a-2"},{name:"b"}]) === ["a"].
   */
  function detectCollections(list) {
    const counts = {};
    for (const p of list) {
      const name = (p?.name || p?.id || '').toLowerCase();
      const parts = name.split('-').filter(Boolean);
      if (parts.length < 2) continue;
      const prefix = parts[0];
      counts[prefix] = (counts[prefix] || 0) + 1;
    }
    return Object.entries(counts)
      .filter(([, n]) => n >= 2)
      .map(([prefix]) => prefix)
      .sort();
  }

  /**
   * Why: Activity sort uses last_write_at when present, falling back to
   * created_at so palaces with zero drawers still sort sensibly. Nulls
   * sort last so empty palaces don't dominate the top of the list.
   * What: returns a numeric epoch ms or null.
   * Test: activityKey({last_write_at:"2026-01-01T00:00:00Z"}) > 0.
   */
  function activityKey(p) {
    const v = p?.last_write_at || p?.created_at;
    if (!v) return null;
    const t = new Date(v).getTime();
    return Number.isFinite(t) ? t : null;
  }

  /**
   * Why: All comparators must agree on a stable ordering even when the
   * primary key ties (e.g. two palaces created at the same instant);
   * falling back to name keeps the tree deterministic.
   * What: returns a comparator that sorts by the chosen mode, then by name.
   * Test: sortComparator('drawers')({drawer_count:5},{drawer_count:3}) < 0.
   */
  function sortComparator(mode) {
    return (a, b) => {
      switch (mode) {
        case 'drawers':
          return (b?.drawer_count ?? 0) - (a?.drawer_count ?? 0)
            || (a?.name || '').localeCompare(b?.name || '');
        case 'activity': {
          const ka = activityKey(a);
          const kb = activityKey(b);
          if (ka === null && kb === null) {
            return (a?.name || '').localeCompare(b?.name || '');
          }
          if (ka === null) return 1; // nulls last
          if (kb === null) return -1;
          return kb - ka || (a?.name || '').localeCompare(b?.name || '');
        }
        case 'created':
          return new Date(b?.created_at || 0).getTime() -
                 new Date(a?.created_at || 0).getTime() ||
                 (a?.name || '').localeCompare(b?.name || '');
        case 'name':
        default:
          return (a?.name || '').localeCompare(b?.name || '');
      }
    };
  }

  // Derived: filtered + sorted palace list.
  let visiblePalaces = $derived.by(() => {
    const f = filterText.trim().toLowerCase();
    let out = palaces.slice();
    if (f) {
      out = out.filter((p) => {
        const name = (p?.name || p?.id || '').toLowerCase();
        const proj = projectOf(p).toLowerCase();
        return name.includes(f) || proj.includes(f);
      });
    }
    if (collection) {
      const c = collection.toLowerCase();
      out = out.filter((p) => {
        const name = (p?.name || p?.id || '').toLowerCase();
        return name.startsWith(`${c}-`) || name === c;
      });
    }
    out.sort(sortComparator(sortBy));
    return out;
  });

  let collections = $derived(detectCollections(palaces));

  /**
   * Why: Group view sorts groups alphabetically and applies the current
   * sort within each group, so operators can scan a project's palaces in
   * the same order they expect across modes.
   * What: returns [{project, palaces, drawerTotal}] grouped + sorted.
   * Test: groupedPalaces with two palaces sharing project should produce
   * one group with both.
   */
  let groupedPalaces = $derived.by(() => {
    const groups = new Map();
    for (const p of visiblePalaces) {
      const proj = projectOf(p);
      if (!groups.has(proj)) groups.set(proj, []);
      groups.get(proj).push(p);
    }
    const arr = [];
    for (const [project, items] of groups.entries()) {
      items.sort(sortComparator(sortBy));
      const drawerTotal = items.reduce((s, p) => s + (p?.drawer_count ?? 0), 0);
      arr.push({ project, palaces: items, drawerTotal });
    }
    arr.sort((a, b) => a.project.localeCompare(b.project));
    return arr;
  });

  /**
   * Why: "Activity" sort key needs a humane "2m ago" so the badge shows
   * recency at a glance.
   * What: relative-time string; returns '—' for null/invalid.
   * Test: relTime(new Date(Date.now()-1500).toISOString()).startsWith('1s').
   */
  function relTime(iso) {
    if (!iso) return '—';
    const t = new Date(iso).getTime();
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
</script>

<h1 class="page-title">Palaces</h1>

{#if error}
  <div class="card" style="border-color: var(--trusty-danger)">
    <div class="card-body" style="color: var(--trusty-danger)">{error}</div>
  </div>
{/if}

<div class="card">
  <div class="card-header flex-between">
    <span>Memory hierarchy</span>
    <button class="btn btn-sm" onclick={loadPalaces} disabled={loading}>
      {loading ? 'Refreshing…' : 'Refresh'}
    </button>
  </div>
  <div class="card-body controls">
    <input
      type="search"
      class="filter-input"
      placeholder="🔍 Filter by name or project…"
      bind:value={filterText}
    />
    <label class="ctl">
      <span class="lbl">Sort</span>
      <select bind:value={sortBy}>
        <option value="activity">Activity</option>
        <option value="name">Name</option>
        <option value="drawers">Drawers</option>
        <option value="created">Created</option>
      </select>
    </label>
    <label class="ctl">
      <span class="lbl">Collection</span>
      <select bind:value={collection}>
        <option value="">All</option>
        {#each collections as c}
          <option value={c}>{c}-*</option>
        {/each}
      </select>
    </label>
    <label class="ctl checkbox">
      <input type="checkbox" bind:checked={groupByProject} />
      <span>Group by project</span>
    </label>
    <span class="count-pill">{visiblePalaces.length} / {palaces.length}</span>
  </div>
  <div class="card-body" style="padding: 0">
    {#if loading}
      <div class="empty">Loading palaces…</div>
    {:else if visiblePalaces.length === 0}
      <div class="empty">
        {palaces.length === 0 ? 'No palaces yet.' : 'No palaces match the filter.'}
      </div>
    {:else if groupByProject}
      <div class="tree">
        {#each groupedPalaces as g (g.project)}
          <div class="group">
            <div class="group-head">
              <span class="tree-icon">▼</span>
              <span class="group-name">{g.project}</span>
              <span class="counts">
                <span class="badge badge-muted">{g.palaces.length} palaces</span>
                <span class="badge badge-info">{g.drawerTotal} drawers</span>
              </span>
            </div>
            {#each g.palaces as p (p.id)}
              {@render palaceRow(p)}
            {/each}
          </div>
        {/each}
      </div>
    {:else}
      <div class="tree">
        {#each visiblePalaces as p (p.id)}
          {@render palaceRow(p)}
        {/each}
      </div>
    {/if}
  </div>
</div>

{#snippet palaceRow(p)}
  <div class="palace">
    <div class="palace-row-wrap">
      <button
        class="tree-row palace-row"
        onclick={() => togglePalace(p.id)}
        aria-expanded={!!expanded[p.id]}
      >
        <span class="caret" class:open={expanded[p.id]}>▸</span>
        <span class="tree-icon">▤</span>
        <span class="tree-name">{p.name || p.id}</span>
        <span class="counts">
          <span class="badge badge-muted" title="Last write">
            {relTime(p.last_write_at)}
          </span>
          <span class="badge badge-muted">{p.wing_count ?? 0} wings</span>
          <span class="badge badge-muted">{p.drawer_count ?? 0} drawers</span>
          <span class="badge badge-info">{p.vector_count ?? 0} vectors</span>
          <span class="badge badge-info">{p.kg_triple_count ?? 0} triples</span>
          <!-- Issue #97: surface KG graph density inline so users can spot
               palaces with content without opening the graph view. -->
          <span class="badge badge-graph" title="KG nodes">
            {p.node_count ?? 0} nodes
          </span>
          <span class="badge badge-graph" title="KG edges">
            {p.edge_count ?? 0} edges
          </span>
        </span>
      </button>
      <!-- Sibling of the row button so we don't nest <button> inside
           <button>, which the browser would "repair" by hoisting and
           detaching click handlers. -->
      <button
        type="button"
        class="badge badge-link palace-graph-link"
        title="Open per-palace graph view"
        onclick={(e) => {
          e.stopPropagation();
          navigateToGraph(p.id);
        }}>
        graph →
      </button>
    </div>
    {#if p.description}
      <div class="palace-desc">{p.description}</div>
    {/if}
    {#if expanded[p.id]}
      <div class="children">
        {#if drawers[p.id]?.loading}
          <div class="tree-note">Loading drawers…</div>
        {:else if drawers[p.id]?.error}
          <div class="tree-note tree-error">{drawers[p.id].error}</div>
        {:else if (drawers[p.id]?.items || []).length === 0}
          <div class="tree-note">No drawers in this palace.</div>
        {:else}
          <div class="drawer-head">
            <span class="tree-icon">⌑</span>
            <span class="text-sm text-secondary">
              Drawers ({drawers[p.id].items.length})
            </span>
          </div>
          {#each drawers[p.id].items as d (d.id)}
            {@const badge = messageBadge(d)}
            <div class="drawer-row">
              <span class="tree-icon">·</span>
              <div class="drawer-body">
                <div class="drawer-content">{preview(d.content)}</div>
                <div class="drawer-meta">
                  <span class="bar" title="importance {d.importance ?? 0}">
                    <span
                      class="bar-fill"
                      style="width: {Math.round((d.importance ?? 0) * 100)}%"
                    ></span>
                  </span>
                  {#if badge.isMessage}
                    <span
                      class="msg-badge {badge.read ? 'msg-read' : 'msg-unread'}"
                      title={badge.from ? `From ${badge.from} • ${badge.read ? 'read' : 'unread'}` : ''}
                    >
                      {badge.read ? '✓ msg' : '✉ msg'} · {badge.purpose}
                    </span>
                  {/if}
                  {#each visibleTags(d.tags) as tag}
                    <span class="tag">{tag}</span>
                  {/each}
                </div>
              </div>
            </div>
          {/each}
        {/if}
      </div>
    {/if}
  </div>
{/snippet}

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .controls {
    display: flex;
    gap: 12px;
    align-items: center;
    flex-wrap: wrap;
    border-bottom: 1px solid var(--trusty-border, #e5e7eb);
  }
  .filter-input {
    flex: 1;
    min-width: 200px;
    padding: 6px 10px;
    border-radius: 4px;
    border: 1px solid var(--trusty-border, #e5e7eb);
    font-size: 13px;
  }
  .ctl {
    display: flex;
    align-items: center;
    gap: 6px;
  }
  .ctl.checkbox {
    cursor: pointer;
  }
  .lbl {
    font-size: 12px;
    color: var(--trusty-text-secondary, #6b7280);
  }
  select {
    padding: 4px 8px;
    border-radius: 4px;
    border: 1px solid var(--trusty-border, #e5e7eb);
    background: white;
    font-size: 13px;
  }
  .count-pill {
    font-size: 11px;
    color: var(--trusty-text-secondary, #6b7280);
    margin-left: auto;
  }
  .tree {
    display: flex;
    flex-direction: column;
  }
  .group {
    border-bottom: 1px solid var(--trusty-border);
  }
  .group-head {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    padding: var(--trusty-space-3) var(--trusty-space-5);
    background: var(--trusty-bg-subtle, #fafafa);
    font-weight: 600;
    font-size: 13px;
  }
  .group-name {
    color: var(--trusty-text, #111827);
  }
  .palace {
    border-bottom: 1px solid var(--trusty-border);
  }
  /*
   * Issue #97 — wraps the palace row button and the sibling "graph →" link
   * so they can't end up nested. Flexbox places the link at the end of the
   * row; the button itself still fills the available width.
   */
  .palace-row-wrap {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
  }
  .palace-row-wrap > .palace-row {
    flex: 1;
  }
  .palace-graph-link {
    margin-right: var(--trusty-space-5);
    flex-shrink: 0;
  }
  .tree-row {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    width: 100%;
    padding: var(--trusty-space-3) var(--trusty-space-5);
    background: none;
    border: none;
    text-align: left;
    flex-wrap: wrap;
  }
  .palace-row:hover {
    background: var(--trusty-content-bg);
  }
  .caret {
    display: inline-block;
    transition: transform 0.15s ease;
    color: var(--trusty-text-muted);
    font-size: var(--trusty-fs-xs);
  }
  .caret.open {
    transform: rotate(90deg);
  }
  .tree-icon {
    color: var(--trusty-text-muted);
  }
  .tree-name {
    font-weight: 600;
    color: var(--trusty-text-primary);
  }
  .counts {
    display: flex;
    gap: var(--trusty-space-1);
    margin-left: auto;
    flex-wrap: wrap;
  }
  .palace-desc {
    padding: 0 var(--trusty-space-5) var(--trusty-space-2) 44px;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-muted);
  }
  .children {
    background: var(--trusty-content-bg);
    padding: var(--trusty-space-2) 0 var(--trusty-space-3) 0;
  }
  .drawer-head {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    padding: var(--trusty-space-2) var(--trusty-space-5) var(--trusty-space-2) 44px;
  }
  .text-secondary {
    color: var(--trusty-text-secondary);
  }
  .drawer-row {
    display: flex;
    gap: var(--trusty-space-2);
    padding: var(--trusty-space-2) var(--trusty-space-5) var(--trusty-space-2) 60px;
  }
  .drawer-body {
    min-width: 0;
    flex: 1;
  }
  .drawer-content {
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-primary);
    word-break: break-word;
  }
  .drawer-meta {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    margin-top: 4px;
    flex-wrap: wrap;
  }
  /* Issue #99: inter-project messaging badge. Two colour variants signal
     unread vs read at a glance; the title attribute carries the full
     sender / read context. */
  .msg-badge {
    display: inline-flex;
    align-items: center;
    padding: 1px 6px;
    border-radius: 4px;
    font-size: 11px;
    font-weight: 600;
    letter-spacing: 0.02em;
  }
  .msg-unread {
    background: rgba(255, 196, 0, 0.18);
    color: #b56a00;
    border: 1px solid rgba(255, 196, 0, 0.35);
  }
  .msg-read {
    background: rgba(120, 120, 120, 0.18);
    color: var(--trusty-text-muted, #6b7280);
    border: 1px solid rgba(120, 120, 120, 0.25);
  }
  .tree-note {
    padding: var(--trusty-space-2) var(--trusty-space-5) var(--trusty-space-2) 44px;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-muted);
  }
  .tree-error {
    color: var(--trusty-danger);
  }
  .empty {
    padding: var(--trusty-space-5);
    text-align: center;
    color: var(--trusty-text-muted, #9ca3af);
    font-size: 13px;
  }
</style>
