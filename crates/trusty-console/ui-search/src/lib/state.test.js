// Roster fetch tests for `refreshIndexes` (#6699).
//
// Why: the roster used to call `GET /indexes` for the ids and then
// `GET /indexes/{id}/status` once per id — 42 requests and 41 per-index
// directory walks to draw one table on a 41-index daemon — and the rows still
// carried no vector-lane health, so a zero-vector index rendered green. These
// pin the replacement: one `?details=true` call, the same column values, and
// the lane-health keys passed through untouched so `indexHealth` reads a row
// exactly as it reads a status body.
// What: stubs the `api` module and asserts which endpoints were called and what
// `getIndexes()` holds afterwards.
// Test: this file — `pnpm test` from crates/trusty-console/ui-search.

import { beforeEach, describe, expect, it, vi } from 'vitest';

const listIndexesDetailed = vi.fn();
const listIndexes = vi.fn();
const indexStatus = vi.fn();
const health = vi.fn(() => Promise.resolve({}));

vi.mock('./api.js', () => ({
  api: {
    listIndexesDetailed: (...a) => listIndexesDetailed(...a),
    listIndexes: (...a) => listIndexes(...a),
    indexStatus: (...a) => indexStatus(...a),
    health: () => health()
  }
}));

/** One row exactly as `GET /indexes?details=true` serves it. */
const ZERO_VECTOR_ROW = {
  id: 'tm-trusty-tools-19',
  root_path: '/Users/me/code/trusty-tools',
  size_bytes: 4102000,
  last_indexed: '2026-09-04T12:00:00Z',
  chunk_count: 58415,
  stages: {
    lexical: { status: 'ready' },
    semantic: { status: 'ready', embedded: 0, total: 0, paused: false },
    graph: { status: 'ready' }
  },
  search_capabilities: ['bm25', 'literal', 'exact_match', 'vector', 'kg'],
  semantic_coverage: {
    vectors_present: 0,
    vectors_unavailable_reason: null,
    chunk_count: 58415,
    embedded_this_boot: 0
  },
  lexical_only: false,
  skip_vector: false
};

async function loadState() {
  vi.resetModules();
  return import('./state.svelte.js');
}

beforeEach(() => {
  listIndexesDetailed.mockReset();
  listIndexes.mockReset();
  indexStatus.mockReset();
  health.mockReset();
  health.mockResolvedValue({});
  delete window.__SEARCH_BASE__;
  delete window.__OPENROUTER_ENABLED__;
});

describe('refreshIndexes (#6699)', () => {
  it('draws the whole roster from one request, with no per-index status fan-out', async () => {
    listIndexesDetailed.mockResolvedValue({ indexes: [ZERO_VECTOR_ROW, { ...ZERO_VECTOR_ROW, id: 'b' }] });
    const { refreshIndexes, getIndexes } = await loadState();

    const rows = await refreshIndexes();

    expect(listIndexesDetailed).toHaveBeenCalledTimes(1);
    expect(indexStatus).not.toHaveBeenCalled();
    expect(listIndexes).not.toHaveBeenCalled();
    expect(rows).toHaveLength(2);
    expect(getIndexes()).toHaveLength(2);
  });

  it('keeps the column values the table already reads, including the size_bytes rename', async () => {
    listIndexesDetailed.mockResolvedValue({ indexes: [ZERO_VECTOR_ROW] });
    const { refreshIndexes } = await loadState();

    const [row] = await refreshIndexes();

    expect(row).toMatchObject({
      id: 'tm-trusty-tools-19',
      chunk_count: 58415,
      root_path: '/Users/me/code/trusty-tools',
      // The wire calls it `size_bytes`; the table's column reads `disk_bytes`.
      disk_bytes: 4102000,
      last_indexed: '2026-09-04T12:00:00Z',
      error: false
    });
  });

  it('passes the lane-health keys through, which is what lets the row be flagged', async () => {
    listIndexesDetailed.mockResolvedValue({ indexes: [ZERO_VECTOR_ROW] });
    const { refreshIndexes } = await loadState();
    const { indexHealth } = await import('./indexingPipeline.js');

    const [row] = await refreshIndexes();

    expect(row.semantic_coverage.vectors_present).toBe(0);
    expect(row.search_capabilities).toContain('vector');
    expect(indexHealth(row).healthy).toBe(false);
  });

  it('reports a failed call as an error and empties the list rather than half-filling it', async () => {
    listIndexesDetailed.mockRejectedValue(new Error('daemon unreachable'));
    const { refreshIndexes, getIndexes, getError } = await loadState();

    await refreshIndexes();

    expect(getIndexes()).toEqual([]);
    expect(getError()).toBe('daemon unreachable');
  });
});

// #9030: chat availability follows the daemon's own `chat_available`.
describe('getChatAvailable (#9030)', () => {
  const CONSOLE_BASE = '/api/search/';

  it('is true on a console-served page when /health says chat_available: true', async () => {
    window.__SEARCH_BASE__ = CONSOLE_BASE;
    health.mockResolvedValue({ status: 'ok', chat_available: true });
    const { refreshHealth, getChatAvailable } = await loadState();
    await refreshHealth();
    expect(getChatAvailable()).toBe(true);
  });

  it.each([
    ['false', { status: 'ok', chat_available: false }],
    ['missing', { status: 'ok' }],
    ['non-boolean', { status: 'ok', chat_available: 'true' }]
  ])('is false on a console-served page when chat_available is %s', async (_n, body) => {
    window.__SEARCH_BASE__ = CONSOLE_BASE;
    window.__OPENROUTER_ENABLED__ = true; // never trusted through the console
    health.mockResolvedValue(body);
    const { refreshHealth, getChatAvailable } = await loadState();
    await refreshHealth();
    expect(getChatAvailable()).toBe(false);
  });

  it('is false on a console-served page before any health payload', async () => {
    window.__SEARCH_BASE__ = CONSOLE_BASE;
    const { getChatAvailable } = await loadState();
    expect(getChatAvailable()).toBe(false);
  });

  it('daemon-served: chat_available from /health wins over the boot global', async () => {
    window.__OPENROUTER_ENABLED__ = true;
    health.mockResolvedValue({ status: 'ok', chat_available: false });
    const { refreshHealth, getChatAvailable } = await loadState();
    expect(getChatAvailable()).toBe(true); // boot global until /health lands
    await refreshHealth();
    expect(getChatAvailable()).toBe(false);
  });

  it('keeps chat_available when a status_changed SSE frame replaces the health body', async () => {
    window.__SEARCH_BASE__ = CONSOLE_BASE;
    health.mockResolvedValue({ status: 'ok', chat_available: true });
    let source;
    vi.stubGlobal(
      'EventSource',
      class {
        constructor() {
          source = this;
        }
        close() {}
      }
    );
    const { refreshHealth, subscribeStatusStream, getChatAvailable } = await loadState();
    await refreshHealth();
    subscribeStatusStream();
    source.onmessage({
      data: JSON.stringify({ type: 'status_changed', indexes: 1, total_chunks: 2, uptime_secs: 3, version: '1' })
    });
    expect(getChatAvailable()).toBe(true);
    vi.unstubAllGlobals();
  });
});
