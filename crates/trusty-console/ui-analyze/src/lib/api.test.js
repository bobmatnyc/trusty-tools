// Regression tests for the five list envelopes the analyzer answers with
// (issue #6155).
//
// Why: `state.svelte.js` assigns every `api.*` result straight into a
// `$state([])` slot and each view iterates it — `Dashboard.svelte` calls
// `hotspots.slice(0, 10)` on the landing view, and `Complexity`, `Smells`,
// `Refactors`, `Clusters` and `Facts` all `{#each}` over theirs. Five of the
// daemon's methods answer an OBJECT wrapping the list, so each one reached its
// view as `slice is not a function` / `not iterable` and the view stalled. Same
// defect and same fix as `unwrapPalaceList` in ui-memory (#7083).
//
// What: drives each wrapper against a stubbed fetch serving the envelope the
// daemon really sends, and asserts an iterable array comes back. The fixtures
// below are transcribed from the handlers, not invented:
//   complexity_hotspots  handlers/analysis.rs  {index_id, top_n, hotspots}
//   smells               handlers/analysis.rs  {index_id, total, offset, limit,
//                                               returned, truncated, chunks}
//   refactor_suggestions handlers/analysis.rs  {index_id, count, min_severity,
//                                               suggestions}
//   clusters             handlers/graph.rs     ClusterResponse{k, method, dim,
//                                               iterations, chunk_count, clusters}
//   facts_list           handlers/facts.rs     {facts, count}
// The five that are already the shape their caller reads — health, indexes,
// quality, upsertFact, deleteFact — are pinned here too, so a later envelope
// added to any of them fails a test rather than a browser.
// Test: this file — `pnpm test`.

import { afterEach, describe, expect, it, vi } from 'vitest';

import { api, smellCategory, unwrapList } from './api.js';

/** One `HotspotItem`: a flattened `CodeChunk` plus its two complexity numbers. */
function hotspot(id, file) {
  return {
    id,
    file,
    start_line: 10,
    end_line: 90,
    content: 'fn big() {}',
    function_name: 'big',
    score: 0.0,
    match_reason: '',
    cyclomatic: 24,
    cognitive: 31
  };
}

/** One `SmellItem`, with the externally-tagged `CodeSmell` values serde emits. */
function smellItem(id, file) {
  return {
    id,
    file,
    start_line: 10,
    end_line: 90,
    function_name: 'big',
    match_reason: '',
    smells: [{ LongFunction: { lines: 80 } }, 'MissingDocstring']
  };
}

/** One `ClusterResponseItem`. */
function cluster(id, label) {
  return { id, label, members: ['a:1:2', 'b:3:4'], cohesion: 0.62, size: 2 };
}

/** One `FactRecord`, only the fields `Facts.svelte` renders. */
function fact(id, subject) {
  return { id, subject, predicate: 'uses', object: 'JWT', provenance: [] };
}

/** Serve one JSON body to every `fetch`, and hand back the URLs called. */
function stubFetch(body) {
  const calls = [];
  vi.stubGlobal('fetch', async (url) => {
    calls.push(String(url));
    return {
      ok: true,
      status: 200,
      statusText: 'OK',
      headers: { get: () => 'application/json' },
      json: async () => body
    };
  });
  return calls;
}

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('the five list envelopes (issue #6155)', () => {
  it('complexityHotspots unwraps {index_id, top_n, hotspots}', async () => {
    const calls = stubFetch({
      index_id: 'trusty-tools',
      top_n: 10,
      hotspots: [hotspot('a:1:2', 'a.rs'), hotspot('b:3:4', 'b.rs')]
    });

    const rows = await api.complexityHotspots('trusty-tools', 10);

    expect(calls[0]).toContain('/indexes/trusty-tools/complexity_hotspots?top_k=10');
    expect(Array.isArray(rows)).toBe(true);
    // Dashboard.svelte's landing view calls exactly this; the envelope threw.
    expect(rows.slice(0, 10).map((h) => h.file)).toEqual(['a.rs', 'b.rs']);
    expect(rows[0].cyclomatic).toBe(24);
  });

  it('smells unwraps the pagination envelope onto its chunks', async () => {
    const calls = stubFetch({
      index_id: 'trusty-tools',
      total: 3214,
      offset: 0,
      limit: 500,
      returned: 2,
      truncated: true,
      chunks: [smellItem('a:1:2', 'a.rs'), smellItem('b:3:4', 'b.rs')]
    });

    const rows = await api.smells('trusty-tools');

    expect(calls[0]).toContain('/indexes/trusty-tools/smells');
    expect(Array.isArray(rows)).toBe(true);
    expect([...rows].map((r) => r.file)).toEqual(['a.rs', 'b.rs']);
  });

  it('smells passes a category filter through as a query parameter', async () => {
    const calls = stubFetch({ chunks: [] });

    await api.smells('trusty-tools', 'LongFunction');

    expect(calls[0]).toContain('smells?category=LongFunction');
  });

  it('refactorSuggestions unwraps {index_id, count, min_severity, suggestions}', async () => {
    const calls = stubFetch({
      index_id: 'trusty-tools',
      count: 2,
      min_severity: 'high',
      suggestions: [
        { severity: 'Critical', refactor_type: 'ExtractMethod', file: 'a.rs' },
        { severity: 'High', refactor_type: 'ReduceNesting', file: 'b.rs' }
      ]
    });

    const rows = await api.refactorSuggestions('trusty-tools', {
      minSeverity: 'high',
      topK: 5
    });

    expect(calls[0]).toContain('min_severity=high');
    expect(calls[0]).toContain('top_k=5');
    expect(Array.isArray(rows)).toBe(true);
    expect([...rows].map((r) => r.refactor_type)).toEqual(['ExtractMethod', 'ReduceNesting']);
  });

  it('clusters unwraps ClusterResponse onto its clusters', async () => {
    const calls = stubFetch({
      k: 8,
      method: 'bow',
      dim: 256,
      iterations: 17,
      chunk_count: 4210,
      clusters: [cluster(0, 'service layer'), cluster(1, 'tests')]
    });

    const rows = await api.clusters('trusty-tools', { k: 8, method: 'bow' });

    expect(calls[0]).toContain('/clusters?k=8&method=bow');
    expect(Array.isArray(rows)).toBe(true);
    expect([...rows].map((c) => c.label)).toEqual(['service layer', 'tests']);
    // Clusters.svelte renders `c.chunk_ids?.length ?? c.size` as the chunk count.
    expect(rows[0].size).toBe(2);
  });

  it('listFacts unwraps {facts, count}', async () => {
    const calls = stubFetch({ facts: [fact(1, 'fn auth'), fact(2, 'fn login')], count: 2 });

    const rows = await api.listFacts('fn', 'uses');

    expect(calls[0]).toContain('subject=fn');
    expect(calls[0]).toContain('predicate=uses');
    expect(Array.isArray(rows)).toBe(true);
    expect([...rows].map((f) => f.subject)).toEqual(['fn auth', 'fn login']);
  });
});

