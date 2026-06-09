/*
 * Why: Centralized module-level reactive state shared by every view, so a
 * single fetch of /health or /indexes is reused across the dashboard,
 * complexity, smells, refactor, clusters, and facts panes. Persisting the
 * selected index in localStorage keeps context when the analyst reloads.
 * What: $state primitives + getter functions + refresh helpers calling the
 * analyzer HTTP API. Also exports initEventStream() which opens the daemon's
 * /sse stream and dispatches AnalyzerEvent frames to the right refresher.
 * Test: Call refreshHealth() in console, then getHealth() — assert non-null.
 */
import { api } from './api.js';
import { apiUrl } from './base.js';

const LS_KEY = 'trusty-analyzer.selectedIndex';
const LS_THEME_KEY = 'trusty-analyzer.theme';

/*
 * Why: Light/dark/system theme support. Applied via [data-theme] on <html>
 * so all CSS variables in tokens.css switch atomically. Persisted in
 * localStorage so the choice survives reload, and follows OS changes when
 * the user picks 'system'.
 * What: $state for the user's preference ('light' | 'dark' | 'system'), plus
 * applyTheme() which resolves 'system' against prefers-color-scheme.
 * Test: setTheme('light') and inspect <html data-theme>; should be 'light'.
 */
const _initialTheme =
  (typeof localStorage !== 'undefined' && localStorage.getItem(LS_THEME_KEY)) ||
  'system';
let _theme = $state(_initialTheme);

export const getTheme = () => _theme;

export function applyTheme(t) {
  if (typeof document === 'undefined') return;
  const resolved =
    t === 'system'
      ? window.matchMedia('(prefers-color-scheme: dark)').matches
        ? 'dark'
        : 'light'
      : t;
  document.documentElement.setAttribute('data-theme', resolved);
}

export function setTheme(t) {
  _theme = t;
  if (typeof localStorage !== 'undefined') {
    localStorage.setItem(LS_THEME_KEY, t);
  }
  applyTheme(t);
}

// Apply on module load so first paint matches user preference.
if (typeof document !== 'undefined') {
  applyTheme(_initialTheme);
  // React to OS-level changes while in 'system' mode.
  if (typeof window !== 'undefined' && window.matchMedia) {
    window
      .matchMedia('(prefers-color-scheme: dark)')
      .addEventListener('change', () => {
        if (getTheme() === 'system') applyTheme('system');
      });
  }
}

let _health = $state(null);
let _indexes = $state([]);
let _selectedIndex = $state(
  typeof localStorage !== 'undefined' ? localStorage.getItem(LS_KEY) || '' : ''
);
let _quality = $state(null);
let _hotspots = $state([]);
let _smells = $state([]);
let _refactors = $state([]);
let _clusters = $state([]);
let _facts = $state([]);
let _sseConnected = $state(false);

export const getHealth = () => _health;
export const getIndexes = () => _indexes;
export const getSelectedIndex = () => _selectedIndex;
export const getQuality = () => _quality;
export const getHotspots = () => _hotspots;
export const getSmells = () => _smells;
export const getRefactors = () => _refactors;
export const getClusters = () => _clusters;
export const getFacts = () => _facts;
export const getSseConnected = () => _sseConnected;

export function setSelectedIndex(id) {
  _selectedIndex = id || '';
  if (typeof localStorage !== 'undefined') {
    if (id) localStorage.setItem(LS_KEY, id);
    else localStorage.removeItem(LS_KEY);
  }
}

export async function refreshHealth() {
  _health = await api.health();
  return _health;
}

export async function refreshIndexes() {
  _indexes = await api.indexes();

  // The trusty-search index list is the source of truth. If the persisted
  // selection no longer exists (index was removed, renamed, or never existed
  // on this machine), drop it so we don't fire analysis calls for a ghost ID.
  const ids = _indexes.map((idx) => (typeof idx === 'string' ? idx : idx.id));
  if (_selectedIndex && !ids.includes(_selectedIndex)) {
    setSelectedIndex('');
  }

  // Auto-select first index if none active.
  if (!_selectedIndex && ids.length > 0) {
    setSelectedIndex(ids[0]);
  }
  return _indexes;
}

export async function refreshQuality(id) {
  if (!id) return null;
  _quality = await api.quality(id);
  return _quality;
}

export async function refreshHotspots(id, topK = 20) {
  if (!id) return [];
  _hotspots = await api.complexityHotspots(id, topK);
  return _hotspots;
}

export async function refreshSmells(id, category) {
  if (!id) return [];
  _smells = await api.smells(id, category);
  return _smells;
}

export async function refreshRefactors(id, opts) {
  if (!id) return [];
  _refactors = await api.refactorSuggestions(id, opts);
  return _refactors;
}

export async function refreshClusters(id, opts) {
  if (!id) return [];
  _clusters = await api.clusters(id, opts);
  return _clusters;
}

export async function refreshFacts(subject, predicate) {
  _facts = await api.listFacts(subject, predicate);
  return _facts;
}

/*
 * Why: The analyzer pushes `AnalyzerEvent` frames on /sse whenever an index is
 * re-analyzed, a fact is upserted, or SCIP data is ingested — so the dashboard
 * can refresh affected slices without polling.
 * What: Opens an EventSource and routes each event to the appropriate refresher
 * for the currently selected index. Returns the source so callers can close it
 * on teardown. EventSource auto-reconnects on transient disconnects.
 * Test: POST a fact and watch the facts table update without manual refresh.
 */
export function initEventStream() {
  const es = new EventSource(apiUrl('/sse'));
  es.onopen = () => {
    _sseConnected = true;
  };
  es.onmessage = (e) => {
    let event;
    try {
      event = JSON.parse(e.data);
    } catch {
      return;
    }
    const id = _selectedIndex;
    switch (event.type) {
      case 'connected':
        _sseConnected = true;
        break;
      case 'analysis_started':
        // Just an in-flight marker; nothing to refetch until completion.
        break;
      case 'analysis_completed':
        refreshIndexes().catch(() => {});
        if (id) {
          refreshQuality(id).catch(() => {});
          refreshHotspots(id).catch(() => {});
          refreshSmells(id).catch(() => {});
          refreshRefactors(id).catch(() => {});
        }
        break;
      case 'fact_upserted':
      case 'fact_deleted':
        refreshFacts().catch(() => {});
        break;
      case 'scip_ingested':
        if (id) refreshClusters(id).catch(() => {});
        break;
      default:
        break;
    }
  };
  es.onerror = () => {
    _sseConnected = false;
    // EventSource reconnects automatically.
    console.warn('SSE connection lost, will reconnect...');
  };
  return es;
}
