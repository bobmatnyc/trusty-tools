// Why: Centralizes daemon-URL resolution and runtime Tauri detection so every
// other module agrees on where the daemon lives and which transport to use.
// What: Exposes the default daemon URL and an `isTauri()` runtime check.
// `apiBase()` always resolves to DEFAULT_DAEMON_URL — there is no
// localStorage override (removed in #3315: nothing ever wrote it, and the
// CSP `connect-src` in `tauri.conf.json` is pinned to DEFAULT_DAEMON_URL
// anyway, so an override could never have reached a different host).
// Test: `apiBase()` returns DEFAULT_DAEMON_URL.

export const DEFAULT_DAEMON_URL = 'http://127.0.0.1:7880';

/** True when running inside the Tauri desktop runtime (v2 internals present). */
export const isTauri = (): boolean =>
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/** Resolve the daemon base URL. */
export function apiBase(): string {
  return DEFAULT_DAEMON_URL;
}
