// Why: #5439 put every tcode daemon route except `GET /health` behind
// `Authorization: Bearer <token>`. This UI has ~30 `fetch()` sites spread over
// components and lib modules; adding the header at each one means the next site
// added forgets it and ships a 401 nobody notices until runtime. Installing ONE
// credential-attaching `fetch` at bootstrap is fail-CLOSED in the direction
// that matters: a new call site is authenticated by default rather than by
// remembering.
//
// The credential comes from the native shell (`get_daemon_token`), the only
// side that can read the daemon's 0600 token file. A plain browser tab cannot,
// so it falls back to a `localStorage` override — the same shape as the
// existing daemon-URL override, and the only way `pnpm dev` against a real
// daemon stays workable. That override is a developer convenience, not a
// security boundary: a page that can write this app's localStorage is already
// running as this app.
//
// What: `installDaemonAuth()` wraps `globalThis.fetch` so requests to the
// daemon base URL carry the credential; `openDaemonEventStream()` is the SSE
// counterpart, because `EventSource` cannot send a header at all and must
// exchange the credential for a single-use ticket instead.
//
// Test: `daemon-auth.test.ts`.

import { apiBase, isTauri } from './api-config';

/** localStorage key holding a hand-pasted credential for plain-browser use. */
export const TOKEN_STORAGE_KEY = 'trusty-code.daemonToken';

/** Query parameter the daemon reads an SSE ticket from. */
export const TICKET_QUERY_PARAM = 'ticket';

/** Route that exchanges the credential for a single-use SSE ticket. */
export const SSE_TICKET_PATH = '/auth/sse-ticket';

let cachedToken: string | null = null;

/**
 * Resolve the daemon credential, or `''` when there is none.
 *
 * Why: cached after the first resolution because every request would otherwise
 * cross the Tauri IPC boundary. The credential is fixed for a daemon's whole
 * life, so there is nothing to invalidate — a daemon restart that rotates it
 * also drops every connection this page holds.
 */
export async function daemonToken(): Promise<string> {
  if (cachedToken !== null) return cachedToken;
  if (isTauri()) {
    const { invoke } = await import('@tauri-apps/api/core');
    cachedToken = await invoke<string>('get_daemon_token');
    return cachedToken;
  }
  try {
    cachedToken = localStorage.getItem(TOKEN_STORAGE_KEY) ?? '';
  } catch {
    // A browser with site data blocked throws on access; no credential is a
    // valid state, so this is not an error path.
    cachedToken = '';
  }
  return cachedToken;
}

/** Drop the cached credential — for tests, and for a re-read after a restart. */
export function resetDaemonTokenCache(): void {
  cachedToken = null;
}

/**
 * Wrap `globalThis.fetch` so daemon requests carry the credential.
 *
 * What: leaves the URL, method, body, and every other option untouched; adds
 * `Authorization` only when the request targets the daemon base URL and does
 * not already set the header itself. Idempotent — calling it twice does not
 * stack two wrappers.
 */
export function installDaemonAuth(): void {
  const original = globalThis.fetch;
  if ((original as { __daemonAuth?: boolean }).__daemonAuth) return;

  const wrapped = async (input: RequestInfo | URL, init?: RequestInit) => {
    const url =
      typeof input === 'string' ? input : input instanceof URL ? input.href : input.url;
    const base = await apiBase();
    if (!url.startsWith(base)) return original(input, init);

    const token = await daemonToken();
    if (!token) return original(input, init);

    const headers = new Headers(init?.headers ?? (input instanceof Request ? input.headers : undefined));
    if (headers.has('Authorization')) return original(input, init);
    headers.set('Authorization', `Bearer ${token}`);
    return original(input, { ...init, headers });
  };
  (wrapped as { __daemonAuth?: boolean }).__daemonAuth = true;
  globalThis.fetch = wrapped as typeof fetch;
}

/**
 * Open an authenticated `EventSource` against a daemon SSE route.
 *
 * Why: `EventSource` has no header API, and putting the durable token in the
 * query string would write it into the daemon's access log and every tracing
 * span. The daemon mints a single-use ticket that expires in seconds, so a
 * ticket captured from a log is already spent.
 *
 * What: `path` is daemon-relative (`/sessions/x/events`). Returns `null` when
 * no ticket can be obtained — the caller then falls back to polling, exactly as
 * it already does when `EventSource` is unavailable.
 */
export async function openDaemonEventStream(path: string): Promise<EventSource | null> {
  if (typeof EventSource === 'undefined') return null;
  const base = await apiBase();
  const token = await daemonToken();
  if (!token) return null;

  const res = await fetch(`${base}${SSE_TICKET_PATH}`, {
    method: 'POST',
    headers: { Authorization: `Bearer ${token}` },
  });
  if (!res.ok) return null;
  const { ticket } = (await res.json()) as { ticket?: string };
  if (!ticket) return null;

  return new EventSource(`${base}${path}?${TICKET_QUERY_PARAM}=${encodeURIComponent(ticket)}`);
}
