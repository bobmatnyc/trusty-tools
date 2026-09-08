<script>
  /*
   * Why: Issue #97 gave every palace a per-palace visual knowledge graph so
   * operators can spot clusters and inspect auto-extracted triples. Issue
   * #4670 fixed how it loads: it used to fetch the ENTIRE graph in one call
   * and lay it out with an O(n²) force sim explicitly budgeted for "<500
   * triples". Real palaces now hold 8,266 triples / 9,311 nodes — 16.5x that
   * budget — and the server silently capped the payload at 5,000 triples while
   * the header badge still read the full node count, so the view presented a
   * partial graph as complete.
   * What: loads a bounded top-degree SEED (`GET /kg/graph/seed`) on mount,
   * expands on click (`GET /kg/graph/neighbors`, direction-aware — the only
   * way to reach a node's INCOMING edges), merges results into the rendered
   * set deduplicated by node id and by (subject,predicate,object), and pins
   * existing node positions during re-layout so expansion grows outward
   * instead of reshuffling. Full-graph load stays available as an explicit,
   * size-warned opt-in. The header always states what is rendered vs. what
   * exists.
   *
   * #7116 changed how it RENDERS. The simulation used to run on
   * `setInterval(tick, 16)` and reassign the reactive `nodes` array at the end
   * of every tick, so all 200 steps of a full re-layout each repainted the
   * whole SVG. On the live trusty-tools palace "load everything" draws 1,242
   * nodes and 5,000 edges, so that was ~6,300 SVG elements repainted 200 times
   * and a tab frozen for minutes. The steps cost 3.15 ms each (0.6 s for all
   * 200) — the paints were the whole cost. Three changes fix it: the
   * simulation moved into a Web Worker and commits at most one snapshot per
   * animation frame (`graph/layoutRunner.js`); coordinates moved out of the
   * reactive `nodes` array into a plain `Float64Array` plus a `posVersion`
   * counter, so a new frame no longer invalidates `links`/`shownDegree`; and
   * above `EDGE_PATH_THRESHOLD` edges collapse into one `<path>` instead of
   * one `<line>` each.
   * Test: `src/lib/graph/forceLayout.test.js` and
   * `src/lib/graph/layoutRunner.test.js`. In the browser: open
   * `#/palace/<id>/graph`, confirm the header reads "N of M nodes shown",
   * click a node and confirm new nodes appear around it while existing ones do
   * not move, then use "load everything" and confirm the progress bar advances
   * while the page still scrolls and the cancel button still responds.
   */
  import { onDestroy, onMount } from 'svelte';
  import { api } from '../api.js';
  import { getRoute, navigate } from '../router.svelte.js';
  import { EXPAND_STEPS, MAX_STEPS, stepBudgetFor } from '../graph/forceLayout.js';
  import { runLayout } from '../graph/layoutRunner.js';

  // Selected palace + payload.
  let palaceId = $state('');
  let triples = $state([]);
  // Palace-wide truth. `rendered*` below is what is actually on screen.
  let counts = $state({ node_count: 0, edge_count: 0, community_count: 0 });
  let loading = $state(true);
  let expanding = $state(null); // node id currently being expanded
  let error = $state(null);
  let notice = $state(null);
  let loadStartedAt = 0;
  let loadElapsedMs = $state(0);
  /** 'seed' (progressive, default) or 'full' (explicit opt-in). */
  let mode = $state('seed');
  /** True when the daemon told us the full-graph payload was capped. */
  let serverTruncated = $state(false);

  /*
   * #7116: `nodes` carries IDENTITY only — no coordinates. Coordinates live in
   * the plain `positions` array below, and a frame commit bumps `posVersion`
   * instead of reassigning `nodes`. That is what stops each of the ~20 frames
   * of a full layout from re-deriving `links` (a 5,000-triple scan) and
   * `shownDegree` (a 5,000-link scan), which the pre-fix code did 200 times.
   */
  let nodes = $state([]); // {id,label,kind,community,degree,expanded}
  let selectedId = $state(null);
  let hoverId = $state(null);

  /** Interleaved [x0,y0,x1,y1,…], indexed by position in `nodes`. */
  let positions = new Float64Array(0);
  /** 1 = held in place (dragged, or pinned during an expansion re-layout). */
  let pinned = new Uint8Array(0);
  /** Bumped on every committed frame; the only reactive coordinate signal. */
  let posVersion = $state(0);
  /** Live layout progress, or null when nothing is running. */
  let layoutProgress = $state(null);
  /** Set when `stepBudgetFor` clipped the step count for this graph's size. */
  let budgetNotice = $state(null);

  // Dedup keys. Kept outside $state — they are bookkeeping, not view data.
  let nodeIds = new Set();
  let tripleKeys = new Set();
  /** id -> index into `nodes` / `positions`. Nodes only ever append. */
  let nodeIndex = new Map();

  // Viewport sizing — recalculated on mount + window resize.
  let width = $state(900);
  let height = $state(600);
  let svgEl = $state(null);
  /** Handle for the running layout, or null. `.cancel()` stops it. */
  let simHandle = null;

  /** Seed placement radius. Must match `forceLayout.js`'s `linkDistance`. */
  const LINK_DISTANCE = 90;

  /*
   * #7116: above this many rendered edges, draw them as ONE `<path>` rather
   * than one `<line>` each. At 5,000 edges the per-line form costs 5,000
   * elements and 20,000 coordinate attribute writes per frame; the merged path
   * costs one element and one attribute. Below the threshold the per-line form
   * stays, because it keeps the arrow markers that make direction readable —
   * and at 328 edges (the seed view) its cost is irrelevant.
   */
  const EDGE_PATH_THRESHOLD = 400;

  /*
   * Why: the client-side force sim is O(n²) per tick. 75 nodes is ~2.8K pair
   * computations per tick; the palace's full 9,311 nodes is ~43M — the freeze
   * this view exists to avoid. 75 also reaches past the top hubs into the
   * mid-degree tier (measured: 91.8% of nodes are degree-1 leaves, only ~7%
   * have degree >= 5), so first paint shows real structure rather than five
   * stars. The server clamps to [1, 200] independently.
   */
  const SEED_LIMIT = 75;
  /** Above this, "load everything" warns before it runs. */
  const FULL_LOAD_WARN_NODES = 500;

  onMount(() => {
    palaceId = palaceFromRoute();
    if (palaceId) loadSeed();
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
      loadSeed();
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
      height = Math.max(420, Math.floor(Math.min(800, window.innerHeight - 240)));
    }
  }

  // -------------------------------------------------------------------------
  // Loading
  // -------------------------------------------------------------------------

  function resetGraph() {
    stopSimulation();
    nodes = [];
    triples = [];
    nodeIds = new Set();
    tripleKeys = new Set();
    nodeIndex = new Map();
    positions = new Float64Array(0);
    pinned = new Uint8Array(0);
    posVersion++;
    selectedId = null;
    serverTruncated = false;
    notice = null;
    budgetNotice = null;
    layoutProgress = null;
  }

  /*
   * Why (#4670): the default load. Fetches only the top-degree skeleton so
   * first paint is bounded regardless of palace size, and records the
   * palace-wide totals so the header can be honest about the gap.
   */
  async function loadSeed() {
    loading = true;
    error = null;
    mode = 'seed';
    loadStartedAt = performance.now();
    resetGraph();
    try {
      const payload = await api.kgGraphSeed(palaceId, SEED_LIMIT);
      counts = {
        node_count: payload?.node_count ?? 0,
        edge_count: payload?.edge_count ?? 0,
        community_count: payload?.community_count ?? 0
      };
      mergeSubgraph(payload, null);
      relayout({ pinExisting: false, steps: MAX_STEPS });
      await autoExpandIfEdgeless();
    } catch (e) {
      error = e.message || String(e);
      resetGraph();
    } finally {
      loading = false;
      loadElapsedMs = Math.round(performance.now() - loadStartedAt);
    }
  }

  /*
   * Why (#4670): measured on the live 8,266-triple trusty-tools palace, only
   * 0.48% of edges connect two nodes of degree >= 2 — the graph is
   * overwhelmingly a star forest of `drawer:<uuid>` hubs with degree-1 leaves.
   * The top-degree hubs are therefore pairwise UNCONNECTED, so the seed's
   * induced subgraph comes back with zero edges and the first paint would be
   * 75 disconnected dots. Auto-expanding the single highest-degree node turns
   * that into one readable star the operator can navigate from, without
   * pretending the rest of the graph is loaded (the badge and the dashed halos
   * still say otherwise).
   * What: no-op whenever the seed already has edges — i.e. on any
   * densely-connected palace this never fires.
   */
  async function autoExpandIfEdgeless() {
    if (links.length > 0 || nodes.length === 0) return;
    const top = nodes.reduce((a, b) => ((b.degree ?? 0) > (a.degree ?? 0) ? b : a));
    if ((top.degree ?? 0) === 0) return;
    selectedId = top.id;
    await expandNode(top.id);
  }

  /*
   * Why (#4670): kept as an explicit opt-in, never the default. The payload is
   * server-capped at 5,000 triples; `truncated` tells us when what we drew is
   * still not everything, and we say so rather than implying completeness.
   */
  async function loadFull() {
    // #7116: the old text promised a freeze, which was accurate then and is
    // wrong now. It states the size and the server cap instead.
    if (
      counts.node_count > FULL_LOAD_WARN_NODES &&
      !window.confirm(
        `This palace has ${counts.node_count.toLocaleString()} nodes and ` +
          `${counts.edge_count.toLocaleString()} edges. The daemon caps this ` +
          `request, so you will get a large subset rather than all of it, and ` +
          `laying it out takes a few seconds. The page stays usable while it ` +
          `runs and you can cancel it. Continue?`
      )
    ) {
      return;
    }
    loading = true;
    error = null;
    mode = 'full';
    loadStartedAt = performance.now();
    resetGraph();
    try {
      const payload = await api.kgGraph(palaceId);
      counts = {
        node_count: payload?.node_count ?? 0,
        edge_count: payload?.edge_count ?? 0,
        community_count: payload?.community_count ?? 0
      };
      serverTruncated = payload?.truncated === true;
      if (serverTruncated) {
        notice =
          `The daemon capped this response at ` +
          `${(payload?.returned_triple_count ?? 0).toLocaleString()} of ` +
          `${(payload?.active_triple_count ?? 0).toLocaleString()} active triples ` +
          `(newest first). This is still not the whole graph.`;
      }
      // Full mode has no node list — derive nodes from the triple endpoints.
      const derived = [];
      for (const t of payload?.triples ?? []) {
        derived.push({ id: t.subject }, { id: t.object });
      }
      mergeSubgraph({ nodes: derived, triples: payload?.triples ?? [] }, null);
      relayout({ pinExisting: false, steps: MAX_STEPS });
    } catch (e) {
      error = e.message || String(e);
      resetGraph();
    } finally {
      loading = false;
      loadElapsedMs = Math.round(performance.now() - loadStartedAt);
    }
  }

  /*
   * Why (#4670): click-to-expand. `direction=both` is the point — the incoming
   * half of the graph had no HTTP route before this endpoint, so a node's
   * "what points at me" edges were simply unreachable.
   * What: fetches one hop around `id`, merges, and re-settles ONLY the newly
   * added nodes (see `relayout`), so the operator's existing mental map of the
   * canvas survives the expansion.
   */
  async function expandNode(id) {
    if (!id || expanding || mode === 'full') return;
    const node = nodeById.get(id);
    if (node?.expanded) return;
    expanding = id;
    error = null;
    try {
      const payload = await api.kgNeighbors(palaceId, id, {
        direction: 'both',
        maxHops: 1
      });
      mergeSubgraph(payload, id);
      if (node) node.expanded = true;
      relayout({ pinExisting: true, steps: EXPAND_STEPS });
    } catch (e) {
      error = e.message || String(e);
    } finally {
      expanding = null;
    }
  }

  // -------------------------------------------------------------------------
  // Merge
  // -------------------------------------------------------------------------

  const tripleKey = (t) => `${t.subject} ${t.predicate} ${t.object}`;

  /*
   * Why: expansion results overlap what is already drawn; adding a node or an
   * edge twice would double-render it and corrupt the layout's link forces.
   * What: dedups nodes by `id` and triples by (subject,predicate,object).
   * Already-known nodes get their `degree` refreshed (the server always
   * reports graph-wide degree) but keep their x/y so the layout is stable.
   * New nodes are seeded on a small ring around `originId` when there is one,
   * so an expansion visibly grows out of the node that was clicked.
   */
  function mergeSubgraph(payload, originId) {
    const byId = new Map();
    for (const n of nodes) byId.set(n.id, n);
    const originIdx = originId != null ? nodeIndex.get(originId) : undefined;
    const incoming = payload?.nodes ?? [];
    // #7116: DISTINCT new ids. Full-load's derived list repeats an id once per
    // incident edge, and a count including repeats both over-allocates and
    // squashes the ring placement below.
    const freshIds = new Set();
    for (const n of incoming) if (n?.id && !nodeIds.has(n.id)) freshIds.add(n.id);
    const fresh = freshIds.size;

    // #7116: grow the coordinate arrays once for the whole batch rather than
    // reallocating per node.
    growPositions(nodes.length + fresh);

    const ox = originIdx != null ? positions[2 * originIdx] : 0;
    const oy = originIdx != null ? positions[2 * originIdx + 1] : 0;
    let placed = 0;

    for (const n of incoming) {
      if (!n?.id) continue;
      /*
       * #7116: test against `nodeIds`, which grows INSIDE this loop, not
       * against the `byId` snapshot taken before it. Full-load derives its node
       * list from triple endpoints, so the same id arrives once per incident
       * edge — 10,000 entries for 1,242 distinct nodes. Checking the snapshot
       * let every repeat through and the keyed `{#each}` then threw
       * `each_key_duplicate`, which is why "load everything" rendered nothing
       * even once it stopped freezing.
       */
      if (nodeIds.has(n.id)) {
        const existing = byId.get(n.id);
        if (existing && typeof n.degree === 'number') existing.degree = n.degree;
        continue;
      }
      nodeIds.add(n.id);
      const idx = nodes.length;
      nodeIndex.set(n.id, idx);
      nodes.push({
        id: n.id,
        label: n.id,
        kind: classify(n.id),
        community: Math.abs(hashStr(n.id)) % Math.max(1, counts.community_count || 8),
        degree: typeof n.degree === 'number' ? n.degree : 0,
        expanded: false,
        isNew: true
      });
      if (originIdx != null) {
        // Ring placement around the expansion origin. Radius scales with the
        // batch size so a 40-neighbour hub does not stack them on top of
        // each other.
        const angle = (placed / Math.max(1, fresh)) * Math.PI * 2;
        const radius = LINK_DISTANCE * (0.9 + fresh / 40);
        positions[2 * idx] = ox + Math.cos(angle) * radius;
        positions[2 * idx + 1] = oy + Math.sin(angle) * radius;
      } else {
        positions[2 * idx] = width / 2 + (Math.random() - 0.5) * 240;
        positions[2 * idx + 1] = height / 2 + (Math.random() - 0.5) * 240;
      }
      pinned[idx] = 0;
      placed++;
    }

    for (const t of payload?.triples ?? []) {
      const key = tripleKey(t);
      if (tripleKeys.has(key)) continue;
      tripleKeys.add(key);
      triples.push(t);
    }
    posVersion++;
  }

  /**
   * Resize `positions`/`pinned` to hold at least `count` nodes, preserving
   * what is already there. Grows in place; never shrinks (a mode switch calls
   * `resetGraph` instead).
   */
  function growPositions(count) {
    if (positions.length >= count * 2) return;
    const next = new Float64Array(count * 2);
    next.set(positions);
    positions = next;
    const nextPinned = new Uint8Array(count);
    nextPinned.set(pinned);
    pinned = nextPinned;
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
   */
  function hashStr(s) {
    let h = 5381;
    for (let i = 0; i < s.length; i++) {
      h = ((h << 5) + h) ^ s.charCodeAt(i);
    }
    return h | 0;
  }

  // -------------------------------------------------------------------------
  // Layout
  // -------------------------------------------------------------------------

  /*
   * Why (#4670): a naive re-run of the whole simulation after every expansion
   * throws the operator's mental map away — every node jumps. Pinning the
   * nodes that were already on screen means the existing layout is literally
   * frozen and only the new arrivals settle, so expansion reads as "the graph
   * grew here" rather than "the graph was replaced".
   * Why (#7116): the run itself now happens in a Web Worker and lands here one
   * committed frame at a time, so a 1,242-node re-layout costs ~20 paints
   * instead of 200 and never occupies the main thread between them.
   * What: pins every non-new node, hands the whole graph to `runLayout`, and
   * releases the pins when it finishes. Pin/pin pairs are skipped in the
   * repulsion loop since neither can move — that also keeps the O(n²) pass
   * proportional to the number of NEW nodes, not the total.
   */
  function relayout({ pinExisting, steps }) {
    stopSimulation();
    if (nodes.length === 0) {
      layoutProgress = null;
      return;
    }

    for (let i = 0; i < nodes.length; i++) {
      pinned[i] = pinExisting && !nodes[i].isNew ? 1 : 0;
    }

    // #7116: clip the step count so a bigger palace cannot multiply the
    // runtime quadratically, and say so rather than quietly doing less work.
    const budget = stepBudgetFor(nodes.length, steps);
    budgetNotice =
      budget < steps
        ? `Layout ran ${budget} of ${steps} settling passes because this graph ` +
          `has ${nodes.length.toLocaleString()} nodes. Clusters are placed; ` +
          `fine positioning is not converged.`
        : null;

    const { source, target } = linkIndices();
    layoutProgress = { step: 0, total: budget };

    simHandle = runLayout({
      spec: {
        count: nodes.length,
        // The worker gets its own copy; `positions` stays ours to render from.
        positions: positions.slice(0, nodes.length * 2),
        linkSource: source,
        linkTarget: target,
        pinned: pinned.slice(0, nodes.length),
        width,
        height
      },
      steps: budget,
      onFrame: commitFrame,
      onDone: () => {
        for (const n of nodes) n.isNew = false;
        pinned.fill(0, 0, nodes.length);
        layoutProgress = null;
        simHandle = null;
        loadElapsedMs = Math.round(performance.now() - loadStartedAt);
      }
    });
  }

  /*
   * Why (#7116): THE fix. A committed frame writes coordinates into the plain
   * `positions` array and bumps one counter. It does not touch `nodes`, so
   * `links` and `shownDegree` — each a 5,000-element scan — are not re-derived,
   * and the template updates only what actually moved.
   */
  function commitFrame(next, info) {
    positions.set(next.subarray(0, Math.min(next.length, positions.length)));
    posVersion++;
    if (layoutProgress) layoutProgress = { step: info.step, total: info.total };
  }

  /** Rendered links as index pairs, which is what the simulation consumes. */
  function linkIndices() {
    const ls = links;
    const source = new Int32Array(ls.length);
    const target = new Int32Array(ls.length);
    for (let i = 0; i < ls.length; i++) {
      source[i] = ls[i].si;
      target[i] = ls[i].ti;
    }
    return { source, target };
  }

  function stopSimulation() {
    simHandle?.cancel();
    simHandle = null;
    layoutProgress = null;
  }

  /** Operator-facing cancel — keeps whatever positions were last committed. */
  function cancelLayout() {
    stopSimulation();
    for (const n of nodes) n.isNew = false;
    pinned.fill(0, 0, nodes.length);
    loadElapsedMs = Math.round(performance.now() - loadStartedAt);
  }

  // -------------------------------------------------------------------------
  // Interaction
  // -------------------------------------------------------------------------

  /*
   * Why: drag-to-pin (issue #97) and click-to-expand (#4670) share the same
   * mouse gesture. Distinguishing them by travel distance keeps both: a press
   * that moves less than DRAG_SLOP px is a click and expands the node; a press
   * that moves further is a drag and only repositions it.
   */
  const DRAG_SLOP = 4;
  let dragId = null;
  let dragStart = null;
  let dragMoved = false;

  function onNodeDown(ev, id) {
    dragId = id;
    dragMoved = false;
    dragStart = { x: ev.clientX, y: ev.clientY };
    selectedId = id;
    // #7116: pin/position by index — coordinates no longer live on the node.
    const idx = nodeIndex.get(id);
    if (idx != null) pinned[idx] = 1;
    ev.stopPropagation();
  }
  function onSvgMove(ev) {
    if (dragId == null) return;
    if (
      dragStart &&
      Math.hypot(ev.clientX - dragStart.x, ev.clientY - dragStart.y) > DRAG_SLOP
    ) {
      dragMoved = true;
    }
    const pt = clientToSvg(ev.clientX, ev.clientY);
    const idx = nodeIndex.get(dragId);
    if (idx != null) {
      positions[2 * idx] = pt.x;
      positions[2 * idx + 1] = pt.y;
      posVersion++;
    }
  }
  function onSvgUp() {
    if (dragId == null) return;
    const id = dragId;
    const idx = nodeIndex.get(id);
    if (idx != null) pinned[idx] = 0;
    dragId = null;
    dragStart = null;
    if (!dragMoved) expandNode(id);
    dragMoved = false;
  }
  function clientToSvg(cx, cy) {
    if (!svgEl) return { x: cx, y: cy };
    const r = svgEl.getBoundingClientRect();
    return { x: cx - r.left, y: cy - r.top };
  }

  // -------------------------------------------------------------------------
  // Derived views
  // -------------------------------------------------------------------------

  /*
   * #7116: everything in this block depends on `nodes` and `triples` — graph
   * MEMBERSHIP — and on nothing positional. A committed layout frame bumps
   * `posVersion` and leaves all of it alone, which is why ~20 frames of a
   * 1,242-node layout no longer re-run a 5,000-element scan apiece.
   */

  /** id -> node, so hit-testing and the side panel are O(1) not O(n). */
  let nodeById = $derived.by(() => {
    const m = new Map();
    for (const n of nodes) m.set(n.id, n);
    return m;
  });

  /*
   * Rendered edges, as INDEX pairs. Triples touching a node we have not loaded
   * are dropped. Indices rather than ids because both the simulation and the
   * edge-path builder look coordinates up by index.
   */
  let links = $derived.by(() => {
    const out = [];
    // Gate on `nodeById` (reactive) rather than the plain `nodeIds` Set, so a
    // merge that adds nodes without adding triples still refreshes the edges.
    const present = nodeById;
    for (const t of triples) {
      if (t.subject === t.object) continue;
      if (!present.has(t.subject) || !present.has(t.object)) continue;
      const si = nodeIndex.get(t.subject);
      const ti = nodeIndex.get(t.object);
      if (si == null || ti == null) continue;
      out.push({ si, ti, source: t.subject, target: t.object, predicate: t.predicate });
    }
    return out;
  });

  /** How many of each node's edges are currently on screen. */
  let shownDegree = $derived.by(() => {
    const m = new Map();
    for (const l of links) {
      m.set(l.source, (m.get(l.source) ?? 0) + 1);
      m.set(l.target, (m.get(l.target) ?? 0) + 1);
    }
    return m;
  });

  /*
   * The one reactive coordinate read. Reading `posVersion` here registers the
   * dependency, so any template expression that goes through `frame` re-runs on
   * a committed layout frame and nothing else does.
   */
  let frame = $derived.by(() => {
    posVersion;
    return positions;
  });

  /** True when edges render as one merged `<path>` rather than one per line. */
  let mergedEdges = $derived(links.length > EDGE_PATH_THRESHOLD);

  /*
   * #7116: the merged form. One `d` attribute replaces 5,000 elements and the
   * 20,000 coordinate writes that updating them cost. Arrow markers are the
   * trade — at this density they were unreadable overlap anyway, and below the
   * threshold the per-line renderer still draws them.
   */
  let edgePath = $derived.by(() => {
    if (!mergedEdges) return '';
    const p = frame;
    const parts = [];
    for (const l of links) {
      parts.push(
        `M${p[2 * l.si].toFixed(1)} ${p[2 * l.si + 1].toFixed(1)}` +
          `L${p[2 * l.ti].toFixed(1)} ${p[2 * l.ti + 1].toFixed(1)}`
      );
    }
    return parts.join('');
  });

  // Side-panel derived view: triples incident on the selected node that we
  // have actually loaded, split into outgoing and incoming.
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
    return { outgoing, incoming, drawerIds: Array.from(drawerIds) };
  });

  let selectedNode = $derived(selectedId ? nodeById.get(selectedId) : null);
  /** True when the rendered set is a strict subset of the palace. */
  let partial = $derived(
    nodes.length < counts.node_count || links.length < counts.edge_count
  );

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

  /** Radius grows with degree so hubs read as hubs at a glance. */
  function radiusFor(node) {
    const base = 4 + Math.min(7, Math.sqrt(node.degree ?? 0) * 1.6);
    return selectedId === node.id ? base + 3 : base;
  }

  /** True when this node still has edges we have not fetched. */
  function hasHiddenEdges(node) {
    return (node.degree ?? 0) > (shownDegree.get(node.id) ?? 0);
  }
