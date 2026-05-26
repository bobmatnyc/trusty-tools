<script>
  /*
   * Why: Issue #97 — give every palace a per-palace visual knowledge graph
   * so operators can spot clusters and inspect the auto-extracted triples
   * coming out of `memory_remember`. The existing `KG.svelte` is a flat
   * subject/triple table that hides topology; this view renders the same
   * data as a force-directed node-link diagram so the structure is visible
   * at a glance.
   * What: Fetches `GET /api/v1/palaces/{id}/kg/graph`, converts the triple
   * list into nodes+links, runs a lightweight d3-style force simulation
   * (implemented inline to avoid a heavy d3 import), and renders the result
   * into an SVG with labelled nodes and directional edges. Clicking a node
   * opens a side panel listing the incident edges and the source drawer ids
   * extracted from `drawer:<uuid>` nodes / triple subjects.
   * Test: open `#/palace/<id>/graph`, confirm nodes render, drag a node to
   * re-settle the layout, click a node and verify the side panel populates.
   */
  import { onDestroy, onMount } from 'svelte';
  import { api } from '../api.js';
  import { getRoute, navigate } from '../router.svelte.js';

  // Selected palace + payload.
  let palaceId = $state('');
  let triples = $state([]);
  let counts = $state({ node_count: 0, edge_count: 0, community_count: 0 });
  let loading = $state(true);
  let error = $state(null);
  let loadStartedAt = 0;
  let loadElapsedMs = $state(0);

  // Layout state — populated by `runLayout`. Both arrays are keyed by index
  // and mutated in-place by the simulation tick.
  let nodes = $state([]); // {id, label, x, y, vx, vy, fx, fy, kind, community}
  let links = $state([]); // {source, target, predicate}
  let selectedId = $state(null);
  let hoverId = $state(null);

  // Viewport sizing — recalculated on mount + window resize.
  let width = $state(900);
  let height = $state(600);
  let svgEl = $state(null);
  let resizeObserver;
  let simHandle = null;

  // Force simulation knobs.
  const LINK_DISTANCE = 90;
  const REPULSION = 1200;
  const CENTER_STRENGTH = 0.04;
  const DAMPING = 0.85;
  const MAX_STEPS = 200; // sim auto-stops after this many ticks
  const TICK_MS = 16; // ~60fps target

  onMount(() => {
    palaceId = palaceFromRoute();
    if (palaceId) loadGraph();
    const onResize = () => sizeFromContainer();
    window.addEventListener('resize', onResize);
    sizeFromContainer();
    return () => {
      window.removeEventListener('resize', onResize);
      stopSimulation();
    };
  });

  onDestroy(() => stopSimulation());

  // React to hash-route changes so a `navigate(...)` from another view
  // re-loads this view automatically.
  $effect(() => {
    const r = getRoute();
    const segs = r?.segments ?? [];
    let next = '';
    if (segs[0] === 'palace' && segs.length >= 2) next = segs[1];
    if (next && next !== palaceId) {
      palaceId = next;
      loadGraph();
    }
  });

  function palaceFromRoute() {
    const r = getRoute();
    const segs = r?.segments ?? [];
    if (segs[0] === 'palace' && segs.length >= 2) return decodeURIComponent(segs[1]);
    return '';
  }

  function sizeFromContainer() {
    if (!svgEl) return;
    const r = svgEl.parentElement?.getBoundingClientRect();
    if (r) {
      width = Math.max(400, Math.floor(r.width));
      // Keep the SVG within a sensible vertical budget so the side panel
      // remains visible. The card body itself can scroll if needed.
      height = Math.max(420, Math.floor(Math.min(800, window.innerHeight - 240)));
    }
  }

  async function loadGraph() {
    loading = true;
    error = null;
    selectedId = null;
    loadStartedAt = performance.now();
    try {
      const payload = await api.kgGraph(palaceId);
      triples = Array.isArray(payload?.triples) ? payload.triples : [];
      counts = {
        node_count: payload?.node_count ?? 0,
        edge_count: payload?.edge_count ?? 0,
        community_count: payload?.community_count ?? 0
      };
      buildLayout();
    } catch (e) {
      error = e.message || String(e);
      triples = [];
      nodes = [];
      links = [];
    } finally {
      loading = false;
      loadElapsedMs = Math.round(performance.now() - loadStartedAt);
    }
  }

  /*
   * Why: Convert the flat `Triple[]` list into the {nodes, links} shape
   * d3-force expects. Each distinct subject and object becomes a node;
   * each triple becomes one directed link.
   * What: Walks `triples`, deduplicating node ids via a Map; produces
   * `{nodes, links}` with stable string ids and assigns a `kind` field
   * (`drawer` / `tag` / `topic` / `room` / `other`) for color coding.
   * Test: visually — labels and edge directionality match the table view.
   */
  function buildLayout() {
    const nodeMap = new Map();
    const nextLinks = [];
    function ensureNode(label) {
      if (!label) return null;
      if (nodeMap.has(label)) return nodeMap.get(label);
      const kind = classify(label);
      const node = {
        id: label,
        label,
        kind,
        community: Math.abs(hashStr(label)) % Math.max(1, counts.community_count || 8),
        x: width / 2 + (Math.random() - 0.5) * 200,
        y: height / 2 + (Math.random() - 0.5) * 200,
        vx: 0,
        vy: 0,
        fx: null,
        fy: null
      };
      nodeMap.set(label, node);
      return node;
    }
    for (const t of triples) {
      const s = ensureNode(t.subject);
      const o = ensureNode(t.object);
      if (s && o && s !== o) {
        nextLinks.push({ source: s.id, target: o.id, predicate: t.predicate });
      }
    }
    nodes = Array.from(nodeMap.values());
    links = nextLinks;
    // Restart the simulation against the new graph.
    stopSimulation();
    runLayout();
  }

  function classify(label) {
    if (typeof label !== 'string') return 'other';
    if (label.startsWith('drawer:')) return 'drawer';
    if (label.startsWith('tag:')) return 'tag';
    if (label.startsWith('topic:')) return 'topic';
    if (label.startsWith('room:')) return 'room';
    return 'other';
  }

  /*
   * Why: Tiny deterministic hash so node colors stay stable across reloads
   * without pulling in an external dep.
   * What: 32-bit djb2 variant returning a signed integer.
   * Test: hashStr('rust') === hashStr('rust'); two distinct strings rarely
   * collide on the same modulus.
   */
  function hashStr(s) {
    let h = 5381;
    for (let i = 0; i < s.length; i++) {
      h = ((h << 5) + h) ^ s.charCodeAt(i);
    }
    return h | 0;
  }

  /*
   * Why: Inline a minimal force-directed layout (link + repulsion + center)
   * so we don't add a 100KB d3-force bundle for what is essentially three
   * loops. Modern browsers run this comfortably for the <500-triple target.
   * What: A `setInterval`-driven tick that updates `nodes[i].x/y` in place
   * and re-assigns `nodes` to trigger Svelte's reactivity. Stops itself
   * after `MAX_STEPS` ticks or when total kinetic energy falls below a
   * threshold.
   * Test: load a palace with at least 5 triples and watch the layout settle
   * within ~3 seconds.
   */
  function runLayout() {
    if (nodes.length === 0) return;
    let step = 0;
    simHandle = setInterval(() => {
      tick();
      step++;
      if (step >= MAX_STEPS) stopSimulation();
    }, TICK_MS);
  }

  function stopSimulation() {
    if (simHandle != null) {
      clearInterval(simHandle);
      simHandle = null;
    }
  }

  function tick() {
    if (nodes.length === 0) return;
    const nodeIndex = new Map();
    for (let i = 0; i < nodes.length; i++) nodeIndex.set(nodes[i].id, i);

    // Repulsion — O(n^2) pairwise. Fine for <500 nodes.
    for (let i = 0; i < nodes.length; i++) {
      const ni = nodes[i];
      for (let j = i + 1; j < nodes.length; j++) {
        const nj = nodes[j];
        const dx = nj.x - ni.x;
        const dy = nj.y - ni.y;
        let dist2 = dx * dx + dy * dy;
        if (dist2 < 1) dist2 = 1;
        const force = REPULSION / dist2;
        const dist = Math.sqrt(dist2);
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;
        ni.vx -= fx;
        ni.vy -= fy;
        nj.vx += fx;
        nj.vy += fy;
      }
    }

    // Link spring — pull connected nodes toward LINK_DISTANCE apart.
    for (const lk of links) {
      const si = nodeIndex.get(lk.source);
      const ti = nodeIndex.get(lk.target);
      if (si == null || ti == null) continue;
      const a = nodes[si];
      const b = nodes[ti];
      const dx = b.x - a.x;
      const dy = b.y - a.y;
      const dist = Math.sqrt(dx * dx + dy * dy) || 1;
      const diff = (dist - LINK_DISTANCE) * 0.05;
      const fx = (dx / dist) * diff;
      const fy = (dy / dist) * diff;
      a.vx += fx;
      a.vy += fy;
      b.vx -= fx;
      b.vy -= fy;
    }

    // Centering — pull everything toward the canvas center so the layout
    // doesn't drift off-screen.
    const cx = width / 2;
    const cy = height / 2;
    for (const n of nodes) {
      n.vx += (cx - n.x) * CENTER_STRENGTH;
      n.vy += (cy - n.y) * CENTER_STRENGTH;
      // Apply velocity + damping.
      if (n.fx == null) n.x += n.vx;
      if (n.fy == null) n.y += n.vy;
      n.vx *= DAMPING;
      n.vy *= DAMPING;
    }

    // Trigger Svelte rerender.
    nodes = nodes;
  }

  /*
   * Why: Drag-to-pin lets the operator nudge a cluttered cluster into
   * place. Standard d3 idiom: on `mousedown` pin the node, on `mousemove`
   * move it, on `mouseup` release.
   * What: Pins by setting fx/fy; unpins by setting them back to null. The
   * `tick()` loop respects fx/fy when set, so dragged nodes stay put.
   * Test: drag a node — the rest of the graph reshapes around it.
   */
  let dragId = null;
  function onNodeDown(ev, id) {
    dragId = id;
    selectedId = id;
    const node = nodes.find((n) => n.id === id);
    if (node) {
      node.fx = node.x;
      node.fy = node.y;
    }
    ev.stopPropagation();
  }
  function onSvgMove(ev) {
    if (dragId == null) return;
    const pt = clientToSvg(ev.clientX, ev.clientY);
    const node = nodes.find((n) => n.id === dragId);
    if (node) {
      node.fx = pt.x;
      node.fy = pt.y;
      node.x = pt.x;
      node.y = pt.y;
      nodes = nodes;
    }
  }
  function onSvgUp() {
    if (dragId == null) return;
    const node = nodes.find((n) => n.id === dragId);
    if (node) {
      node.fx = null;
      node.fy = null;
    }
    dragId = null;
  }
  function clientToSvg(cx, cy) {
    if (!svgEl) return { x: cx, y: cy };
    const r = svgEl.getBoundingClientRect();
    return { x: cx - r.left, y: cy - r.top };
  }

  // Side-panel derived view: triples incident on the selected node, split
  // into outgoing and incoming for clarity. Also surfaces source drawer
  // ids extracted from `drawer:<uuid>` references so the operator can jump
  // from a node back to the drawer that produced it.
  let incident = $derived.by(() => {
    if (!selectedId) return { outgoing: [], incoming: [], drawerIds: [] };
    const outgoing = triples.filter((t) => t.subject === selectedId);
    const incoming = triples.filter((t) => t.object === selectedId);
    const drawerIds = new Set();
    for (const t of [...outgoing, ...incoming]) {
      if (typeof t.subject === 'string' && t.subject.startsWith('drawer:')) {
        drawerIds.add(t.subject.slice('drawer:'.length));
      }
      if (typeof t.object === 'string' && t.object.startsWith('drawer:')) {
        drawerIds.add(t.object.slice('drawer:'.length));
      }
    }
    return {
      outgoing,
      incoming,
      drawerIds: Array.from(drawerIds)
    };
  });

  function colorFor(node) {
    const palette = [
      '#6366f1',
      '#ec4899',
      '#10b981',
      '#f59e0b',
      '#0ea5e9',
      '#8b5cf6',
      '#14b8a6',
      '#f43f5e'
    ];
    if (counts.community_count > 0) {
      return palette[node.community % palette.length];
    }
    // Color-by-kind fallback when no Louvain pass has run.
    switch (node.kind) {
      case 'drawer':
        return '#6366f1';
      case 'tag':
        return '#10b981';
      case 'topic':
        return '#f59e0b';
      case 'room':
        return '#ec4899';
      default:
        return '#64748b';
    }
  }
