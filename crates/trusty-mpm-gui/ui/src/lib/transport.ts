// Why: Components must work identically in the Tauri desktop app and the
// standalone browser build. This module is the single seam: in Tauri mode it
// forwards to native IPC, in web mode it issues REST calls against the daemon.
// Components never call `fetch` or `@tauri-apps/api` directly.
// What: A dual-mode `invoke(command, args)` plus `subscribeEvents(...)` SSE
// helper. `API_MAP` maps each command name to an HTTP method + path builder.
// Test: In a browser (no Tauri), `invoke('list_sessions')` performs a GET to
// `${apiBase()}/sessions`; under Tauri it calls the Rust command of the same
// name. `subscribeEvents(null, cb)` opens an EventSource on `/events`.

import { apiBase, isTauri } from './api-config';
import type { ChatMessage } from '../stores/app';

/** Maps frontend command names to daemon REST routes. */
const API_MAP: Record<string, { method: string; path: (args: any) => string }> = {
  check_health: { method: 'GET', path: () => '/health' },
  list_sessions: { method: 'GET', path: () => '/sessions' },
  pause_session: { method: 'POST', path: (a) => `/sessions/${a.id}/pause` },
  resume_session: { method: 'POST', path: (a) => `/sessions/${a.id}/resume` },
  stop_session:   { method: 'DELETE', path: (a) => `/sessions/${a.id}` },
  get_breakers:   { method: 'GET', path: () => '/breakers' },
  get_daemon_url: { method: 'GET', path: () => '/health' }, // no-op in web mode
  session_output: { method: 'GET', path: (a) => `/sessions/${a.id}/output` },
  coordinator_context: { method: 'GET', path: () => '/api/v1/sessions/context' },
  coordinator_chat: { method: 'POST', path: () => '/api/v1/sessions/chat' },
};

/** Forward a call to the native Tauri command of the same name. */
async function tauriInvoke(command: string, args?: Record<string, unknown>): Promise<any> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke(command, args);
}

/** Issue a REST call against the daemon for `command`. */
async function restInvoke(command: string, args?: Record<string, unknown>): Promise<any> {
  // get_daemon_url has no REST equivalent — answer locally in web mode.
  if (command === 'get_daemon_url') return apiBase();

  const mapping = API_MAP[command];
  if (!mapping) throw new Error(`Unknown command: ${command}`);

  const url = `${apiBase()}${mapping.path(args ?? {})}`;
  const options: RequestInit = {
    method: mapping.method,
    headers: { 'Content-Type': 'application/json' },
  };
  if (mapping.method !== 'GET' && args) {
    options.body = JSON.stringify(args);
  }

  const response = await fetch(url, options);
  if (!response.ok) {
    const text = await response.text();
    throw new Error(text || `HTTP ${response.status}`);
  }
  const contentType = response.headers.get('content-type');
  if (contentType?.includes('application/json')) return response.json();
  return response.text();
}

/**
 * Why: Single entrypoint so components can ignore which runtime they are in.
 * What: Dispatches to Tauri IPC or REST based on `isTauri()`.
 * Test: Stub `window.__TAURI_INTERNALS__` and assert the Tauri path is taken;
 * remove it and assert a `fetch` is issued.
 */
export async function invoke(command: string, args?: Record<string, unknown>): Promise<any> {
  return isTauri() ? tauriInvoke(command, args) : restInvoke(command, args);
}

/**
 * Fetch the coordinator's current context (active sessions, workdirs).
 *
 * Why: `CoordinatorChat` opens with a greeting summarizing what the
 * coordinator can see; this is the single call that supplies that snapshot.
 * What: Dual-mode wrapper over `GET /api/v1/sessions/context`.
 * Test: With the daemon up, the returned value is a JSON object describing
 * the active sessions.
 */
export async function coordinatorContext(): Promise<any> {
  return invoke('coordinator_context');
}

/**
 * Send a chat turn to the coordinator and get its reply.
 *
 * Why: The coordinator chat is the GUI's permanent main panel; every user
 * message flows through here. `@session-name:` prefixes are interpreted
 * server-side, which may populate `routed_to` / `command_output` on the reply.
 * What: Dual-mode wrapper over `POST /api/v1/sessions/chat` carrying the
 * new message plus prior history for context.
 * Test: Post a plain message → a `coordinator` reply returns; post one
 * prefixed with `@id:` → the reply (or the user echo) carries `routed_to`.
 */
export async function coordinatorChat(
  message: string,
  history: ChatMessage[],
): Promise<any> {
  return invoke('coordinator_chat', { message, history });
}

/**
 * Subscribe to the daemon's SSE event stream.
 *
 * Why: The EventFeed and live session updates need a push channel; SSE works
 * the same in Tauri's webview and a plain browser, so one helper covers both.
 * What: Opens an `EventSource` on `/events` (global) or `/sessions/{id}/events`
 * (scoped), parses each JSON message, and invokes `cb`. Returns an unsubscribe
 * function that closes the connection.
 * Test: Start the daemon, call `subscribeEvents(null, cb)`, emit an event, and
 * assert `cb` fires; call the returned function and assert the stream closes.
 */
export function subscribeEvents(
  sessionId: string | null,
  cb: (event: any) => void,
): () => void {
  const path = sessionId ? `/sessions/${sessionId}/events` : '/events';
  const source = new EventSource(`${apiBase()}${path}`);

  source.onmessage = (e) => {
    try {
      cb(JSON.parse(e.data));
    } catch {
      // Ignore malformed event payloads.
    }
  };

  return () => source.close();
}
