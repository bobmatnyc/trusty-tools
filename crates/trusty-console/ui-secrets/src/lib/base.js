/*
 * Why: #9067 — the console serves this page at `/tools/secrets/` and answers
 * its calls under `/api/secrets/`. The console injects that prefix as
 * `window.__SECRETS_BASE__` (src/tools_ui.rs), the same arrangement the
 * search, memory and analyze pages use.
 * What: `apiUrl(verb)` resolves one route name against the injected base,
 * read once at module load. With no injected base it resolves against
 * `/api/secrets/` on the page's own origin; the page is never served by
 * anything but the console.
 * Test: `src/lib/base.test.js`.
 */

const FALLBACK = '/api/secrets/';

function computeBase() {
  if (typeof window !== 'undefined' && window.__SECRETS_BASE__) {
    const b = window.__SECRETS_BASE__;
    return b.endsWith('/') ? b : b + '/';
  }
  if (typeof document === 'undefined') {
    return FALLBACK;
  }
  return new URL(FALLBACK, document.baseURI).href;
}

const API_BASE = computeBase();

/**
 * The absolute URL of one `/api/secrets/<verb>` route.
 * @param {string} verb  e.g. `session`, `list`
 * @returns {string}
 */
export function apiUrl(verb) {
  return new URL(verb, API_BASE).href;
}
