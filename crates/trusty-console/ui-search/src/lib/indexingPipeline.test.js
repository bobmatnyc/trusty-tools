/**
 * The indexing-pipeline mappings (#6524).
 *
 * Why: these functions decide what colour an operator sees against a stage, and
 * the one that matters most is invisible from the status string alone — a paused
 * embedding stage is `in_progress` on the wire. A regression there shows a
 * spinner against a stopped stage, which is exactly the confusion the row was
 * added to remove.
 * Test: `pnpm test` from `crates/trusty-console/ui-search`.
 */

import { describe, it, expect } from 'vitest';
import {
  STAGES,
  stageBadge,
  stageMeta,
  isEmbeddingPaused,
  eventKindTone,
  relativeTime,
  fileEventRow,
  pushFeedRow,
  FEED_LIMIT,
  vectorCoverageFault,
  laneBadge,
  indexHealth,
  coverageMeta
} from './indexingPipeline.js';

describe('stageBadge', () => {
  it('maps every StageStatus the daemon can send', () => {
    expect(stageBadge({ status: 'ready' })).toMatchObject({
      tone: 'success',
      spinner: false
    });
    expect(stageBadge({ status: 'in_progress' })).toMatchObject({
      tone: 'info',
      spinner: true
    });
    expect(stageBadge({ status: 'failed' })).toMatchObject({ tone: 'danger' });
    expect(stageBadge({ status: 'skipped' })).toMatchObject({ tone: 'muted' });
    expect(stageBadge({ status: 'pending' })).toMatchObject({ tone: '' });
  });

  it('shows PAUSED over the spinner, because a paused stage is still in_progress', () => {
    const badge = stageBadge({ status: 'in_progress', paused: true });
    expect(badge.label).toBe('PAUSED');
    expect(badge.tone).toBe('warning');
    expect(badge.spinner).toBe(false);
  });

  it('takes only a literal true as paused, so a missing field is not a pause', () => {
    expect(stageBadge({ status: 'in_progress', paused: false }).label).toBe('Working');
    expect(stageBadge({ status: 'in_progress' }).label).toBe('Working');
  });

  it('reports an absent or unrecognised stage as unknown rather than guessing', () => {
    expect(stageBadge(undefined)).toMatchObject({ tone: 'muted', label: 'unknown' });
    expect(stageBadge(null)).toMatchObject({ tone: 'muted', label: 'unknown' });
    expect(stageBadge({ status: 'quantum' })).toMatchObject({ label: 'unknown' });
  });

  it('names the three lanes in pipeline order', () => {
    expect(STAGES.map((s) => s.key)).toEqual(['lexical', 'semantic', 'graph']);
  });
});

describe('stageMeta', () => {
  it('renders only the counters the stage actually carries', () => {
    expect(stageMeta({ status: 'ready', files: 12, chunks: 400 })).toBe(
      '12 files · 400 chunks'
    );
    expect(stageMeta({ status: 'in_progress', embedded: 30, total: 400 })).toBe(
      '30/400 embedded'
    );
    expect(stageMeta({ status: 'pending' })).toBe('');
    expect(stageMeta(null)).toBe('');
  });

  it('reports a total with no embedded count as work still owed', () => {
    expect(stageMeta({ total: 400 })).toBe('400 to embed');
  });

  it('does not confuse a zero counter with an absent one', () => {
    expect(stageMeta({ embedded: 0, total: 400 })).toBe('0/400 embedded');
    expect(stageMeta({ files: 0 })).toBe('0 files');
  });
});

describe('isEmbeddingPaused', () => {
  it('reads the flag off the semantic stage and nowhere else', () => {
    expect(isEmbeddingPaused({ stages: { semantic: { paused: true } } })).toBe(true);
    expect(isEmbeddingPaused({ stages: { semantic: { paused: false } } })).toBe(false);
    expect(isEmbeddingPaused({ stages: { lexical: { paused: true } } })).toBe(false);
  });

  it('is false, never undefined, for a status that has not arrived', () => {
    expect(isEmbeddingPaused(null)).toBe(false);
    expect(isEmbeddingPaused({})).toBe(false);
    expect(isEmbeddingPaused({ stages: {} })).toBe(false);
  });
});

describe('eventKindTone', () => {
  it('gives each watcher event kind its own tone', () => {
    expect(eventKindTone('modified')).toBe('info');
    expect(eventKindTone('removed')).toBe('danger');
    expect(eventKindTone('rescan')).toBe('warning');
    expect(eventKindTone('something-new')).toBe('muted');
  });
});

describe('relativeTime', () => {
  const now = 1_700_000_000_000;

  it('scales from seconds to days', () => {
    expect(relativeTime(now - 5_000, now)).toBe('just now');
    expect(relativeTime(now - 5 * 60_000, now)).toBe('5m ago');
    expect(relativeTime(now - 3 * 3_600_000, now)).toBe('3h ago');
    expect(relativeTime(now - 2 * 86_400_000, now)).toBe('2d ago');
  });

  it('reads clock skew as just now rather than as a negative age', () => {
    expect(relativeTime(now + 30_000, now)).toBe('just now');
  });

  it('renders a missing timestamp as a dash', () => {
    expect(relativeTime(undefined, now)).toBe('—');
    expect(relativeTime(Number.NaN, now)).toBe('—');
  });
});

