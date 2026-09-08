/*
 * Why: `PalaceGraph.svelte` merges every payload — the bounded seed, a
 * click-to-expand hop, and the whole "load everything" graph — through one
 * function, and #7116 showed that function had no test of its own because it
 * lived inline in the component and closed over a dozen component locals.
 * Full-load derives its node list from triple endpoints, so the same id
 * arrives once per incident edge (10,000 entries for 1,242 distinct nodes);
 * the pre-fix code tested membership against a `byId` snapshot taken before
 * the loop, so every repeat was pushed and the keyed `{#each}` threw
 * `each_key_duplicate` — the view rendered zero nodes.
 * What: the same merge, as a pure function over an explicit graph state
 * object. Dedups nodes by `id` against a set that grows INSIDE the loop and
 * triples by (subject,predicate,object). Known nodes get their `degree`
 * refreshed (the server always reports graph-wide degree) but keep their
 * coordinates so the layout is stable. New nodes are seeded on a ring around
 * `originId` when there is one, so an expansion visibly grows out of the node
 * that was clicked.
 * Test: `src/lib/graph/mergeSubgraph.test.js`.
 */

/** Dedup key for a triple. */
export const tripleKey = (t) => `${t.subject} ${t.predicate} ${t.object}`;

/** Node colour class, derived from the id's namespace prefix. */
export function classify(label) {
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
export function hashStr(s) {
  let h = 5381;
  for (let i = 0; i < s.length; i++) {
    h = ((h << 5) + h) ^ s.charCodeAt(i);
  }
  return h | 0;
}

/**
 * A fresh, empty graph state. The component holds `nodes`/`triples` as runes
 * and hands them in, so this is for tests and for documenting the shape.
 */
export function createGraphState() {
  return {
    nodes: [],
    triples: [],
    nodeIds: new Set(),
    tripleKeys: new Set(),
    nodeIndex: new Map(),
    positions: new Float64Array(0),
    pinned: new Uint8Array(0)
  };
}

/**
 * Resize `positions`/`pinned` to hold at least `count` nodes, preserving what
 * is already there. Grows in place; never shrinks.
 */
export function growPositions(state, count) {
  if (state.positions.length >= count * 2) return;
  const next = new Float64Array(count * 2);
  next.set(state.positions);
  state.positions = next;
  const nextPinned = new Uint8Array(count);
  nextPinned.set(state.pinned);
  state.pinned = nextPinned;
}

/**
 * Merge `payload` into `state`, in place.
 *
 * `state.positions` and `state.pinned` may be REPLACED with larger arrays; the
 * caller must read them back off `state` afterwards.
 *
 * @param {object} state — `{nodes, triples, nodeIds, tripleKeys, nodeIndex, positions, pinned}`
 * @param {object} payload — `{nodes?: [{id, degree?}], triples?: [{subject, predicate, object}]}`
 * @param {string|null} originId — expansion origin, or null for a fresh load
 * @param {object} options — `{communityCount, width, height, linkDistance, random}`
 * @returns {{addedNodes: number, addedTriples: number}}
 */
export function mergeSubgraph(state, payload, originId, options = {}) {
  const {
    communityCount = 8,
    width = 900,
    height = 600,
    linkDistance = 90,
    random = Math.random
  } = options;
  const { nodes, triples, nodeIds, tripleKeys, nodeIndex } = state;

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
  growPositions(state, nodes.length + fresh);
  const positions = state.positions;
  const pinned = state.pinned;

  const ox = originIdx != null ? positions[2 * originIdx] : 0;
  const oy = originIdx != null ? positions[2 * originIdx + 1] : 0;
  let placed = 0;

  for (const n of incoming) {
    if (!n?.id) continue;
    /*
     * #7116: test against `nodeIds`, which grows INSIDE this loop, not against
     * the `byId` snapshot taken before it. Checking the snapshot let every
     * repeated endpoint through and the keyed `{#each}` then threw
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
      community: Math.abs(hashStr(n.id)) % Math.max(1, communityCount || 8),
      degree: typeof n.degree === 'number' ? n.degree : 0,
      expanded: false,
      isNew: true
    });
    if (originIdx != null) {
      // Ring placement around the expansion origin. Radius scales with the
      // batch size so a 40-neighbour hub does not stack them on top of each
      // other.
      const angle = (placed / Math.max(1, fresh)) * Math.PI * 2;
      const radius = linkDistance * (0.9 + fresh / 40);
      positions[2 * idx] = ox + Math.cos(angle) * radius;
      positions[2 * idx + 1] = oy + Math.sin(angle) * radius;
    } else {
      positions[2 * idx] = width / 2 + (random() - 0.5) * 240;
      positions[2 * idx + 1] = height / 2 + (random() - 0.5) * 240;
    }
    pinned[idx] = 0;
    placed++;
  }

  let addedTriples = 0;
  for (const t of payload?.triples ?? []) {
    const key = tripleKey(t);
    if (tripleKeys.has(key)) continue;
    tripleKeys.add(key);
    triples.push(t);
    addedTriples++;
  }

  return { addedNodes: placed, addedTriples };
}
