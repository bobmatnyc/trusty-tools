/**
 * Shared API base-URL config for both app.ts (fetch) and transport.ts (invoke).
 *
 * Why: Both modules need the same "use http://localhost:8765 in Tauri,
 * same-origin empty string in browser" logic but can't import from each
 * other (transport.ts already imports getCurrentApiToken from app.ts, so
 * importing back would create a cycle). A third module breaks the cycle.
 *
 * Test: In Tauri mode (TAURI_INTERNALS present), apiBase() returns the
 * configured host or http://localhost:8765. In browser mode, returns ''.
 */

// Tauri v2: window.__TAURI_INTERNALS__; v1: window.__TAURI__.
export const isTauri: boolean =
  typeof window !== 'undefined' &&
  ('__TAURI_INTERNALS__' in window || '__TAURI__' in window);

// Must match the port in App.svelte's ensure_api_server call (currently 8765).
export const DEFAULT_API_PORT = 8765;

/**
 * Returns the API base URL for fetch() calls.
 * Empty string in browser (same-origin); absolute http:// URL in Tauri.
 */
export function apiBase(): string {
  const env = import.meta.env as Record<string, string | undefined>;
  if (isTauri) {
    return env.VITE_OMPM_API ?? `http://localhost:${DEFAULT_API_PORT}`;
  }
  return env.VITE_OMPM_API ?? '';
}
