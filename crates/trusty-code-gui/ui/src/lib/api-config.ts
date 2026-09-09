// Why: centralizes base-URL resolution so every view agrees on where to reach
// the daemon. #6637 changed what is on the other end: the daemon speaks framed
// JSON-RPC over a Unix socket now, and a webview cannot dial one, so this app's
// own Rust side runs an HTTP+SSE bridge and `get_daemon_url` returns THAT. From
// this file's side nothing else changed — it is still "the base URL to fetch()".
//
// What went away with the daemon's TCP listener: `DEFAULT_DAEMON_URL`. It was a
// third manually-maintained copy of the daemon's default port (7882), pinned by
// a test that read the Rust constant back out of `serve/mod.rs`. The bridge
// binds an EPHEMERAL loopback port, so there is no default to copy and nothing
// to pin — the port is a fact only the native side knows, which is exactly what
// `get_daemon_url` is for.
//
// What: an `isTauri()` runtime check and an async `apiBase()` accessor. In Tauri
// mode `apiBase()` asks the Rust side; in a plain browser tab it reads the
// localStorage override, because a page outside this app cannot discover an
// ephemeral port that a process it is not part of assigned to itself.
// Test: `api-config.test.ts`.

/** localStorage key a plain browser tab reads the bridge URL from. */
export const BASE_URL_STORAGE_KEY = 'trusty-code.daemonUrl';

/** True when running inside the Tauri desktop runtime (v2 internals present). */
export const isTauri = (): boolean =>
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/**
 * Resolve the bridge base URL for this runtime.
 *
 * Why web mode has no fallback: the bridge binds `127.0.0.1:0` inside the Tauri
 * process, so its port is different every launch and no page outside that
 * process can guess it. A hardcoded default would be a URL that is wrong by
 * construction — and after #6637's PR 2c there is nothing listening on the
 * daemon's old port either. `pnpm dev` against a running app sets
 * `BASE_URL_STORAGE_KEY` to the URL the shell logs at launch.
 *
 * What: Tauri mode invokes `get_daemon_url`; web mode reads the localStorage
 * override, or `''` when there is none — which surfaces as a failed fetch the
 * views already render, rather than a silent request to the wrong host.
 */
export async function apiBase(): Promise<string> {
  if (isTauri()) {
    const { invoke } = await import('@tauri-apps/api/core');
    return invoke<string>('get_daemon_url');
  }
  try {
    return localStorage.getItem(BASE_URL_STORAGE_KEY) ?? '';
  } catch {
    // A browser with site data blocked throws on access; an unconfigured base
    // is a valid state, so this is not an error path.
    return '';
  }
}
