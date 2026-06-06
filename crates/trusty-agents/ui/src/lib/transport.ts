// Transport abstraction — works in both Tauri and browser contexts.
//
// Why: The Tauri build talks to Rust over `invoke()`, which handles process
// spawning + polling + event forwarding server-side. A plain `pnpm dev`
// browser build has no Tauri runtime, so we fall back to the REST API exposed
// by `open-mpm --api`. Having one function means components don't care which
// transport is active. Same dual-mode pattern as ai-commander's transport.ts.
// What: `invoke(command, args)` dispatches to Tauri when available, else
// translates to a REST call against `http://localhost:<port>/api/...`.
// Test: In Tauri, calling `invoke('check_health')` hits the Rust handler. In
// a plain browser (no Tauri globals), it hits `GET /api/health`.

import { getCurrentApiToken } from '../stores/app';
import { isTauri, apiBase } from './api-config';

// Why: Browser mode has no Tauri event system, but ChatView still subscribes
// to `task-progress` / `task-complete` / `task-error` to update the UI as a
// task runs. We use a tiny in-process EventTarget so the polling loop in
// `fetchFallback` can publish progress and ChatView's existing listeners pick
// them up unchanged.
// What: A single `EventTarget` shared by `listenEvent` (subscribe) and
// `emitWeb` (publish) when running outside Tauri.
// Test: In a browser, call `listenEvent('foo', cb)` then `emitWeb('foo', 1)`
// and assert `cb` was invoked with `1`.
const webBus = new EventTarget();

function emitWeb<T>(event: string, payload: T): void {
  webBus.dispatchEvent(new CustomEvent(event, { detail: payload }));
}

// Why: Some callers (App.svelte's SSE bridge) sit outside this module but
// still need to translate incoming server events into the same `webBus`
// names ChatView / TaskHistory already listen on. Re-exporting the emitter
// keeps the bus internal while letting the bridge fan out without inventing
// a parallel event channel.
export const emitWebEvent = emitWeb;

/**
 * Why: When `open-mpm --api --api-token …` is running, every `/api/*` request
 * (other than `/api/health` and `/api/config`) needs `Authorization: Bearer
 * <token>`. Centralizing header construction means callers can't accidentally
 * skip it. (#181)
 * What: Returns a header bag including `Authorization` when a non-empty token
 * is present in the app store; otherwise returns an empty object.
 * Test: `setApiToken('abc')`, call `authHeaders()`, assert
 * `{ Authorization: 'Bearer abc' }`. With empty token, assert `{}`.
 */
function authHeaders(): Record<string, string> {
  const t = getCurrentApiToken();
  return t ? { Authorization: `Bearer ${t}` } : {};
}

async function tauriInvoke(command: string, args?: Record<string, unknown>): Promise<unknown> {
  const { invoke } = await import('@tauri-apps/api/core');
  return invoke(command, args);
}

/**
 * Why: Fallback used when the frontend is served by `vite` (no Tauri runtime).
 * What: Translates the small set of GUI Tauri commands to REST calls against
 * the `open-mpm --api` server so the same UI code works in the browser during
 * development without spinning up the desktop shell.
 * Test: `VITE_OMPM_API=http://localhost:7654 pnpm dev` then load `/` — submit
 * a message, verify network shows `POST /api/task` then polls of
 * `GET /api/task/:id`.
 */
async function fetchFallback(command: string, args?: Record<string, unknown>): Promise<unknown> {
  const base = apiBase();
  switch (command) {
    case 'check_health': {
      try {
        const r = await fetch(`${base}/api/health`);
        return r.ok;
      } catch {
        return false;
      }
    }
    case 'list_tasks': {
      const r = await fetch(`${base}/api/tasks`, { headers: authHeaders() });
      if (!r.ok) throw new Error(`list_tasks: ${r.status}`);
      return r.json();
    }
    case 'send_message': {
      const body = {
        task: args?.content ?? '',
        workflow: args?.workflow ?? 'prescriptive',
        project_path: args?.projectPath ?? args?.project_path ?? null,
      };
      const submit = await fetch(`${base}/api/task`, {
        method: 'POST',
        headers: { 'Content-Type': 'application/json', ...authHeaders() },
        body: JSON.stringify(body),
      });
      if (!submit.ok) {
        const err = `send_message submit failed: ${submit.status}`;
        emitWeb('task-error', { task_id: '', error: err });
        throw new Error(err);
      }
      const { id } = (await submit.json()) as { id: string };
      // Browser fallback: drive ChatView's listeners by emitting progress
      // events on the in-process EventBus. We mirror the Tauri command's
      // event shape (`task-progress`, `task-complete`, `task-error`) so
      // ChatView code is identical in both modes.
      emitWeb('task-progress', {
        task_id: id,
        message: 'Task submitted, waiting for result…',
      });
      const startMs = Date.now();
      const deadline = startMs + 10 * 60 * 1000;
      while (Date.now() < deadline) {
        await new Promise((res) => setTimeout(res, 1500));
        const r = await fetch(`${base}/api/task/${id}`, { headers: authHeaders() });
        if (!r.ok) {
          const err = `poll failed: ${r.status}`;
          emitWeb('task-error', { task_id: id, error: err });
          throw new Error(err);
        }
        const resp = await r.json();
        if (resp.status && resp.status !== 'running') {
          emitWeb('task-complete', {
            id,
            narrative: resp.narrative ?? JSON.stringify(resp),
            status: resp.status,
          });
          return resp.narrative ?? JSON.stringify(resp);
        }
        const elapsedS = Math.round((Date.now() - startMs) / 1000);
        emitWeb('task-progress', {
          task_id: id,
          message: `Running… (${elapsedS}s)`,
        });
      }
      const timeoutErr = 'send_message timed out after 10m';
      emitWeb('task-error', { task_id: id, error: timeoutErr });
      throw new Error(timeoutErr);
    }
    case 'ensure_api_server': {
      // In browser mode the API server is already running externally.
      return null;
    }
    default:
      throw new Error(`Unknown command in browser transport: ${command}`);
  }
}

