// Why: #5439 put every tcode daemon route behind
// `Authorization: Bearer <token>`. This UI has ~30 `fetch()` sites spread over
// components and lib modules; adding the header at each one means the next site
// added forgets it and ships a 401 nobody notices until runtime. Installing ONE
// credential-attaching `fetch` at bootstrap is fail-CLOSED in the direction
// that matters: a new call site is authenticated by default rather than by
// remembering.
//
// The credential comes from the native shell (`get_daemon_token`), minted per
// launch and held in memory. #10166 kept it (owner ruling Q3) when the bridge
// stopped listening on a port: `fetch()` now reaches it over the
// `tcode-bridge://` scheme, and the two event streams over an IPC `Channel`
// (`open_daemon_stream`), each still presenting the credential or a ticket
// minted with it. A plain browser tab has no shell and so no credential.
//
// What: `installDaemonAuth()` wraps `globalThis.fetch` so requests to the
// bridge base URL carry the credential; `openDaemonEventStream()` opens a
// stream through the shell with a single-use ticket and keeps it open.
//
// Test: `daemon-auth.test.ts`.

import { apiBase, isTauri } from './api-config';

/** Route that exchanges the credential for a single-use SSE ticket. */
export const SSE_TICKET_PATH = '/auth/sse-ticket';

let cachedToken: string | null = null;

/**
 * Resolve the bridge credential, or '' when there is none.
 *
 * Why: cached after the first resolution because every request would otherwise
 * cross the Tauri IPC boundary. The credential is fixed for the app's whole
 * life, so there is nothing to invalidate.
 */
export async function daemonToken(): Promise<string> {
  if (cachedToken !== null) return cachedToken;
  if (!isTauri()) return '';
  const { invoke } = await import('@tauri-apps/api/core');
  cachedToken = await invoke<string>('get_daemon_token');
  return cachedToken;
}

/** Drop the cached credential — for tests. */
export function resetDaemonTokenCache(): void {
  cachedToken = null;
}

/**
 * Do `url` and `base` share an origin?
 *
 * Why: the first version compared with `url.startsWith(base)`, the prefix-match
 * class this repo already fixed once in #3280. A parsed comparison makes the
 * host, port, and scheme all exact.
 *
 * #10166: the comparison is `protocol` + `host`, not `URL.origin`. For a
 * non-special scheme such as `tcode-bridge:` the URL standard defines `origin`
 * as the opaque `"null"`, so an `origin` comparison would call EVERY
 * non-special URL — `evil-scheme://anywhere/` included — same-origin with the
 * bridge, and attach the credential to it.
 *
 * What: an unparseable URL, or an empty host on either side, is not
 * same-origin — fail closed, send no credential.
 */
export function sameOrigin(url: string, base: string): boolean {
  try {
    const a = new URL(url);
    const b = new URL(base);
    return a.host !== '' && a.protocol === b.protocol && a.host === b.host;
  } catch {
    return false;
  }
}

/**
 * Wrap `globalThis.fetch` so bridge requests carry the credential.
 *
 * What: leaves the URL, method, body, and every other option untouched; adds
 * `Authorization` only when the request targets the bridge base URL and does
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

/** A live stream subscription. `close()` stops it and cancels any pending retry. */
export interface DaemonEventStream {
  close(): void;
}

/** What the Rust side's stream pump sends (`bridge::channel::StreamEvent`). */
export type StreamEvent =
  | { kind: 'message'; data: string }
  | { kind: 'end'; error: string | null };

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
 * Open an authenticated subscription to a bridge stream, and keep it open.
 *
 * Why the ticket: it binds the stream to the path the token holder asked for
 * and is spent on first use, so a ticket captured from a log is worthless.
 *
 * Why this is a subscription rather than one open: a stream ends whenever the
 * daemon drops it, and every reopen needs a FRESH ticket, which only this
 * layer can mint.
 *
 * What (#10166): mints, then invokes `open_daemon_stream` with an IPC
 * `Channel`. Each `message` is handed to `onMessage` as a `MessageEvent` (the
 * shape the old `EventSource` delivered, so callers did not change); an `end`
 * — a clean close or a failure alike — is a drop, re-minted on a bounded
 * backoff ([`RECONNECT_BACKOFF_MS`], then steady at its last value). Events
 * from a superseded attempt are ignored. Returns `null` outside Tauri, or when
 * the FIRST attempt cannot get a ticket or open the stream — the caller then
 * falls back to polling.
 */
export async function openDaemonEventStream(
  path: string,
  onMessage: (event: MessageEvent) => void,
): Promise<DaemonEventStream | null> {
  if (!isTauri()) return null;
  const { Channel, invoke } = await import('@tauri-apps/api/core');
  const base = await apiBase();

  let current: number | null = null;
  let generation = 0;
  let timer: ReturnType<typeof setTimeout> | null = null;
  let attempt = 0;
  let closed = false;

  const stop = (id: number) => {
    void invoke('close_daemon_stream', { id }).catch(() => {
      // The stream is gone either way; nothing is left to release.
    });
  };

  /** One attempt: mint, open, and wire the channel. `false` when it failed. */
  const open = async (): Promise<boolean> => {
    let ticket: string | null;
    try {
      ticket = await mintTicket(base, path);
    } catch {
      // A rejected fetch is a failed attempt, never an exception that would
      // stop the reconnect loop for good.
      return false;
    }
    if (!ticket || closed) return false;
    const mine = ++generation;
    let ended = false;
    const channel = new Channel<StreamEvent>();
    channel.onmessage = (event) => {
      if (closed || mine !== generation) return;
      if (event.kind === 'message') {
        // A message means the stream is healthy: reset the backoff so a later,
        // unrelated drop retries promptly rather than at the previous ceiling.
        attempt = 0;
        onMessage(new MessageEvent('message', { data: event.data }));
        return;
      }
      ended = true;
      current = null;
      scheduleReconnect();
    };
    let id: number;
    try {
      id = await invoke<number>('open_daemon_stream', { path, ticket, onEvent: channel });
    } catch {
      return false;
    }
    if (closed || mine !== generation) {
      stop(id);
    } else if (!ended) {
      current = id;
    }
    return true;
  };

  const scheduleReconnect = () => {
    if (closed || timer !== null) return;
    const delay =
      RECONNECT_BACKOFF_MS[Math.min(attempt, RECONNECT_BACKOFF_MS.length - 1)];
    attempt += 1;
    timer = setTimeout(() => {
      timer = null;
      if (closed) return;
      void open().then((ok) => {
        if (!ok) scheduleReconnect();
      });
    }, delay);
  };

  if (!(await open())) return null;

  return {
    close() {
      closed = true;
      if (timer !== null) {
        clearTimeout(timer);
        timer = null;
      }
      if (current !== null) {
        stop(current);
        current = null;
      }
    },
  };
}