describe('fileEventRow', () => {
  const now = 1_700_000_000_000;

  it('maps a file change to its path, tone and age', () => {
    const row = fileEventRow(
      { path: 'src/a.rs', kind: 'modified', at_unix_ms: now - 60_000 },
      now
    );
    expect(row).toMatchObject({
      path: 'src/a.rs',
      kind: 'modified',
      tone: 'info',
      when: '1m ago'
    });
  });

  it('spells out a rescan instead of rendering the daemon dot', () => {
    const row = fileEventRow({ path: '.', kind: 'rescan', at_unix_ms: now }, now);
    expect(row.path).toBe('the whole tree was rescanned');
    expect(row.tone).toBe('warning');
  });

  it('renders a lag notice as its own row, not as a change with no path', () => {
    const row = fileEventRow({ type: 'lag', skipped: 4 }, now);
    expect(row.kind).toBe('lag');
    expect(row.tone).toBe('warning');
    expect(row.path).toContain('4 changes not shown');
  });

  it('renders a broken feed as an error row carrying the daemon message', () => {
    const row = fileEventRow({ type: 'error', message: 'the feed died' }, now);
    expect(row).toMatchObject({ kind: 'error', tone: 'danger', path: 'the feed died' });
  });
});

describe('pushFeedRow', () => {
  it('adds newest first and returns a new array', () => {
    const first = pushFeedRow([], { path: 'a' });
    const second = pushFeedRow(first, { path: 'b' });
    expect(second.map((r) => r.path)).toEqual(['b', 'a']);
    expect(first).toHaveLength(1);
  });

  it('never grows past the daemon ring size', () => {
    let rows = [];
    for (let i = 0; i < FEED_LIMIT + 50; i += 1) {
      rows = pushFeedRow(rows, { path: `f${i}` });
    }
    expect(rows).toHaveLength(FEED_LIMIT);
    expect(rows[0].path).toBe(`f${FEED_LIMIT + 49}`);
  });
});

// ---------------------------------------------------------------------------
// Lane health (#6689)
// ---------------------------------------------------------------------------

/**
 * The exact shape `tm-trusty-tools-19` serves live: 58,415 chunks, a semantic
 * stage reporting `ready`, `vector` advertised, and an empty vector store. Every
 * assertion about the flag is written against this rather than a hand-tuned
 * minimal object, so a change that stops recognising the real payload fails.
 */
const EMPTY_VECTOR_STORE = {
  index_id: 'tm-trusty-tools-19',
  chunk_count: 58415,
  status: 'ready',
  stages: {
    lexical: { status: 'ready', files: 4102, chunks: 58415 },
    semantic: { status: 'ready', embedded: 0, total: 0, paused: false },
    graph: { status: 'ready' }
  },
  semantic_coverage: {
    vectors_present: 0,
    vectors_unavailable_reason: null,
    chunk_count: 58415,
    embedded_this_boot: 0
  },
  search_capabilities: ['bm25', 'literal', 'exact_match', 'vector', 'kg'],
  lexical_only: false,
  skip_kg: false,
  skip_vector: false
};

/** The same corpus, healthy: a vector for every chunk. */
const HEALTHY = {
  ...EMPTY_VECTOR_STORE,
  index_id: 'tm-trusty-tools-20',
  semantic_coverage: { ...EMPTY_VECTOR_STORE.semantic_coverage, vectors_present: 58415 }
};