describe('unwrapList', () => {
  it('passes a bare array through, for a daemon predating the envelope', () => {
    expect(unwrapList([{ id: 'a' }], 'hotspots')).toEqual([{ id: 'a' }]);
  });

  it('answers an empty array for a missing, malformed or empty body', () => {
    expect(unwrapList(null, 'chunks')).toEqual([]);
    expect(unwrapList({}, 'chunks')).toEqual([]);
    expect(unwrapList('nonsense', 'chunks')).toEqual([]);
    // The key present but not a list — never a throw at the call site.
    expect(unwrapList({ chunks: 12 }, 'chunks')).toEqual([]);
  });

  it('reads only the named key, so one envelope cannot answer for another', () => {
    expect(unwrapList({ hotspots: [1], chunks: [2, 3] }, 'chunks')).toEqual([2, 3]);
  });
});

describe('smellCategory — serde external tagging (issue #6155)', () => {
  it('names a variant that carries fields by its one key', () => {
    expect(smellCategory({ LongFunction: { lines: 80 } })).toBe('LongFunction');
    expect(smellCategory({ TooManyParams: { count: 9 } })).toBe('TooManyParams');
  });

  it('names a unit variant, which serde sends as a bare string', () => {
    expect(smellCategory('MissingDocstring')).toBe('MissingDocstring');
  });

  it('prefers an explicit category or name when one is present', () => {
    expect(smellCategory({ category: 'long_function' })).toBe('long_function');
    expect(smellCategory({ name: 'deep_nesting' })).toBe('deep_nesting');
  });

  it('answers unknown rather than throwing on a shape it cannot read', () => {
    expect(smellCategory(null)).toBe('unknown');
    expect(smellCategory({})).toBe('unknown');
    expect(smellCategory(42)).toBe('unknown');
  });
});

describe('the five wrappers that must NOT unwrap', () => {
  it('health returns the flat HealthResponse object', async () => {
    stubFetch({ status: 'ok', version: '0.5.1', search_reachable: true });

    const health = await api.health();

    // Dashboard.svelte reads both fields off the object directly.
    expect(health.status).toBe('ok');
    expect(health.search_reachable).toBe(true);
  });

  it('indexes returns the bare IndexSummary array the daemon sends', async () => {
    stubFetch([{ id: 'trusty-tools', root_path: null }, { id: 'izzie', root_path: null }]);

    const indexes = await api.indexes();

    // refreshIndexes() maps over this to build the picker.
    expect(indexes.map((i) => i.id)).toEqual(['trusty-tools', 'izzie']);
  });

  it('quality returns the flat QualityReport object', async () => {
    stubFetch({
      avg_cyclomatic: 4.25,
      pct_grade_a: 0.61,
      smell_count: 3214,
      chunk_count: 41230
    });

    const quality = await api.quality('trusty-tools');

    expect(quality.smell_count).toBe(3214);
    expect(quality.avg_cyclomatic).toBeCloseTo(4.25);
    // `grade` is NOT a QualityReport field — the daemon has never sent one, so
    // the Dashboard's letter card reads `?`. Pinned so a daemon that starts
    // sending it is a test change, not a surprise.
    expect(quality.grade).toBeUndefined();
  });

  it('upsertFact posts the fact and returns the daemon ack', async () => {
    const calls = stubFetch({ id: 7, upserted: true });

    const ack = await api.upsertFact({ subject: 's', predicate: 'p', object: 'o' });

    expect(calls[0]).toContain('/facts');
    expect(ack.upserted).toBe(true);
  });

  it('deleteFact targets the fact id and returns the daemon ack', async () => {
    const calls = stubFetch({ id: 7, removed: true });

    const ack = await api.deleteFact(7);

    expect(calls[0]).toContain('/facts/7');
    expect(ack.removed).toBe(true);
  });
});
