// Why: centralizes base-URL resolution so every view agrees on where to reach
// the daemon. The daemon speaks framed JSON-RPC over a Unix socket (#6637) and
// a webview cannot dial one, so this app's Rust side runs the bridge and
// `get_daemon_url` names it. Since #10166 the bridge opens no port: the base is
// the `tcode-bridge://localhost` custom URI scheme, answered in-process by the
// Rust side. From this file's side it is still "the base URL to fetch()".
//
// macOS only (owner ruling Q5, 2026-10-23): WebKit addresses a custom scheme as
// `<scheme>://localhost`; Windows and Android would need
// `http://tcode-bridge.localhost`, which this app does not support.
//
// What: an `isTauri()` runtime check and an async `apiBase()` accessor. In
// Tauri mode `apiBase()` asks the Rust side; in a plain browser tab it is `''`.
// There is no browser mode any more (owner ruling Q2): nothing listens, so a
// page outside this app has nothing to reach, and the `localStorage` URL
// override that pointed at the old loopback port is gone.
// Test: `api-config.test.ts`.

/** True when running inside the Tauri desktop runtime (v2 internals present). */
export const isTauri = (): boolean =>
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/**
 * Resolve the bridge base URL for this runtime.
 *
 * What: Tauri mode invokes `get_daemon_url` (`tcode-bridge://localhost`); a
 * plain browser tab gets `''`, which surfaces as a failed fetch the views
 * already render, rather than a silent request to some other host.
 */
export async function apiBase(): Promise<string> {
  if (!isTauri()) return '';
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke<string>('get_daemon_url');
}
