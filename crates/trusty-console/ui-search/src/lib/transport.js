/*
 * Why: ADR-0032 makes the console the only TCP surface; it reaches
 * trusty-search over the daemon's Unix socket. The same bundle is still
 * served by the daemon's own `/ui` while its HTTP listener lives (#6285 has
 * not retired it). The Configuration view must state the transport that is
 * actually in use and never invent a port or URL for the daemon.
 * What: `dashboardTransport` reads only facts — the console-injected
 * `__SEARCH_BASE__`, the daemon-injected `__DAEMON_PORT__`, and a `transport`
 * object on `/health` if the daemon reports one (proposed in #9030:
 * `{ socket_path: string|null, http_addr: string|null }`). A value no source
 * supplies is `null`, never a default.
 * `chatUnavailableReason` explains why the chat panel is off for that mode.
 * Test: `Config.test.js`, `transport.test.js`.
 */

import { apiBase } from './base.js';

/**
 * @typedef {{
 *   mode: 'console' | 'daemon-http',
 *   route: string,
 *   httpPort: number | null,
 *   socketPath: string | null,
 *   httpAddr: string | null,
 *   reported: boolean,
 * }} DashboardTransport
 */

/** The prefix trusty-console bridges to the daemon socket (`tools_ui.rs` `SEARCH.api_base`). */
export const CONSOLE_API_PREFIX = '/api/search/';

/**
 * Why: `__SEARCH_BASE__` is also the documented override for other
 * deployments (base.js), so its mere presence does not mean the console
 * serves the page. An override pointing at another host is a direct
 * connection, not the console's socket bridge.
 * What: true only when the base resolves to this page's own origin under the
 * console's `/api/search/` prefix. That covers both a path-only value and the
 * absolute same-origin URL the console injects (`new URL("/api/search/",
 * document.baseURI).href`, `tools_ui.rs`).
 * Test: `transport.test.js`.
 * @param {any} win
 */
export function isConsoleServed(win = typeof window === 'undefined' ? undefined : window) {
  const raw = win?.__SEARCH_BASE__;
  if (typeof raw !== 'string' || raw === '') return false;
  const origin = win?.location?.origin;
  let url;
  try {
    url = new URL(raw, origin && origin !== 'null' ? origin : 'http://localhost/');
  } catch {
    return false;
  }
  const sameOrigin = raw.startsWith('/') || (origin != null && url.origin === origin);
  return sameOrigin && url.pathname.startsWith(CONSOLE_API_PREFIX);
}

/**
 * @param {any} health  The latest `/health` body, or null.
 * @param {any} [win]
 * @returns {DashboardTransport}
 */
export function dashboardTransport(health, win = typeof window === 'undefined' ? undefined : window) {
  const reported = health && typeof health.transport === 'object' && health.transport !== null
    ? health.transport
    : null;
  const socketPath = typeof reported?.socket_path === 'string' ? reported.socket_path : null;
  const httpAddr = typeof reported?.http_addr === 'string' ? reported.http_addr : null;
  if (isConsoleServed(win)) {
    // The console prefix only: its host is the console's own listener, which
    // says nothing about how the console reaches the daemon.
    const route = new URL(win.__SEARCH_BASE__, 'http://localhost/').pathname;
    return { mode: 'console', route, httpPort: null, socketPath, httpAddr, reported: reported !== null };
  }
  // Served by the daemon's own HTTP listener: this page's API origin IS that
  // listener, and the daemon injects the port it bound. A missing or
  // non-integer port stays null.
  const port = Number.isInteger(win?.__DAEMON_PORT__) ? win.__DAEMON_PORT__ : null;
  const route = new URL(apiBase(), 'http://localhost/').origin;
  return { mode: 'daemon-http', route, httpPort: port, socketPath, httpAddr, reported: reported !== null };
}

/**
 * Why the chat panel is disabled, in the operator's terms.
 * @param {'console' | 'daemon-http'} mode
 */
export function chatUnavailableReason(mode) {
  if (mode === 'console') {
    return 'Chat is not available through the console: no trusty-search socket method serves /chat (#6285).';
  }
  return 'Chat needs OPENROUTER_API_KEY in the daemon environment or a running local model server (Ollama / LM Studio).';
}
