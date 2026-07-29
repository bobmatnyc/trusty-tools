// Costs tab API client + pure presentation helpers (#4098, COST-09).
//
// Why: `CostsView.svelte` must distinguish four backend states — costs,
// nothing recorded, nothing dispatched, and a fault — and it must never render
// a confident `$0.00` over a missing usage log. Keeping the fetch + the state
// discrimination here (rather than inline in the component) makes both
// unit-testable without mounting Svelte, and keeps the component to rendering.
//
// Note the deliberate deviation from the sibling clients (`workstreams.ts`,
// `agentConfig.ts`): those swallow failures and return `[]`, which is right for
// a convenience sidebar. It is WRONG here. An empty array is
// indistinguishable from "$0 spent", which is the single claim this tab must
// never make falsely, so `fetchCosts` returns a discriminated result and lets
// the view say which failure it hit.
//
// What: `CostRow`/`CostSummary` mirror `usage::aggregate`'s serialized shape;
// `fetchCosts` wraps `GET /api/costs`; `formatUsd`/`formatCount`/`barPercent`
// are pure formatters the view and its tests share.
// Test: `costs.test.ts`.

import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';

/** One aggregate bucket. `key` is an agent name, model id, or `YYYY-MM-DD`. */
export interface CostRow {
  key: string;
  input_tokens: number;
  output_tokens: number;
  cost_usd: number;
  dispatch_count: number;
  duration_ms: number;
}

/** The `GET /api/costs` payload. Mirrors `usage::aggregate::CostSummary`. */
export interface CostSummary {
  available: boolean;
  source: string;
  records: number;
  malformed_lines: number;
  first_ts: string | null;
  last_ts: string | null;
  window_days: number | null;
  totals: CostRow;
  by_agent: CostRow[];
  by_model: CostRow[];
  by_date: CostRow[];
  /** Present only when `available` is false — why there is nothing to show. */
  reason?: string;
}

/**
 * The four states the view renders, kept as a discriminated union so no branch
 * can be silently skipped:
 *  - `ok`        — real aggregated costs (possibly zero rows, but a real log).
 *  - `no-data`   — no usage log exists yet. NOT `$0.00`.
 *  - `error`     — the log exists and could not be read, or the request failed.
 */
export type CostsState =
  | { kind: 'ok'; summary: CostSummary }
  | { kind: 'no-data'; reason: string; source: string }
  | { kind: 'error'; message: string };

function authHeaders(): Record<string, string> {
  const token = getCurrentApiToken();
  return token ? { Authorization: `Bearer ${token}` } : {};
}

/**
 * `GET /api/costs`, mapped to a [`CostsState`].
 *
 * Why: See the module note — this deliberately does NOT degrade a failure into
 * an empty dataset. A cost figure the operator cannot trust is worse than no
 * figure, so every non-success path keeps its identity all the way to the view.
 * What: Issues the request with the optional `?days=` window, maps a non-OK
 * status or a thrown network error to `error`, and an `available: false` body
 * to `no-data` carrying the server's own reason string.
 * Test: `fetchCosts` cases in `costs.test.ts`.
 */
export async function fetchCosts(days?: number): Promise<CostsState> {
  const query = days && days > 0 ? `?days=${days}` : '';
  try {
    const r = await fetch(`${apiBase()}/api/costs${query}`, { headers: authHeaders() });
    const body = (await r.json().catch(() => null)) as (CostSummary & { error?: string }) | null;
    if (!r.ok) {
      return { kind: 'error', message: body?.error ?? `GET /api/costs failed: ${r.status}` };
    }
    if (!body) return { kind: 'error', message: 'GET /api/costs returned an unreadable body' };
    if (body.available === false) {
      return {
        kind: 'no-data',
        reason: body.reason ?? 'no usage has been recorded for this project yet',
        source: body.source ?? '',
      };
    }
    return { kind: 'ok', summary: body };
  } catch (e) {
    return { kind: 'error', message: e instanceof Error ? e.message : String(e) };
  }
}

/**
 * Format a USD amount at a precision that does not imply false certainty.
 *
 * Why: Sub-cent dispatch costs round to `$0.00` at two decimals, which reads as
 * "free" for a column of real spend. The statusline already solved this the
 * same way (`repl::tui::status::format_cost_value`), so the two surfaces agree.
 * What: 4 decimals below $0.01, 2 decimals at or above.
 * Test: `formatUsd` cases in `costs.test.ts`.
 */
export function formatUsd(cost: number): string {
  if (!Number.isFinite(cost)) return '—';
  return cost < 0.01 ? `$${cost.toFixed(4)}` : `$${cost.toFixed(2)}`;
}

/** Compact integer with a `k`/`M` suffix — token counts get long fast. */
export function formatCount(n: number): string {
  if (!Number.isFinite(n)) return '—';
  if (n >= 1_000_000) return `${(n / 1_000_000).toFixed(1)}M`;
  if (n >= 1_000) return `${(n / 1_000).toFixed(1)}k`;
  return `${n}`;
}

/**
 * Bar width as a percentage of the largest row in the same breakdown.
 *
 * Why: The bars are relative magnitude, not absolute scale, so the chart stays
 * readable whether the day's spend is cents or hundreds of dollars.
 * What: `cost / max * 100`, clamped to [0, 100]; `0` when `max` is not a
 * positive finite number (an all-zero breakdown draws no bars rather than
 * dividing by zero into `NaN%`).
 * Test: `barPercent` cases in `costs.test.ts`.
 */
export function barPercent(cost: number, max: number): number {
  if (!Number.isFinite(cost) || !Number.isFinite(max) || max <= 0) return 0;
  return Math.max(0, Math.min(100, (cost / max) * 100));
}
