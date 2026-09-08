/*
 * Why (#7116): "load everything" rendered zero nodes. `loadFull` has no node
 * list from the server, so it derives one from the triple ENDPOINTS — the same
 * id therefore arrives once per incident edge (10,000 entries for 1,242
 * distinct nodes on the live trusty-tools palace). The merge tested membership
 * against a `byId` snapshot taken before its loop, so every repeat was pushed
 * and the keyed `{#each}` threw `each_key_duplicate`.
 * What: drives the extracted merge with endpoint-derived payloads that repeat
 * ids across edges, and asserts each id lands exactly once while nodes that
 * were already on the canvas keep their identity and coordinates.
 */
import { describe, expect, it } from 'vitest';
import { createGraphState, mergeSubgraph } from './mergeSubgraph.js';

/** The endpoint-derived node list `loadFull` builds: two entries per triple. */
function derivedNodes(triples) {
  const out = [];
  for (const t of triples) out.push({ id: t.subject }, { id: t.object });
  return out;
}

/** A star: one hub with `leaves` degree-1 neighbours. */
function starTriples(hub, leaves) {
  return Array.from({ length: leaves }, (_, i) => ({
    subject: hub,
    predicate: 'has',
    object: `leaf:${i}`
  }));
}

const ids = (state) => state.nodes.map((n) => n.id);

describe('mergeSubgraph', () => {
  it('adds each id exactly once when endpoints repeat across edges', () => {
    const state = createGraphState();
    // A chain, so every interior id is an endpoint of two triples.
    const triples = [
      { subject: 'a', predicate: 'p', object: 'b' },
      { subject: 'b', predicate: 'p', object: 'c' },
      { subject: 'c', predicate: 'p', object: 'd' }
    ];
    const incoming = derivedNodes(triples);
    expect(incoming).toHaveLength(6); // b and c each arrive twice

    mergeSubgraph(state, { nodes: incoming, triples }, null);

    expect(ids(state)).toEqual(['a', 'b', 'c', 'd']);
    expect(new Set(ids(state)).size).toBe(state.nodes.length);
  });

  it('keeps ids unique on a hub-and-leaf payload the size of a real load', () => {
    const state = createGraphState();
    // 1 hub + 400 leaves = 401 distinct ids arriving as 800 endpoint entries.
    const triples = starTriples('drawer:hub', 400);
    mergeSubgraph(state, { nodes: derivedNodes(triples), triples }, null);

    expect(state.nodes).toHaveLength(401);
    expect(new Set(ids(state)).size).toBe(401);
    // The keyed `{#each}` indexes by id; nodeIndex must be a bijection onto
    // 0..n-1 or the coordinate lookups in the renderer read the wrong node.
    expect(state.nodeIndex.size).toBe(401);
    expect(new Set(state.nodeIndex.values()).size).toBe(401);
    for (const [id, idx] of state.nodeIndex) expect(state.nodes[idx].id).toBe(id);
  });

  it('preserves pre-existing nodes and their coordinates across a merge', () => {
    const state = createGraphState();
    const seed = [
      { id: 'a', degree: 3 },
      { id: 'b', degree: 1 }
    ];
    mergeSubgraph(state, { nodes: seed, triples: [] }, null);

    const before = state.nodes.map((n) => n);
    const coords = Array.from(state.positions.slice(0, 4));

    // An expansion that re-reports both known ids (twice each, as endpoints
    // would) alongside one genuinely new neighbour.
    const triples = [
      { subject: 'a', predicate: 'p', object: 'b' },
      { subject: 'a', predicate: 'p', object: 'c' }
    ];
    mergeSubgraph(state, { nodes: derivedNodes(triples), triples }, 'a');

    expect(ids(state)).toEqual(['a', 'b', 'c']);
    // Same objects, not replacements — the renderer keys on them.
    expect(state.nodes[0]).toBe(before[0]);
    expect(state.nodes[1]).toBe(before[1]);
    expect(Array.from(state.positions.slice(0, 4))).toEqual(coords);
  });

  it('refreshes degree on a known node without duplicating it', () => {
    const state = createGraphState();
    mergeSubgraph(state, { nodes: [{ id: 'a', degree: 1 }], triples: [] }, null);
    mergeSubgraph(state, { nodes: [{ id: 'a', degree: 9 }], triples: [] }, null);

    expect(state.nodes).toHaveLength(1);
    expect(state.nodes[0].degree).toBe(9);
  });

  it('dedups triples by (subject, predicate, object)', () => {
    const state = createGraphState();
    const t = { subject: 'a', predicate: 'p', object: 'b' };
    const payload = { nodes: derivedNodes([t, t]), triples: [t, { ...t }] };

    const result = mergeSubgraph(state, payload, null);

    expect(state.triples).toHaveLength(1);
    expect(result).toEqual({ addedNodes: 2, addedTriples: 1 });
  });

  it('sizes the coordinate arrays to the DISTINCT node count', () => {
    const state = createGraphState();
    const triples = starTriples('hub', 10); // 11 distinct, 20 entries
    mergeSubgraph(state, { nodes: derivedNodes(triples), triples }, null);

    expect(state.nodes).toHaveLength(11);
    expect(state.positions.length).toBe(22);
    expect(state.pinned.length).toBe(11);
    for (let i = 0; i < state.nodes.length; i++) {
      expect(Number.isFinite(state.positions[2 * i])).toBe(true);
      expect(Number.isFinite(state.positions[2 * i + 1])).toBe(true);
    }
  });

  it('skips entries with no id and tolerates an empty payload', () => {
    const state = createGraphState();
    mergeSubgraph(state, { nodes: [null, {}, { id: '' }, { id: 'a' }], triples: [] }, null);
    mergeSubgraph(state, undefined, null);

    expect(ids(state)).toEqual(['a']);
  });
});
