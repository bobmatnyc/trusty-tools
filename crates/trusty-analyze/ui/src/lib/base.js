/*
 * Why: When the SPA is served through the trusty-console reverse-proxy at
 * `/proxy/analyze/`, absolute fetch paths like `/health` or EventSource URLs
 * like `/sse` would resolve to the console host root instead of the daemon.
 * This helper derives the correct base URL from the document's actual location
 * so that all API calls work both when served directly by the daemon
 * (base = origin/) and when served under a proxy sub-path
 * (base = origin/proxy/analyze/).
 * What: Returns an absolute base URL string by stripping the trailing
 * `index.html` (if any) from document.baseURI, falling back to "/" when
 * running outside a browser (e.g. SSR tests). The legacy
 * `window.__ANALYZER_BASE__` override is still honoured if set, so any
 * existing deployment that injects that global keeps working.
 * Test: In a browser at http://127.0.0.1:7788/proxy/analyze/ the return
 * value should be "http://127.0.0.1:7788/proxy/analyze/"; at
 * http://127.0.0.1:7879/ it should be "http://127.0.0.1:7879/".
 * Verify proxy mode: open the SPA at /proxy/analyze/ and confirm api.health()
 * fetches /proxy/analyze/health not /health.
 */

/**
 * Returns the base URL for API calls. Checks (in order):
 * 1. `window.__ANALYZER_BASE__` (legacy override, kept for backward-compat).
 * 2. `document.baseURI` stripped of trailing `index.html`.
 * 3. "/" as a final fallback for non-browser environments.
 * @returns {string}
 */
export function apiBase() {
  if (typeof window !== 'undefined' && window.__ANALYZER_BASE__) {
    const b = window.__ANALYZER_BASE__;
    return b.endsWith('/') ? b : b + '/';
  }
  if (typeof document === 'undefined') {
    return '/';
  }
  return document.baseURI.replace(/index\.html$/, '');
}

/**
 * Resolves an API path relative to the derived base URL.
 * Paths starting with "/" are treated as relative to the base, NOT to the
 * origin, so "/health" under base "http://host/proxy/analyze/" becomes
 * "http://host/proxy/analyze/health".
 * @param {string} path  Absolute-looking path, e.g. "/health" or "/indexes"
 * @returns {string}     Fully-qualified URL string
 */
export function apiUrl(path) {
  const rel = path.startsWith('/') ? path.slice(1) : path;
  return new URL(rel, apiBase()).href;
}
