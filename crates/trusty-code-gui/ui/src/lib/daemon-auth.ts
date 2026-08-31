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
 * Do `url` and `base` share an origin?
 *
 * Why: the first version compared with `url.startsWith(base)`, the prefix-match
 * class this repo already fixed once in #3280. `http://127.0.0.1:7882` is a
 * prefix of `http://127.0.0.1:7882.attacker.example`, so a request to the
 * attacker's host would have carried the credential. Comparing parsed origins
 * makes the host, port, and scheme all exact.
 *
 * What: `new URL(...).origin` on both sides. An unparseable URL is not
 * same-origin — fail closed, send no credential.
 */
export function sameOrigin(url: string, base: string): boolean {
  try {
    return new URL(url).origin === new URL(base).origin;
  } catch {
    return false;
  }
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
    if (!sameOrigin(url, base)) return original(input, init);

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

/** Backoff between re-mint attempts after a stream drops, in milliseconds. */
const RECONNECT_BACKOFF_MS = [1000, 2000, 5000, 10000];

/** A live SSE subscription. `close()` stops it and cancels any pending retry. */
export interface DaemonEventStream {
  close(): void;
}

/** Mint a ticket for `path`, or `null` when none can be obtained. */
async function mintTicket(base: string, path: string): Promise<string | null> {
  const token = await daemonToken();
  if (!token) return null;
  const res = await fetch(
    `${base}${SSE_TICKET_PATH}?path=${encodeURIComponent(path)}`,
    { method: 'POST', headers: { Authorization: `Bearer ${token}` } },
  );
  if (!res.ok) return null;
  const { ticket } = (await res.json()) as { ticket?: string };
  return ticket ?? null;
}

/**
 * Open an authenticated SSE subscription to a daemon stream, and keep it open.
 *
 * Why the ticket: `EventSource` has no header API, and putting the durable
 * token in the query string would write it into the daemon's access log and
 * every tracing span. The daemon mints a single-use ticket that expires in
 * seconds, so a ticket captured from a log is already spent.
 *
 * Why this is a subscription rather than a bare `EventSource`: single-use is
 * exactly what breaks `EventSource`'s own reconnect. On any drop the browser
 * retries the SAME URL, which carries the SAME spent ticket, so the daemon
 * answers `401` and the stream dies permanently — silently, with the component
 * still holding a handle it believes is live. Reconnecting has to mint a fresh
 * ticket, which only this layer can do.
 *
 * What: mints, opens, and on `error` closes and re-mints on a bounded backoff
 * ([`RECONNECT_BACKOFF_MS`], then steady at its last value). `onMessage` is
 * rebound to each new `EventSource`, so a caller sees one continuous stream.
 * Returns `null` when the FIRST attempt cannot get a ticket — the caller then
 * falls back to polling, exactly as it already does when `EventSource` is
 * unavailable.
 */
export async function openDaemonEventStream(
  path: string,
  onMessage: (event: MessageEvent) => void,
): Promise<DaemonEventStream | null> {
  if (typeof EventSource === 'undefined') return null;
  const base = await apiBase();

  const first = await mintTicket(base, path);
  if (!first) return null;

  let source: EventSource | null = null;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let attempt = 0;
  let closed = false;

  const open = (ticket: string) => {
    if (closed) return;
    source = new EventSource(
      `${base}${path}?${TICKET_QUERY_PARAM}=${encodeURIComponent(ticket)}`,
    );
    source.onmessage = (event) => {
      // A message means the stream is healthy: reset the backoff so a later,
      // unrelated drop retries promptly rather than at the previous ceiling.
      attempt = 0;
      onMessage(event);
    };
    source.onerror = () => {
      // The browser would retry this URL itself, with the spent ticket. Close
      // first so it cannot, then come back with a fresh one.
      source?.close();
      source = null;
      scheduleReconnect();
    };
  };

  const scheduleReconnect = () => {
    if (closed || timer !== null) return;
    const delay =
      RECONNECT_BACKOFF_MS[Math.min(attempt, RECONNECT_BACKOFF_MS.length - 1)];
    attempt += 1;
    timer = setTimeout(() => {
      timer = null;
      if (closed) return;
      void (async () => {
        const ticket = await mintTicket(base, path);
        if (closed) return;
        if (ticket) open(ticket);
        else scheduleReconnect();
      })();
    }, delay);
  };

  open(first);

  return {
    close() {
      closed = true;
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      source?.close();
      source = null;
    },
  };
}
