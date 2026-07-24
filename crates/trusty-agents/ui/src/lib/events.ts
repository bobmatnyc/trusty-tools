// Pure, framework-free reducer + store for the EVENTS tab (#3819, epic
// #3052) — a filterable view of incoming events.
//
// Why: Bob's nav reshape replaces the Personality tab with EVENTS: "a
// FILTERABLE view of INCOMING EVENTS." The eventstream-processing epic
// (#3798, children #3799-#3803) owns the real connector/routing backend and
// is largely NOT implemented yet, so this slice is frontend-only: it taps
// the SAME `AppEvent` stream `App.svelte`'s `bridgeEventToWebBus` already
// receives (from `/api/events` SSE in browser mode, the Tauri `listen()`
// bridge in desktop mode) and folds every event into a bounded rolling log,
// mirroring `slack-mirror.ts`'s pure-reducer pattern exactly (a pure
// event→row mapper + a bounded writable store) so the transform is
// vitest-testable without mounting Svelte or a DOM, and `EventsView.svelte`
// stays a thin filter/render layer over it.
// What: `eventRowFromEvent` maps one `AppEvent` to an `EventRow`;
// `pushEvent` folds it into the bounded `eventLog` store. `EVENT_SOURCES`
// classifies a row's `type` into a coarse source bucket for the source
// filter (`task`, `slack`, `system`, `gmail`) — `gmail` is the first real
// per-connector source, landing with #3820's Gmail listener
// (`Event::ListenerEventReceived`, wire type `listener_event_received`,
// published by `crate::listeners::poll` onto the SAME bus this file
// already taps — no new plumbing needed on this side).
// Test: `events.test.ts`.

import { writable } from 'svelte/store';
import type { AppEvent } from './transport';

/** One rendered row in the Events tab's filterable list. */
export interface EventRow {
  /** The raw `AppEvent.type` (`session_started`, `slack_message_received`, …). */
  type: string;
  /** Coarse bucket for the source filter — see `sourceOf`. */
  source: 'task' | 'slack' | 'system' | 'gmail';
  /** A short, human-glanceable summary line — never empty. */
  summary: string;
  /** Client receive time (ms) — used for ordering + the time filter. */
  received_at: number;
}

/** Cap on retained rows so a long-lived session can't grow unbounded. */
export const EVENT_LOG_MAX = 300;

/**
 * Why: classifies an event's `type` into the source filter's buckets.
 * `slack`, `gmail`, and `ping`/`lag` (diagnostic) are their own honest
 * categories; everything else — task/agent/workflow lifecycle events — is
 * `task`, the bucket that will keep splitting into per-connector sources as
 * more of #3798's listener family lands (Calendar next).
 * What: Pure string-prefix classification. `listener_event_received` is
 * NOT prefix-classified by type alone (the wire type is the same for every
 * connector) — `eventRowFromEvent` reads the event's own `provider` field
 * for that one case; see its `case` arm below.
 * Test: `sourceOf_classifies_*`.
 */
export function sourceOf(type: string): EventRow['source'] {
  if (type.startsWith('slack_')) return 'slack';
  if (type === 'ping' || type === 'lag') return 'system';
  return 'task';
}

/**
 * Why: A raw `AppEvent` has too many optional, type-specific fields to
 * render directly — this picks the ONE most relevant string per event type
 * so every row reads as a single glanceable line, mirroring
 * `slack-mirror.ts::slackBubbleFromEvent`'s per-type field selection.
 * What: Never returns an empty summary — falls back to the bare event type
 * when no more specific field is present.
 * Test: `eventRowFromEvent_summarizes_known_types`,
 * `eventRowFromEvent_falls_back_to_type_for_unknown_events`.
 */
export function eventRowFromEvent(ev: AppEvent): EventRow {
  const str = (v: unknown): string => (typeof v === 'string' ? v : '');
  // `listener_event_received` (#3820) carries its own source bucket via
  // `provider` (`"gmail"` today) rather than a `type`-prefix rule, since the
  // wire `type` is the same string for every connector. Handled before the
  // switch below so it can `return` its own `source` instead of always
  // falling through to `sourceOf`.
  if (ev.type === 'listener_event_received') {
    const provider = str(ev.provider);
    return {
      type: ev.type,
      source: provider === 'gmail' ? 'gmail' : 'task',
      summary: str(ev.summary) || ev.type,
      received_at: Date.now(),
    };
  }
  let summary = '';
  switch (ev.type) {
    case 'pm_thinking':
    case 'agent_message':
      summary = str(ev.text);
      break;
    case 'pm_delegating':
      summary = `delegating to ${str(ev.agent)}: ${str(ev.task_preview)}`;
      break;
    case 'agent_spawned':
      summary = `${str(ev.agent)} starting`;
      break;
    case 'agent_done':
      summary = `${str(ev.agent)} done (${str(ev.status)})`;
      break;
    case 'agent_failed':
      summary = `${str(ev.agent)} failed: ${str(ev.error)}`;
      break;
    case 'tool_called':
      summary = `${str(ev.tool)}: ${str(ev.preview)}`;
      break;
    case 'phase_started':
      summary = `phase started: ${str(ev.phase)}`;
      break;
    case 'phase_done':
      summary = `phase ${str(ev.phase)} ${str(ev.status)}`;
      break;
    case 'slack_message_received':
      summary = `#${str(ev.channel)}: ${str(ev.text)}`;
      break;
    case 'slack_reply_sent':
      summary = `reply → #${str(ev.channel)}: ${str(ev.text)}`;
      break;
    case 'session_started':
      summary = 'session started';
      break;
    case 'session_done':
      summary = `session done (${str(ev.status)})`;
      break;
    default:
      summary = '';
  }
  return {
    type: ev.type,
    source: sourceOf(ev.type),
    summary: summary || ev.type,
    received_at: Date.now(),
  };
}

/** The rolling event log, oldest first. */
export const eventLog = writable<EventRow[]>([]);

/**
 * Why: App.svelte's SSE/Tauri bridge calls this for every event alongside
 * its existing `bridgeEventToWebBus` routing — additive, never a
 * replacement (mirrors `pushSlackEvent`'s call-site contract).
 * What: Appends the mapped row, trimming to `EVENT_LOG_MAX`. `ping`/`lag`
 * ARE included (unlike the Slack mirror, which ignores them) — the Events
 * tab's `system` filter bucket exists specifically so a user can see (or
 * hide) the SSE heartbeat.
 * Test: `pushEvent_appends_and_trims`.
 */
export function pushEvent(ev: AppEvent): EventRow {
  const row = eventRowFromEvent(ev);
  eventLog.update((log) => {
    const next = [...log, row];
    return next.length > EVENT_LOG_MAX ? next.slice(next.length - EVENT_LOG_MAX) : next;
  });
  return row;
}

/** Reset the log (used by tests and a future "clear" affordance). */
export function clearEventLog(): void {
  eventLog.set([]);
}