</script>

<div class="page">
  <div class="header">
    <h1 class="page-title">Knowledge Graph</h1>
    <div class="header-meta">
      {#if palaceId}
        <span class="badge badge-info">palace: {palaceId}</span>
      {/if}
      <span class="badge badge-muted">{counts.node_count} nodes</span>
      <span class="badge badge-muted">{counts.edge_count} edges</span>
      {#if loadElapsedMs > 0 && !loading}
        <span class="badge badge-muted" title="API + layout time">
          loaded in {loadElapsedMs}ms
        </span>
      {/if}
      <button
        type="button"
        class="back-link"
        onclick={() => navigate('/palaces')}>
        ← back to palaces
      </button>
    </div>
  </div>

  {#if loading}
    <div class="state">Loading graph…</div>
  {:else if error}
    <div class="state state-error">{error}</div>
  {:else if nodes.length === 0}
    <div class="state">
      This palace has no KG triples yet. Write a memory or run
      <code>trusty-memory kg-rebuild --palace {palaceId}</code> to back-fill.
    </div>
  {:else}
    <div class="layout">
      <div class="canvas">
        <!-- svelte-ignore a11y_no_noninteractive_element_interactions -->
        <svg
          bind:this={svgEl}
          width={width}
          height={height}
          role="application"
          aria-label="Per-palace knowledge graph"
          onmousemove={onSvgMove}
          onmouseup={onSvgUp}
          onmouseleave={onSvgUp}>
          <defs>
            <marker
              id="arrow"
              viewBox="0 0 10 10"
              refX="9"
              refY="5"
              markerWidth="6"
              markerHeight="6"
              orient="auto-start-reverse">
              <path d="M0,0 L10,5 L0,10 z" fill="#94a3b8" />
            </marker>
          </defs>
          {#each links as l (l.source + '|' + l.predicate + '|' + l.target)}
            {@const a = nodes.find((n) => n.id === l.source)}
            {@const b = nodes.find((n) => n.id === l.target)}
            {#if a && b}
              <line
                x1={a.x}
                y1={a.y}
                x2={b.x}
                y2={b.y}
                stroke="#94a3b8"
                stroke-width="1"
                stroke-opacity="0.55"
                marker-end="url(#arrow)" />
            {/if}
          {/each}
          {#each nodes as n (n.id)}
            <g
              transform={`translate(${n.x},${n.y})`}
              onmousedown={(ev) => onNodeDown(ev, n.id)}
              onmouseenter={() => (hoverId = n.id)}
              onmouseleave={() => (hoverId = null)}
              role="button"
              tabindex="0"
              class="node">
              <circle
                r={selectedId === n.id ? 9 : 6}
                fill={colorFor(n)}
                stroke={selectedId === n.id ? '#0f172a' : '#fff'}
                stroke-width={selectedId === n.id ? 2 : 1} />
              {#if selectedId === n.id || hoverId === n.id}
                <text
                  x="10"
                  y="4"
                  font-size="11"
                  fill="#0f172a"
                  paint-order="stroke"
                  stroke="#fff"
                  stroke-width="3">
                  {n.label}
                </text>
              {/if}
            </g>
          {/each}
        </svg>
      </div>
      <aside class="side-panel">
        {#if selectedId}
          <div class="side-title">{selectedId}</div>
          <div class="side-sub">
            {incident.outgoing.length} outgoing · {incident.incoming.length} incoming
          </div>
          {#if incident.outgoing.length > 0}
            <div class="side-section">
              <div class="side-section-title">Outgoing</div>
              <ul class="side-list">
                {#each incident.outgoing as t (t.subject + t.predicate + t.object)}
                  <li>
                    <span class="pred">{t.predicate}</span>
                    <span class="arrow">→</span>
                    <span class="obj">{t.object}</span>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}
          {#if incident.incoming.length > 0}
            <div class="side-section">
              <div class="side-section-title">Incoming</div>
              <ul class="side-list">
                {#each incident.incoming as t (t.subject + t.predicate + t.object)}
                  <li>
                    <span class="obj">{t.subject}</span>
                    <span class="arrow">→</span>
                    <span class="pred">{t.predicate}</span>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}
          {#if incident.drawerIds.length > 0}
            <div class="side-section">
              <div class="side-section-title">Source drawers</div>
              <ul class="side-list">
                {#each incident.drawerIds as did}
                  <li>
                    <code>{did}</code>
                  </li>
                {/each}
              </ul>
            </div>
          {/if}
        {:else}
          <div class="side-empty">
            Click a node to inspect its edges and the source drawers that
            produced them.
          </div>
        {/if}
      </aside>
    </div>
  {/if}
</div>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-3) 0;
    font-weight: 600;
  }
  .header {
    margin-bottom: var(--trusty-space-4);
  }
  .header-meta {
    display: flex;
    gap: 8px;
    align-items: center;
    flex-wrap: wrap;
  }
  .back-link {
    background: transparent;
    border: 1px solid var(--trusty-border, #e5e7eb);
    border-radius: 4px;
    padding: 2px 8px;
    font-size: var(--trusty-fs-xs);
    cursor: pointer;
    color: var(--trusty-text-secondary, #6b7280);
    font-family: inherit;
  }
  .back-link:hover {
    background: var(--trusty-bg-subtle, #f8fafc);
  }
  .state {
    padding: var(--trusty-space-4);
    background: var(--trusty-bg-subtle, #f8fafc);
    border-radius: var(--trusty-radius, 6px);
    color: var(--trusty-text-secondary, #6b7280);
  }
  .state-error {
    color: var(--trusty-danger, #dc2626);
    background: var(--trusty-danger-soft, #fef2f2);
  }
  .layout {
    display: grid;
    grid-template-columns: 1fr 320px;
    gap: var(--trusty-space-4);
    align-items: start;
  }
  .canvas {
    border: 1px solid var(--trusty-border, #e5e7eb);
    border-radius: var(--trusty-radius, 6px);
    background: #fff;
    overflow: hidden;
    min-height: 420px;
  }
  .canvas svg {
    display: block;
    width: 100%;
    height: auto;
    user-select: none;
  }
  .node {
    cursor: pointer;
  }
  .side-panel {
    border: 1px solid var(--trusty-border, #e5e7eb);
    border-radius: var(--trusty-radius, 6px);
    padding: var(--trusty-space-3);
    background: #fff;
    max-height: 80vh;
    overflow-y: auto;
  }
  .side-title {
    font-weight: 600;
    font-size: var(--trusty-fs-sm);
    word-break: break-all;
    margin-bottom: 2px;
  }
  .side-sub {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-secondary, #6b7280);
    margin-bottom: var(--trusty-space-3);
  }
  .side-section {
    margin-top: var(--trusty-space-3);
  }
  .side-section-title {
    font-size: var(--trusty-fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--trusty-text-muted, #94a3b8);
    margin-bottom: 4px;
  }
  .side-list {
    list-style: none;
    padding: 0;
    margin: 0;
    font-size: var(--trusty-fs-xs);
  }
  .side-list li {
    padding: 3px 0;
    border-bottom: 1px dashed var(--trusty-border, #e5e7eb);
    word-break: break-all;
  }
  .side-empty {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-secondary, #6b7280);
  }
  .pred {
    color: var(--trusty-accent, #6366f1);
    font-weight: 500;
  }
  .arrow {
    color: var(--trusty-text-muted, #94a3b8);
    margin: 0 4px;
  }
  .obj {
    color: var(--trusty-text-primary, #0f172a);
  }
</style>
