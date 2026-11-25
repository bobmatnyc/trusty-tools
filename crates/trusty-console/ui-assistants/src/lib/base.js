/*
 * Why: #11749 — the console serves this page at `/tools/assistants/` and
 * answers its calls under `/api/console/assistants`. The console injects that
 * prefix as `window.__ASSISTANTS_BASE__` (src/tools_ui.rs), the same
 * arrangement the search, memory, analyze and secrets pages use.
 * What: `listUrl()`, `turnsUrl(id, limit)` and `askUrl(id)` resolve against
 * the injected base, read once at module load. With no injected base they
 * resolve against `/api/console/assistants` on the page's own origin. The
 * list route has no trailing slash, so the base is kept without one.
 * Test: `src/lib/base.test.js`.
 */

const FALLBACK = '/api/console/assistants';

function computeBase() {
  let base = FALLBACK;
  if (typeof window !== 'undefined' && window.__ASSISTANTS_BASE__) {
    base = window.__ASSISTANTS_BASE__;
  } else if (typeof document !== 'undefined') {
    base = new URL(FALLBACK, document.baseURI).href;
  }
  return base.replace(/\/+$/, '');
}

const API_BASE = computeBase();

/** `GET /api/console/assistants`. */
export function listUrl() {
  return API_BASE;
}

/**
 * `GET /api/console/assistants/{id}/turns?limit=N`.
 * @param {string} id
 * @param {number} limit
 */
export function turnsUrl(id, limit) {
  return `${API_BASE}/${encodeURIComponent(id)}/turns?limit=${limit}`;
}

/**
 * `POST /api/console/assistants/{id}/ask`.
 * @param {string} id
 */
export function askUrl(id) {
  return `${API_BASE}/${encodeURIComponent(id)}/ask`;
}
