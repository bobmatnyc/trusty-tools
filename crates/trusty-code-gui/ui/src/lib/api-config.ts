// Why: Centralizes daemon-URL resolution and runtime Tauri detection so the
// health panel and any future view agree on where the tcode daemon lives and
// how to reach it. Mirrors crates/trusty-mpm-gui/ui/src/lib/api-config.ts.
// What: Exposes the default daemon URL, an `isTauri()` runtime check, and an
// async `apiBase()` accessor — in Tauri mode it asks the Rust `get_daemon_url`
// command (which can read `TRUSTY_CODE_URL`); in web mode it honors a
// `localStorage` override, falling back to the default.
// Test: With localStorage empty and no Tauri runtime, `apiBase()` resolves to
// DEFAULT_DAEMON_URL; after setting `trusty-code.daemonUrl`, it resolves to
// the stored value.

/// Default tcode daemon HTTP endpoint — matches
/// `trusty_code::serve::DEFAULT_HTTP_PORT` (7881).
export const DEFAULT_DAEMON_URL = 'http://127.0.0.1:7881';

/** True when running inside the Tauri desktop runtime (v2 internals present). */
export const isTauri = (): boolean =>
  typeof window !== 'undefined' && '__TAURI_INTERNALS__' in window;

/**
 * Resolve the daemon base URL for this runtime.
 *
 * Why: only the native Tauri process can read `TRUSTY_CODE_URL`; a plain
 * browser tab has no such env access, so it falls back to a localStorage
 * override or the documented default.
 * What: Tauri mode invokes `get_daemon_url`; web mode reads
 * `localStorage['trusty-code.daemonUrl']`.
 */
export async function apiBase(): Promise<string> {
  if (isTauri()) {
    const { invoke } = await import('@tauri-apps/api/core');
    return invoke<string>('get_daemon_url');
  }
  if (typeof localStorage === 'undefined') return DEFAULT_DAEMON_URL;
  return localStorage.getItem('trusty-code.daemonUrl') ?? DEFAULT_DAEMON_URL;
}