describe('vectorCoverageFault', () => {
  it('flags a ready, vector-advertising index whose store holds zero vectors', () => {
    // The #6689 acceptance case. A status-only implementation returns null here.
    const fault = vectorCoverageFault(EMPTY_VECTOR_STORE);
    expect(fault).not.toBeNull();
    expect(fault.code).toBe('empty_vector_store');
    expect(fault.detail).toContain('58,415');
  });

  it('needs all three signals, so removing any one clears the flag', () => {
    // Zero vectors is not enough on its own — these three each make it healthy.
    expect(
      vectorCoverageFault({ ...EMPTY_VECTOR_STORE, search_capabilities: ['bm25', 'literal'] })
    ).toBeNull();
    expect(
      vectorCoverageFault({
        ...EMPTY_VECTOR_STORE,
        chunk_count: 0,
        semantic_coverage: { ...EMPTY_VECTOR_STORE.semantic_coverage, chunk_count: 0 }
      })
    ).toBeNull();
    expect(vectorCoverageFault(HEALTHY)).toBeNull();
  });

  it('leaves an index that was never meant to embed alone', () => {
    expect(vectorCoverageFault({ ...EMPTY_VECTOR_STORE, skip_vector: true })).toBeNull();
    expect(vectorCoverageFault({ ...EMPTY_VECTOR_STORE, lexical_only: true })).toBeNull();
    expect(
      vectorCoverageFault({
        ...EMPTY_VECTOR_STORE,
        stages: { ...EMPTY_VECTOR_STORE.stages, semantic: { status: 'skipped' } }
      })
    ).toBeNull();
  });

  it('does not flag a lane still embedding, which has not earned the capability yet', () => {
    expect(
      vectorCoverageFault({
        ...EMPTY_VECTOR_STORE,
        stages: {
          ...EMPTY_VECTOR_STORE.stages,
          semantic: { status: 'in_progress', embedded: 900, total: 58415 }
        },
        search_capabilities: ['bm25', 'literal', 'exact_match']
      })
    ).toBeNull();
  });

  it('separates a BM25-only null from a store whose count would not read', () => {
    const nulled = (reason) => ({
      ...EMPTY_VECTOR_STORE,
      semantic_coverage: {
        ...EMPTY_VECTOR_STORE.semantic_coverage,
        vectors_present: null,
        vectors_unavailable_reason: reason
      }
    });
    expect(vectorCoverageFault(nulled('no_vector_store'))).toBeNull();
    expect(vectorCoverageFault(nulled('count_unreadable')).code).toBe('count_unreadable');
  });

  it('treats a daemon too old to send coverage as no evidence, not as a fault', () => {
    const { semantic_coverage, ...withoutCoverage } = EMPTY_VECTOR_STORE;
    expect(semantic_coverage).toBeDefined();
    expect(vectorCoverageFault(withoutCoverage)).toBeNull();
    expect(vectorCoverageFault(null)).toBeNull();
    expect(vectorCoverageFault(undefined)).toBeNull();
  });
});

describe('laneBadge', () => {
  it('turns the semantic badge red for the empty store the stage calls ready', () => {
    // stageBadge on the same lane says success — that divergence IS the fix.
    expect(stageBadge(EMPTY_VECTOR_STORE.stages.semantic).tone).toBe('success');
    const badge = laneBadge('semantic', EMPTY_VECTOR_STORE);
    expect(badge.tone).toBe('danger');
    expect(badge.label).toBe('Empty');
    expect(badge.detail).toContain('0 vectors');
  });

  it('leaves the healthy index and the other two lanes on their stage status', () => {
    expect(laneBadge('semantic', HEALTHY)).toMatchObject({ tone: 'success', label: 'Ready' });
    expect(laneBadge('lexical', EMPTY_VECTOR_STORE)).toMatchObject({ tone: 'success' });
    expect(laneBadge('graph', EMPTY_VECTOR_STORE)).toMatchObject({ tone: 'success' });
  });

  it('still reports a paused lane as paused, because a pause withdraws the capability', () => {
    const paused = {
      ...EMPTY_VECTOR_STORE,
      stages: {
        ...EMPTY_VECTOR_STORE.stages,
        semantic: { status: 'in_progress', paused: true }
      },
      search_capabilities: ['bm25', 'literal', 'exact_match']
    };
    expect(laneBadge('semantic', paused).label).toBe('PAUSED');
  });
});

describe('indexHealth', () => {
  it('calls the zero-vector index degraded and names the lane at fault', () => {
    const health = indexHealth(EMPTY_VECTOR_STORE);
    expect(health.healthy).toBe(false);
    expect(health.label).toBe('Degraded');
    expect(health.faults).toHaveLength(1);
    expect(health.faults[0].lane).toBe('semantic');
  });

  it('calls the healthy index healthy', () => {
    expect(indexHealth(HEALTHY)).toMatchObject({ healthy: true, label: 'Healthy', faults: [] });
  });

  it('reports a failed lane with the daemon own failure string', () => {
    const health = indexHealth({
      ...HEALTHY,
      stages: {
        ...HEALTHY.stages,
        graph: { status: 'failed', failure: 'kg store open timed out' }
      }
    });
    expect(health.healthy).toBe(false);
    expect(health.faults[0]).toMatchObject({ lane: 'graph', detail: 'kg store open timed out' });
  });

  it('says a failed semantic lane once, not twice', () => {
    const health = indexHealth({
      ...EMPTY_VECTOR_STORE,
      stages: {
        ...EMPTY_VECTOR_STORE.stages,
        semantic: { status: 'failed', failure: 'embedder unreachable' }
      }
    });
    expect(health.faults.filter((f) => f.lane === 'semantic')).toHaveLength(1);
  });

  it('is unknown before a status arrives, never green', () => {
    expect(indexHealth(null)).toMatchObject({ healthy: null, label: 'Unknown' });
  });
});

describe('coverageMeta', () => {
  it('shows the cumulative pair, which is not the per-boot embedded count', () => {
    expect(coverageMeta(EMPTY_VECTOR_STORE)).toBe('0 / 58,415 vectors stored');
    expect(coverageMeta(HEALTHY)).toBe('58,415 / 58,415 vectors stored');
  });

  it('names the reason when there is no number to show', () => {
    expect(
      coverageMeta({
        semantic_coverage: { vectors_present: null, vectors_unavailable_reason: 'no_vector_store' }
      })
    ).toBe('vectors: no_vector_store');
    expect(coverageMeta({})).toBe('');
  });
});