/**
 * Why: Single entry point so components can call backend commands without
 * knowing whether they are running under Tauri or the browser. This is the
 * drop-in replacement for `@tauri-apps/api/core#invoke` used everywhere in the
 * UI.
 * What: Dispatches to `tauriInvoke` inside Tauri; otherwise maps to the REST
 * equivalent in `fetchFallback`.
 * Test: Stub `window.__TAURI_INTERNALS__` and confirm `invoke('check_health')`
 * routes through Tauri; remove it and confirm it routes through `fetch`.
 */
export async function invoke<T = unknown>(
  command: string,
  args?: Record<string, unknown>,
): Promise<T> {
  if (isTauri) {
    return (await tauriInvoke(command, args)) as T;
  }
  return (await fetchFallback(command, args)) as T;
}

export function isDesktop(): boolean {
  return isTauri;
}

// Event listening: in Tauri we proxy to `@tauri-apps/api/event.listen`; in the
// browser fallback we no-op because the REST path returns the final result
// synchronously from `send_message`.
export type UnlistenFn = () => void;

/**
 * Why: ChatView needs to subscribe to backend progress/complete/error events
 * regardless of transport. In browser mode the polling loop in
 * `fetchFallback` publishes events on `webBus` so the same listener wiring
 * works for both transports — no ChatView changes required.
 * What: Thin wrapper over Tauri's `listen<T>()` in desktop mode; subscribes
 * to the in-process `webBus` EventTarget in browser mode.
 * Test: In Tauri, emit `task-progress` from Rust and assert the callback
 * fires; in browser, call `listenEvent('task-progress', cb)` then
 * `emitWeb('task-progress', {...})` and assert `cb` ran with the payload.
 */
export async function listenEvent<T>(
  event: string,
  handler: (payload: T) => void,
): Promise<UnlistenFn> {
  if (isTauri) {
    const { listen } = await import('@tauri-apps/api/event');
    const unlisten = await listen<T>(event, (e) => handler(e.payload as T));
    return unlisten;
  }
  // Web mode: subscribe on the in-process EventBus.
  const listener = (e: Event) => handler((e as CustomEvent<T>).detail);
  webBus.addEventListener(event, listener);
  return () => webBus.removeEventListener(event, listener);
}

/**
 * Why: #192 Phase B replaces 2-second polling of `/api/tasks` with a
 * persistent Server-Sent Events stream from the Rust API server. Browsers
 * reconnect dropped EventSources automatically, so we get free resilience —
 * we just need a small wrapper to route incoming `event:` payloads to a
 * typed callback and surface errors to the caller for UI status feedback.
 * What: Opens an `EventSource` against `/api/events` (optionally filtered by
 * `session_id`), parses every `event:`-named SSE message as JSON `AppEvent`,
 * and invokes `onEvent`. `onError` fires on transport errors; the browser
 * keeps trying to reconnect in the background. Returns the raw EventSource
 * so callers can `close()` on unmount.
 * Test: Run `cargo run -- --api --port 7654` then open the browser; observe
 * that submitted tasks emit `session_started`, `pm_thinking`, etc. without
 * any polling network activity.
 */
export interface AppEvent {
  type: string;
  session_id?: string;
  text?: string;
  agent?: string;
  phase?: string;
  status?: string;
  tool?: string;
  preview?: string;
  task_preview?: string;
  project?: string;
  error?: string;
  [key: string]: unknown;
}

export function connectEventSource(
  sessionId?: string,
  onEvent?: (event: AppEvent) => void,
  onError?: (e: Event) => void,
): EventSource {
  const base = apiBase();
  const url = sessionId
    ? `${base}/api/events?session_id=${encodeURIComponent(sessionId)}`
    : `${base}/api/events`;
  const es = new EventSource(url);
  es.addEventListener('event', (e: MessageEvent) => {
    if (!onEvent) return;
    try {
      const parsed = JSON.parse(e.data) as AppEvent;
      onEvent(parsed);
    } catch {
      // Malformed payload — ignore. The server normally emits valid JSON;
      // a bad line is most likely a transient proxy mangling that fixes
      // itself on the next event.
    }
  });
  // The server also sends `event: ping` keepalives and `event: lag` notices;
  // both are diagnostic-only so we wire them through `onEvent` with their
  // raw type tag for callers that want to react.
  es.addEventListener('ping', () => {
    onEvent?.({ type: 'ping' });
  });
  es.addEventListener('lag', (e: MessageEvent) => {
    if (!onEvent) return;
    try {
      const parsed = JSON.parse(e.data) as { skipped?: number };
      onEvent({ type: 'lag', skipped: parsed.skipped });
    } catch {
      // ignore
    }
  });
  if (onError) es.onerror = onError;
  return es;
}
