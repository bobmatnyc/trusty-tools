/**
 * The dashboard's client to the CONSOLE's own routes, as distinct from the
 * memory bridge (#6928).
 *
 * Why: `api.js` speaks to trusty-memory through `/api/memory/…`, which the
 * console translates into one `memory.*` JSON-RPC call per path. Two things
 * this dashboard now needs are not on that surface at all. The hero row's
 * seven figures come from `GET /api/console/metrics/memory` — `memory.status`
 * has no room count and, since #6372, counts only cache-resident palaces, so a
 * host with 94 palaces and 2 resident would report two palaces' worth of
 * drawers in the row. Compact and delete are `tools/call` envelopes the
 * console already owns as routes (#6360, #6371), not folded `memory.*`
 * methods.
 *
 * What: every path is resolved RELATIVE to `apiBase()` rather than against the
 * origin, so `../console/…` lands on the console's routes under any mount —
 * including a proxy sub-path, which an origin-absolute `/api/console/…` would
 * break.
 * Test: `palaceActions.test.js` covers the path shapes;
 * `crates/trusty-console/tests/` covers the routes themselves.
 */

import { apiBase } from './base.js';
import { compactPath, deletePath, readCompactResult, readDeleteResult } from './palaceActions.js';

/** Where the hero row's seven figures come from. */
const METRICS_PATH = '../console/metrics/memory';

/**
 * Resolve one console-relative path against the injected API base.
 *
 * @param {string} path e.g. `'../console/metrics/memory'`
 * @returns {string} absolute URL
 */
export function consoleUrl(path) {
  return new URL(path, apiBase()).href;
}

/**
 * The console's cached trusty-memory metrics report, or `null`.
 *
 * `503` is the documented "no poll has completed yet" answer — the daemon is
 * absent, or this is first boot — and is a `null` rather than a throw, because
 * the hero row renders its seven tiles as unknown in that case instead of
 * replacing the page with an error.
 *
 * @returns {Promise<object|null>} the report, or `null` when none is cached
 */
export async function consoleMetrics() {
  const res = await fetch(consoleUrl(METRICS_PATH));
  if (res.status === 503) return null;
  if (!res.ok) throw new Error(`metrics: HTTP ${res.status}`);
  return res.json();
}

/**
 * Compact one palace through the console's route.
 *
 * @param {string} id palace id
 * @returns {Promise<{ok: boolean, message: string}>} the outcome to display
 */
export async function compactPalace(id) {
  return act(compactPath(id), { method: 'POST' }, readCompactResult, 'compact');
}

/**
 * Delete one palace through the console's route.
 *
 * @param {string} id palace id
 * @param {boolean} force delete even when the palace still holds drawers
 * @returns {Promise<{ok: boolean, message: string}>} the outcome to display
 */
export async function deletePalace(id, force) {
  return act(deletePath(id, force), { method: 'DELETE' }, readDeleteResult, 'delete');
}

/**
 * One action exchange: call the route, then believe only what the body says.
 *
 * A transport failure is reported as the console failing to reach its own
 * route, which is a different problem from the daemon refusing — and saying so
 * is what stops an operator retrying a delete that already happened.
 */
async function act(path, init, read, noun) {
  let status = 0;
  let body = null;
  try {
    const res = await fetch(consoleUrl(path), init);
    status = res.status;
    body = await res.json().catch(() => null);
  } catch (e) {
    return {
      ok: false,
      message: `The console could not reach its own ${noun} route: ${e.message || e}`,
    };
  }
  return read(status, body);
}
