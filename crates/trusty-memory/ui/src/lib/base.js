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
 * once at module load (before any navigation), then stripping a trailing
 * `index.html` and a trailing `ui/` path segment. Checks `window.__MEMORY_BASE__`
 * first so deployments that inject that global keep working.
 * Test: In a browser at http://127.0.0.1:7788/proxy/memory/ the return value
 * should be "http://127.0.0.1:7788/proxy/memory/"; at http://127.0.0.1:7079/
 * it should be "http://127.0.0.1:7079/". Unit-covered in base.test.js.
 * Verify proxy mode: open the SPA at /proxy/memory/ and confirm api.health()
 * fetches /proxy/memory/health not /health.
 *
 * NOTE on the `ui/` strip (issue #1329): trusty-search and trusty-analyze
 * mount their SPA at the daemon's `/ui/` route while the API endpoints are
 * siblings at the parent root, so those two SPAs MUST strip the trailing
 * `ui/` segment to reach the API. trusty-memory instead serves its SPA at the
 * ROOT (`/`) via a fallback handler with the API under `/api/v1/*` and
 * `/health` (see src/web/static_assets.rs), so document.baseURI never ends in
 * `ui/` here and the strip is a harmless no-op. It is kept identical across all
 * three files to honour the KEEP IN SYNC contract and stay correct if memory
 * ever moves to a `/ui/` mount.
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
 * 2. `document.baseURI` with trailing `index.html` and trailing `ui/` stripped.
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
  // 1. Strip a trailing "index.html" so the base always ends with "/".
  // 2. Strip the trailing "ui/" mount segment (no-op for memory's root-mounted
  //    SPA; required for the search/analyze `/ui/` mounts — issue #1329).
  return document.baseURI
    .replace(/index\.html$/, '')
    .replace(/(^|\/)ui\/$/, '$1');
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
