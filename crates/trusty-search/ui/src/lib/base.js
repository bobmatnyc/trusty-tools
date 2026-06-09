/*
 * Why: When the SPA is served through the trusty-console reverse-proxy at
 * `/proxy/search/`, absolute fetch paths like `/health` or EventSource URLs
 * like `/status/stream` would resolve to the console host root instead of
 * the daemon. This helper derives the correct base URL from the document's
 * actual location so that all API calls work both when served directly by
 * the daemon (base = origin/) and when served under a proxy sub-path
 * (base = origin/proxy/search/).
 * What: Returns an absolute base URL string by stripping the trailing
 * `index.html` (if any) from document.baseURI, falling back to the
 * document's origin when running outside a browser (e.g. SSR tests).
 * Test: In a browser at http://127.0.0.1:7788/proxy/search/ the return
 * value should be "http://127.0.0.1:7788/proxy/search/"; at
 * http://127.0.0.1:7878/ it should be "http://127.0.0.1:7878/".
 * Verify proxy mode: check that api.health() sends to /proxy/search/health
 * not /health when the SPA is opened through the console.
 */

/**
 * Returns the base URL for API calls, derived from the current document
 * location. Strips a trailing "index.html" so the base always ends with "/".
 * @returns {string}
 */
export function apiBase() {
  if (typeof document === 'undefined') {
    // Non-browser environment (tests / SSR); fall back to a relative root.
    return '/';
  }
  // document.baseURI is the fully-qualified URL of the document (or the
  // effective <base href> if one is set). With base:'./' in Vite, no <base>
  // tag is emitted, so this equals the URL of the HTML page itself.
  // Strip trailing "index.html" so the base always ends with "/".
  return document.baseURI.replace(/index\.html$/, '');
}

/**
 * Resolves an API path relative to the derived base URL.
 * Paths starting with "/" are treated as relative to the base, NOT to the
 * origin, so "/health" under base "http://host/proxy/search/" becomes
 * "http://host/proxy/search/health".
 * @param {string} path  Absolute-looking path, e.g. "/health" or "/indexes"
 * @returns {string}     Fully-qualified URL string
 */
export function apiUrl(path) {
  // Strip the leading "/" so the path is treated as relative to the base
  // directory, not to the server root.
  const rel = path.startsWith('/') ? path.slice(1) : path;
  return new URL(rel, apiBase()).href;
}