</script>

<div class="page">
  <div class="header">
    <h1 class="page-title">Knowledge Graph</h1>
    <div class="header-meta">
      {#if palaceId}
        <span class="badge badge-info">palace: {palaceId}</span>
      {/if}
      <!--
        #4670: the load-bearing badge. The old header printed the palace-wide
        `node_count` unconditionally next to a truncated render, which is what
        made the truncation invisible. It now always states rendered-vs-total.
      -->
      <span class="badge" class:badge-warn={partial} class:badge-muted={!partial}>
        {nodes.length.toLocaleString()} of {counts.node_count.toLocaleString()} nodes
        {#if partial}shown{/if}
      </span>
      <span class="badge" class:badge-warn={partial} class:badge-muted={!partial}>
        {links.length.toLocaleString()} of {counts.edge_count.toLocaleString()} edges
      </span>
      {#if partial && mode === 'seed'}
        <span class="hint">click a node to expand</span>
      {/if}
      {#if loadElapsedMs > 0 && !loading}
        <span class="badge badge-muted" title="API + layout time">
          loaded in {loadElapsedMs}ms
        </span>
      {/if}
      {#if mode === 'seed'}
        <button type="button" class="back-link" onclick={loadFull}>
          load everything
        </button>
      {:else}
        <button type="button" class="back-link" onclick={loadSeed}>
          back to seed view
        </button>
      {/if}
      <button type="button" class="back-link" onclick={() => navigate('/palaces')}>
        ← back to palaces
      </button>
    </div>
  </div>

  {#if notice}
    <div class="state state-warn">{notice}</div>
  {/if}

  <!--
    #7116: the layout no longer blocks, so it needs to say it is running and
    offer a way out. Cancel keeps whatever positions were last committed.
  -->
  {#if layoutProgress}
    <div class="layout-progress">
      <div class="layout-progress-row">
        <span>
          Laying out {nodes.length.toLocaleString()} nodes and
          {links.length.toLocaleString()} edges — pass {layoutProgress.step} of
          {layoutProgress.total}
        </span>
        <button type="button" class="back-link" onclick={cancelLayout}>cancel</button>
      </div>
      <div class="layout-progress-track">
        <div
          class="layout-progress-fill"
          style={`width:${Math.round(
            (layoutProgress.step / Math.max(1, layoutProgress.total)) * 100
          )}%`}>
        </div>
      </div>
    </div>
  {/if}

  {#if budgetNotice}
    <div class="state state-warn">{budgetNotice}</div>
  {/if}

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
          <!--
            #7116: two edge renderers. Above EDGE_PATH_THRESHOLD every edge is
            one segment of a single `<path>`, so a layout frame writes one
            attribute instead of updating 5,000 elements. Below it, one `<line>`
            each, which keeps the direction arrows.
          -->
          {#if mergedEdges}
            <path
              d={edgePath}
              fill="none"
              stroke="#94a3b8"
              stroke-width="1"
              stroke-opacity="0.35" />
          {:else}
            {#each links as l (l.source + '|' + l.predicate + '|' + l.target)}
              <line
                x1={frame[2 * l.si]}
                y1={frame[2 * l.si + 1]}
                x2={frame[2 * l.ti]}
                y2={frame[2 * l.ti + 1]}
                stroke="#94a3b8"
                stroke-width="1"
                stroke-opacity="0.55"
                marker-end="url(#arrow)" />
            {/each}
          {/if}
          {#each nodes as n, i (n.id)}
            <g
              transform={`translate(${frame[2 * i]},${frame[2 * i + 1]})`}
              onmousedown={(ev) => onNodeDown(ev, n.id)}
              onmouseenter={() => (hoverId = n.id)}
              onmouseleave={() => (hoverId = null)}
              role="button"
              tabindex="0"
              class="node">
              {#if hasHiddenEdges(n) && mode === 'seed'}
                <!-- Dashed halo = this node has edges not yet fetched. -->
                <circle
                  r={radiusFor(n) + 4}
                  fill="none"
                  stroke={colorFor(n)}
                  stroke-width="1"
                  stroke-opacity={expanding === n.id ? 0.9 : 0.45}
                  stroke-dasharray="2 3" />
              {/if}
              <circle
                r={radiusFor(n)}
                fill={colorFor(n)}
                stroke={selectedId === n.id ? '#0f172a' : '#fff'}
                stroke-width={selectedId === n.id ? 2 : 1} />
              {#if selectedId === n.id || hoverId === n.id}
                <text
                  x={radiusFor(n) + 4}
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
            {#if selectedNode}
              · {selectedNode.degree ?? 0} total in palace
            {/if}
          </div>
          {#if mode === 'seed' && selectedNode && hasHiddenEdges(selectedNode)}
            <button
              type="button"
              class="expand-btn"
              disabled={expanding != null}
              onclick={() => expandNode(selectedId)}>
              {expanding === selectedId ? 'expanding…' : 'expand neighbours'}
            </button>
          {/if}
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
            {#if mode === 'seed'}
              Showing the {nodes.length} highest-connected nodes. Click a node to
              load its neighbours (a dashed halo means it has edges not yet
              fetched), or drag it to reposition.
            {:else}
              Click a node to inspect its edges and the source drawers that
              produced them.
            {/if}
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
  .hint {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-secondary, #6b7280);
  }
  .badge-warn {
    background: var(--trusty-warn-soft, #fffbeb);
    color: var(--trusty-warn, #b45309);
    border: 1px solid var(--trusty-warn-border, #fde68a);
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
    background: var(--trusty-surface-raised, #f8fafc);
  }
  .expand-btn {
    margin: var(--trusty-space-2) 0;
    width: 100%;
    background: transparent;
    border: 1px solid var(--trusty-accent, #6366f1);
    color: var(--trusty-accent, #6366f1);
    border-radius: 4px;
    padding: 4px 8px;
    font-size: var(--trusty-fs-xs);
    font-family: inherit;
    cursor: pointer;
  }
  .expand-btn:disabled {
    opacity: 0.5;
    cursor: default;
  }
  .state {
    padding: var(--trusty-space-4);
    background: var(--trusty-surface-raised, #f8fafc);
    border-radius: var(--trusty-radius, 6px);
    color: var(--trusty-text-secondary, #6b7280);
    margin-bottom: var(--trusty-space-3);
  }
  .state-error {
    color: var(--trusty-danger, #dc2626);
    background: var(--trusty-danger-soft, #fef2f2);
  }
  .state-warn {
    color: var(--trusty-warn, #b45309);
    background: var(--trusty-warn-soft, #fffbeb);
  }
  /* #7116: layout-run progress. Foundry tokens only — no new palette. */
  .layout-progress {
    padding: var(--trusty-space-3) var(--trusty-space-4);
    background: var(--trusty-surface-raised);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    margin-bottom: var(--trusty-space-3);
  }
  .layout-progress-row {
    display: flex;
    align-items: center;
    justify-content: space-between;
    gap: var(--trusty-space-3);
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-secondary);
    margin-bottom: var(--trusty-space-2);
  }
  .layout-progress-track {
    height: var(--trusty-space-1);
    background: var(--trusty-border);
    border-radius: var(--trusty-radius-sm);
    overflow: hidden;
  }
  .layout-progress-fill {
    height: 100%;
    background: var(--trusty-accent);
    transition: width 120ms linear;
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
