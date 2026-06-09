/*
 * Why: When the SPA is served through the trusty-console reverse-proxy at
 * `/proxy/memory/`, absolute fetch paths like `/health` or EventSource URLs
 * like `/sse` would resolve to the console host root instead of the daemon.
 * This helper derives the correct base URL from the document's actual location
 * so that all API calls work both when served directly by the daemon
 * (base = origin/) and when served under a proxy sub-path
 * (base = origin/proxy/memory/).
 * What: Returns an absolute base URL string by stripping the trailing
 * `index.html` (if any) from document.baseURI, falling back to "/" when
 * running outside a browser (e.g. SSR tests).
 * Test: In a browser at http://127.0.0.1:7788/proxy/memory/ the return value
 * should be "http://127.0.0.1:7788/proxy/memory/"; at http://127.0.0.1:7079/
 * it should be "http://127.0.0.1:7079/".
 * Verify proxy mode: open the SPA at /proxy/memory/ and confirm api.health()
 * fetches /proxy/memory/health not /health.
 */

/**
 * Returns the base URL for API calls, derived from the current document
 * location. Strips a trailing "index.html" so the base always ends with "/".
 * @returns {string}
 */
export function apiBase() {
  if (typeof document === 'undefined') {
    return '/';
  }
  return document.baseURI.replace(/index\.html$/, '');
}

/**
 * Resolves an API path relative to the derived base URL.
 * Paths starting with "/" are treated as relative to the base, NOT to the
 * origin, so "/health" under base "http://host/proxy/memory/" becomes
 * "http://host/proxy/memory/health".
 * @param {string} path  Absolute-looking path, e.g. "/health" or "/api/v1/status"
 * @returns {string}     Fully-qualified URL string
 */
export function apiUrl(path) {
  const rel = path.startsWith('/') ? path.slice(1) : path;
  return new URL(rel, apiBase()).href;
}
