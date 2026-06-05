/*
 * Why: Centralised reactive state for daemon health and the index catalogue,
 * so multiple views don't refetch on every mount.
 * What: Exports plain getters/setters backed by Svelte 5 runes, plus refresh
 * helpers. The shapes are intentionally flat so views can `$derived(getX())`
 * directly.
 * Test: Mount two views, call refreshIndexes() in one, observe the other
 * update its derived counters without a manual refresh.
 */

import { api } from './api.js';

let _health = $state(null);
let _indexes = $state([]); // [{ id, chunk_count, root_path }]
let _loading = $state(false);
let _error = $state(null);
let _liveStats = $state(null); // { indexes, total_chunks, uptime_secs, version }
let _statusSource = null;
let _statusRefcount = 0;

/**
 * Why: The Chat panel must be hidden when no provider is configured, and the
 * determination must work both before the first /health response arrives (via
 * the server-injected window.__OPENROUTER_ENABLED__ global) and stay reactive
 * once /health has loaded (which now includes a `chat_available` field).
 * What: Returns true when either the injected boot global is truthy OR the
 * most-recent /health response carries `chat_available: true`.
 * Test: Set window.__OPENROUTER_ENABLED__ = true in the browser console; call
 * getChatAvailable() and assert true. Set to false, assert false.
 */
export function getChatAvailable() {
  // Prefer the live /health value (updated by the poll loop) so a daemon
  // restart with a newly-configured key is picked up without page refresh.
  if (_health?.chat_available !== undefined) {
    return Boolean(_health.chat_available);
  }
  // Fall back to the server-injected boot global while the first /health
  // round-trip is still in flight.
  if (typeof window !== 'undefined' && window.__OPENROUTER_ENABLED__ !== undefined) {
    return Boolean(window.__OPENROUTER_ENABLED__);
  }
  return false;
}

export function getHealth() {
  return _health;
}

export function getLiveStats() {
  return _liveStats;
}

/**
 * Why: The dashboard's headline counters (Indexes / Documents / Uptime /
 * Version) should update without a manual refresh. The daemon exposes
 * `/status/stream` as a Server-Sent Events feed pushing
 * `{ indexes, total_chunks, uptime_secs, version }` every 2 seconds.
 * What: Opens a singleton EventSource (reference-counted across callers),
 * merges each event into `_health` so existing `getHealth()` consumers keep
 * working, and also exposes `getLiveStats()` for the full payload (including
 * `total_chunks`).
 * Test: Call `subscribeStatusStream()`, wait > 2s, assert
 * `getLiveStats().total_chunks` is a number; call `unsubscribeStatusStream()`
 * and assert no further messages arrive.
 */
export function subscribeStatusStream() {
  _statusRefcount += 1;
  if (_statusSource) return _statusSource;

  const src = new EventSource('/status/stream');
  src.onmessage = (ev) => {
    let event;
    try {
      event = JSON.parse(ev.data);
    } catch {
      return;
    }
    // Mirrors trusty-memory's pattern: switch on the tagged `type` field
    // and route each event variant to the appropriate state mutation.
    switch (event.type) {
      case 'status_changed': {
        const payload = {
          indexes: event.indexes ?? 0,
          total_chunks: event.total_chunks ?? 0,
          uptime_secs: event.uptime_secs ?? 0,
          version: event.version ?? ''
        };
        _liveStats = payload;
        // Mirror into _health so existing $derived(getHealth()) consumers
        // keep updating live without code changes elsewhere.
        _health = {
          status: 'ok',
          version: payload.version || _health?.version || '',
          indexes: payload.indexes,
          uptime_secs: payload.uptime_secs
        };
        break;
      }
      case 'index_registered':
      case 'index_removed': {
        // Why: A new/dropped index changes the catalogue the dashboard
        // renders. Re-fetch /indexes (fan-out across per-index /status)
        // so the "Recent indexes" table updates within one SSE round-trip
        // — no page refresh needed. Mirrors trusty-memory's
        // `palace_created` pattern.
        // What: Fire-and-forget refresh; errors leave the list intact.
        // Test: register an index via `POST /indexes`, observe the
        // dashboard table gain a row within ~1s without reloading.
        refreshIndexes().catch(() => {});
        break;
      }
      case 'connected':
      case 'lag':
      default:
        // Ignore connection-marker and lag-notice frames; EventSource
        // auto-reconnects on transient errors so no action needed here.
        break;
    }
  };
  src.onerror = () => {
    // EventSource auto-reconnects on transient errors; just note the blip.
    console.warn('SSE connection lost, will reconnect...');
  };
  _statusSource = src;
  return src;
}

export function unsubscribeStatusStream() {
  _statusRefcount = Math.max(0, _statusRefcount - 1);
  if (_statusRefcount === 0 && _statusSource) {
    _statusSource.close();
    _statusSource = null;
  }
}

export function getIndexes() {
  return _indexes;
}

export function getLoading() {
  return _loading;
}

export function getError() {
  return _error;
}

export async function refreshHealth() {
  try {
    _health = await api.health();
  } catch (e) {
    _health = { status: 'unreachable', version: '', indexes: 0, uptime_secs: 0 };
    _error = e.message || String(e);
  }
  return _health;
}

/**
 * Why: The /indexes endpoint only returns names; the admin UI wants chunk
 * counts and root paths for every index. We fan out per-index /status calls
 * in parallel and merge into a single array.
 * What: Refreshes `_indexes` to a list of `{ id, chunk_count, root_path }`.
 * Indexes whose status call fails are still included with `error: true`.
 * Test: Register two indexes, call refreshIndexes(), assert length === 2.
 */
export async function refreshIndexes() {
  _loading = true;
  _error = null;
  try {
    const body = await api.listIndexes();
    const names = body?.indexes || [];
    const pairs = await Promise.all(
      names.map(async (id) => {
        try {
          const s = await api.indexStatus(id);
          return {
            id,
            chunk_count: s.chunk_count ?? 0,
            root_path: s.root_path ?? '',
            disk_bytes: s.disk_bytes ?? null,
            last_indexed: s.last_indexed ?? null,
            error: false
          };
        } catch (_e) {
          return {
            id,
            chunk_count: 0,
            root_path: '',
            disk_bytes: null,
            last_indexed: null,
            error: true
          };
        }
      })
    );
    _indexes = pairs;
  } catch (e) {
    _error = e.message || String(e);
    _indexes = [];
  } finally {
    _loading = false;
  }
  return _indexes;
}
