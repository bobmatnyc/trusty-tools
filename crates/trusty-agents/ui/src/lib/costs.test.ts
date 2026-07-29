// Costs API client + formatter tests (#4098, COST-09).
//
// Why: `fetchCosts` deliberately breaks the sibling clients' return-`[]`-on-
// failure convention, because an empty dataset is indistinguishable from "$0
// spent" — the one claim this tab must never make falsely. That divergence is
// only worth anything if each failure keeps its identity, so these tests pin
// the mapping from every backend answer to a `CostsState` variant.
// What: `fetchCosts` against a stubbed `fetch`, plus the pure formatters.
// Test: this file.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { barPercent, fetchCosts, formatCount, formatUsd } from './costs';

function stubFetch(status: number, body: unknown) {
  vi.stubGlobal(
    'fetch',
    vi.fn(async () => ({
      ok: status >= 200 && status < 300,
      status,
      json: async () => body,
    })),
  );
}

const SUMMARY = {
  available: true,
  source: '/p/.trusty-agents/state/usage.jsonl',
  records: 2,
  malformed_lines: 0,
  first_ts: '2026-07-27T10:00:00Z',
  last_ts: '2026-07-28T10:00:00Z',
  window_days: null,
  totals: {
    key: 'total',
    input_tokens: 2_000_000,
    output_tokens: 0,
    cost_usd: 3.8,
    dispatch_count: 2,
    duration_ms: 20,
  },
  by_agent: [],
  by_model: [],
  by_date: [],
};

afterEach(() => {
  vi.unstubAllGlobals();
});

describe('fetchCosts', () => {
  it('maps a populated payload to ok', async () => {
    stubFetch(200, SUMMARY);
    const state = await fetchCosts();
    expect(state.kind).toBe('ok');
    if (state.kind === 'ok') expect(state.summary.totals.cost_usd).toBe(3.8);
  });

  it('maps available:false to no-data, never to an empty ok', async () => {
    stubFetch(200, {
      available: false,
      reason: 'no usage has been recorded for this project yet',
      source: '/p/.trusty-agents/state/usage.jsonl',
    });
    const state = await fetchCosts();
    expect(state.kind).toBe('no-data');
    if (state.kind === 'no-data') {
      expect(state.reason).toContain('no usage has been recorded');
      expect(state.source).toContain('usage.jsonl');
    }
  });

  it('maps a 500 to error carrying the server message', async () => {
    stubFetch(500, { available: false, error: 'could not read usage log /p/usage.jsonl' });
    const state = await fetchCosts();
    expect(state.kind).toBe('error');
    if (state.kind === 'error') expect(state.message).toContain('could not read usage log');
  });

  it('maps a thrown network error to error, not to empty data', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('connection refused');
      }),
    );
    const state = await fetchCosts();
    expect(state.kind).toBe('error');
    if (state.kind === 'error') expect(state.message).toBe('connection refused');
  });

  it('sends ?days= only for a positive window', async () => {
    stubFetch(200, SUMMARY);
    await fetchCosts(7);
    await fetchCosts(0);
    await fetchCosts();
    const urls = (globalThis.fetch as ReturnType<typeof vi.fn>).mock.calls.map((c) => String(c[0]));
    expect(urls[0]).toContain('?days=7');
    expect(urls[1]).not.toContain('days=');
    expect(urls[2]).not.toContain('days=');
  });
});

describe('formatUsd', () => {
  it('keeps sub-cent amounts visible instead of rounding them to $0.00', () => {
    expect(formatUsd(0.0004)).toBe('$0.0004');
    expect(formatUsd(0.009)).toBe('$0.0090');
  });

  it('uses two decimals at or above a cent', () => {
    expect(formatUsd(0.01)).toBe('$0.01');
    expect(formatUsd(3.8)).toBe('$3.80');
    expect(formatUsd(1234.5)).toBe('$1234.50');
  });

  it('renders a non-finite cost as a dash rather than NaN', () => {
    expect(formatUsd(Number.NaN)).toBe('—');
    expect(formatUsd(Number.POSITIVE_INFINITY)).toBe('—');
  });
});

describe('formatCount', () => {
  it('compacts thousands and millions', () => {
    expect(formatCount(999)).toBe('999');
    expect(formatCount(1500)).toBe('1.5k');
    expect(formatCount(2_400_000)).toBe('2.4M');
  });
});

describe('barPercent', () => {
  it('scales relative to the largest row', () => {
    expect(barPercent(5, 10)).toBe(50);
    expect(barPercent(10, 10)).toBe(100);
  });

  it('returns 0 rather than NaN for an all-zero breakdown', () => {
    expect(barPercent(0, 0)).toBe(0);
    expect(barPercent(1, Number.NaN)).toBe(0);
  });

  it('clamps out-of-range values', () => {
    expect(barPercent(20, 10)).toBe(100);
    expect(barPercent(-5, 10)).toBe(0);
  });
});
