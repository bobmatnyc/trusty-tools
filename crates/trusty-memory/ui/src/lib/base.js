// KEEP IN SYNC WITH crates/trusty-{analyze,search}/ui/src/lib/base.js
/*
 * Why: When the SPA is served through the trusty-console reverse-proxy at
 * `/proxy/memory/`, absolute fetch paths like `/health` or EventSource URLs
 * like `/sse` would resolve to the console host root instead of the daemon.
 * This helper derives the correct base URL from the document's actual location
 * so that all API calls work both when served directly by the daemon
 * (base = origin/) and when served under a proxy sub-path
 * (base = origin/proxy/memory/).
 * What: Returns an absolute base URL string by snapshotting document.baseURI
 * once at module load (before any navigation), stripping the trailing
 * `index.html` if present. Checks `window.__MEMORY_BASE__` first so
 * deployments that inject that global keep working.
 * Test: In a browser at http://127.0.0.1:7788/proxy/memory/ the return value
 * should be "http://127.0.0.1:7788/proxy/memory/"; at http://127.0.0.1:7079/
 * it should be "http://127.0.0.1:7079/".
 * Verify proxy mode: open the SPA at /proxy/memory/ and confirm api.health()
 * fetches /proxy/memory/health not /health.
 *
 * NOTE: The base is snapshotted once at module-init time (see API_BASE
 * below). All three SPAs use hash-based routing, so location.pathname never
 * changes after load — but snapshotting makes the helper robust if that
 * ever changes.
 */

/**
 * Compute the base URL once from the current document location.
 * Checks (in order):
 * 1. `window.__MEMORY_BASE__` (override, for deployment flexibility).
 * 2. `document.baseURI` stripped of trailing `index.html`.
 * 3. "/" as a final fallback for non-browser environments.
 * @returns {string}
 */
function computeBase() {
  if (typeof window !== 'undefined' && window.__MEMORY_BASE__) {
    const b = window.__MEMORY_BASE__;
    return b.endsWith('/') ? b : b + '/';
  }
  if (typeof document === 'undefined') {
    return '/';
  }
  return document.baseURI.replace(/index\.html$/, '');
}

// Snapshot the base once at module load. This runs before any client-side
// navigation, guaranteeing the proxy sub-path is captured correctly even if
// routing ever switches to pathname-based navigation in the future.
const API_BASE = computeBase();

/**
 * Returns the snapshotted base URL for API calls.
 * @returns {string}
 */
export function apiBase() {
  return API_BASE;
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
  return new URL(rel, API_BASE).href;
}
