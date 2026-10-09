/**
 * The header's link to the local Architect dashboard (#9474).
 *
 * Why: the owner asked the console to link to the Architect dashboard. That
 * dashboard is a separate local server, so the link must appear only while it
 * is running on this host — never a dead link on a host without one.
 *
 * What: the server decides liveness (`GET /api/console/architect-dashboard`
 * answers `{ url }` or `{ url: null }`); this module turns that answer into an
 * `href` or `null`. It re-checks the URL is loopback `http`, and hides the link
 * when the console page itself is viewed from another machine (a tailnet
 * viewer's 127.0.0.1 is their own host, so the link would be dead for them).
 * Test: `architectLink.test.js` — run `node --test src/architectLink.test.js`
 * from `crates/trusty-console/ui`.
 */

/** Window name the console's Architect link targets; reused if a tab has it. */
export const ARCHITECT_WINDOW_NAME = 'architect-dashboard';

/** This tab's own name, set on load; the Architect dashboard links target it. */
export const CONSOLE_WINDOW_NAME = 'trusty-console';

/** The route that reports the live dashboard URL. */
export const ARCHITECT_DASHBOARD_ROUTE = '/api/console/architect-dashboard';

/** Hostnames that name this machine's loopback interface. */
const LOOPBACK_HOSTS = new Set(['127.0.0.1', 'localhost', '[::1]', '::1']);

/** True when `hostname` is a loopback name, case-insensitively. */
function isLoopback(hostname) {
  return typeof hostname === 'string' && LOOPBACK_HOSTS.has(hostname.toLowerCase());
}

/**
 * The link target for a route answer, or `null` for no link.
 *
 * `null` unless the answer carries a parseable `http:` URL whose host is
 * loopback, with no userinfo, AND the console page is itself served from a
 * loopback host.
 */
export function dashboardHref(payload, pageHostname) {
  const raw = payload?.url;
  if (typeof raw !== 'string' || raw === '' || !isLoopback(pageHostname)) return null;
  let url;
  try {
    url = new URL(raw);
  } catch {
    return null;
  }
  if (url.protocol !== 'http:' || url.username || url.password) return null;
  return isLoopback(url.hostname) ? url.href : null;
}

/**
 * Ask the server for the live dashboard link, resolving to `null` on any
 * failure — a missing link is the safe answer, so this never rejects.
 */
export async function fetchArchitectDashboard(
  fetchImpl = fetch,
  pageHostname = globalThis.location?.hostname,
) {
  try {
    const resp = await fetchImpl(ARCHITECT_DASHBOARD_ROUTE);
    if (!resp.ok) return null;
    return dashboardHref(await resp.json(), pageHostname);
  } catch {
    return null;
  }
}
