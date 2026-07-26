// The browser-mode SSE→web-bus bridge (#192 Phase B), extracted verbatim from
// `App.svelte` so it can be unit-tested (#3759).
//
// Why: Every typed server `AppEvent` arriving on `/api/events` has to be
// routed onto the legacy web-bus names (`task-progress` / `task-complete` /
// `task-error` / `task-delta`) that `ChatView` / `TaskHistory` / `InputArea`
// already listen on, plus fanned out to the structured sinks (`workflowState`,
// the Events-tab log, the Slack mirror, the recap store). While this lived
// inline in `App.svelte` it had ZERO automated coverage — every doc comment
// around it said "Test: Manual" — which is how #3759 (the `session_done`
// empty-narrative race) shipped and survived. Living in `src/lib` next to the
// other pure-ish reducers (`events.ts`, `slack-mirror.ts`, `chatStream.ts`)
// means the whole routing table is now vitest-reachable without mounting the
// app shell. Behavior is unchanged by the move itself.
// What: `bridgeEventToWebBus(ev)` — the single entry point `App.svelte`'s
// `connectEventSource` callback calls for each incoming event. It is
// side-effecting by design (it writes to the same stores the app renders
// from); tests drive it and observe those stores plus the web bus.
// This bridge runs in BROWSER MODE ONLY — `App.svelte`'s `startEventStream()`
// early-returns under Tauri, where the desktop shell has its own `listen()`
// bridge.
// Test: `eventBridge.test.ts`.

import { pushSlackEvent } from './slack-mirror';
import { pushEvent } from './events';
import { emitWebEvent, type AppEvent } from './transport';
import { bridgeDelta } from './chatStream';
import { addMessage } from '../stores/app';
import { setRecap, type Recap } from '../stores/recap';
import { handleWorkflowEvent } from '../stores/workflow';

/**
 * Why: When a typed server event arrives, route it onto the legacy webBus
 * names so ChatView / TaskHistory / Sidebar pick it up without rewriting
 * each component's listener wiring. Phase B keeps the migration mechanical
 * — Phase C will surface the new event vocabulary directly to components
 * that can render richer state (per-phase timeline, per-agent output).
 * What: Maps a small set of high-signal server events to the existing
 * `task-progress` / `task-complete` / `task-error` web-bus events. Unknown
 * event types are forwarded as `task-progress` with a friendly summary so
 * they show up in the UI rather than being silently dropped.
 * Test: `bridge_session_done_emits_no_task_complete`,
 * `bridge_routes_known_events`.
 */
export function bridgeEventToWebBus(ev: AppEvent) {
  // #3819: feed the Events tab's rolling log for EVERY event, including
  // ping/lag — additive, never short-circuits anything below.
  pushEvent(ev);
  // #3217: feed the structured workflow store first — additive, never
  // short-circuits the flattened-text switch below.
  handleWorkflowEvent(ev);
  switch (ev.type) {
    case 'session_started':
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: 'Task started…',
      });
      break;
    case 'session_done':
      // #3759: deliberately emits NOTHING on the web bus.
      //
      // This arm used to emit `task-complete` with a hardcoded
      // `narrative: ''`. `task-complete` is the narrative-BEARING result
      // event, and `session_done` structurally cannot carry a narrative — the
      // SSE lifecycle event has no result text on the wire. So the fabricated
      // empty string raced the poll-driven `task-complete` in
      // `transport.ts::send_message` (the emitter that carries the REAL text):
      // whichever landed last won in `ChatView`, and with token streaming
      // (#3753) `session_done` normally lands FIRST — up to one poll interval
      // (250–500ms) ahead of the real narrative — blanking freshly streamed
      // text to "(no narrative)".
      //
      // Nothing is lost by staying silent here. The status bookkeeping this
      // arm appeared to do already happens ABOVE, unconditionally and with
      // better fidelity: `handleWorkflowEvent` maps `session_done` to
      // `workflowState.status` (and filters foreign sessions while doing so),
      // and `pushEvent` logs it to the Events tab. The narrative, the
      // `isRunning` flip, and the `TaskHistory` refresh all still arrive with
      // the poll-driven `task-complete` for this tab's own task — which, being
      // per-task rather than a process-wide broadcast, also stops a FOREIGN
      // session's completion (a Telegram task finishing mid-turn) from
      // clearing this tab's spinner.
      //
      // The `break` is load-bearing: without an explicit arm, `session_done`
      // would fall through to `default:` and emit a `[session_done]`
      // `task-progress` instead.
      break;
    case 'session_cancelled':
      emitWebEvent('task-error', {
        task_id: ev.session_id ?? '',
        error: 'cancelled',
      });
      break;
    case 'pm_thinking':
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: ev.text ?? '(thinking)',
      });
      break;
    case 'pm_delegating':
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: `Delegating to ${ev.agent}: ${ev.task_preview ?? ''}`,
      });
      break;
    case 'agent_spawned':
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: `Agent ${ev.agent} starting…`,
      });
      break;
    case 'agent_message':
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: `[${ev.agent}] ${ev.text ?? ''}`,
      });
      break;
    case 'agent_message_delta': {
      // Token-level streaming: forward each coalesced fragment on the
      // dedicated `task-delta` bus so ChatView can grow the in-flight reply
      // bubble incrementally (see `lib/chatStream.ts`). The final
      // `task-complete` still replaces the accumulation with the
      // authoritative narrative.
      const delta = bridgeDelta(ev);
      if (delta) emitWebEvent('task-delta', delta);
      break;
    }
    case 'agent_done':
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: `Agent ${ev.agent} done (${ev.status ?? 'ok'})`,
      });
      break;
    case 'agent_failed':
      emitWebEvent('task-error', {
        task_id: ev.session_id ?? '',
        error: `Agent ${ev.agent} failed: ${ev.error ?? ''}`,
      });
      break;
    case 'tool_called':
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: `Tool ${ev.tool}: ${ev.preview ?? ''}`,
      });
      break;
    case 'phase_started':
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: `Phase: ${ev.phase}`,
      });
      break;
    case 'phase_done':
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: `Phase ${ev.phase} ${ev.status ?? 'done'}`,
      });
      break;
    case 'recap_generated': {
      // Why: #371 — surface session recaps in two places: the persistent
      // RecapPanel between chat and input (latest recap per session) and as
      // a banner-style chat message so the recap is preserved in the
      // scrollback. Both consumers read from the recap store; the chat
      // banner is appended via addMessage with role='recap'.
      const sessionId = (ev.session_id ?? '') as string;
      const summary = ((ev as Record<string, unknown>).summary ?? '') as string;
      const rawRows = (ev as Record<string, unknown>).table_rows as unknown;
      const rows: [string, string][] = Array.isArray(rawRows)
        ? (rawRows as unknown[])
            .filter((r): r is [unknown, unknown] => Array.isArray(r) && r.length === 2)
            .map(([s, r]) => [String(s), String(r)])
        : [];
      const recap: Recap = {
        session_id: sessionId,
        summary,
        table_rows: rows,
        received_at: Date.now(),
      };
      setRecap(recap);
      if (sessionId) {
        addMessage(sessionId, {
          id: `recap-${sessionId}-${recap.received_at}`,
          role: 'recap',
          content: summary,
          timestamp: recap.received_at,
          recapRows: rows,
        });
      }
      break;
    }
    case 'slack_message_received':
    case 'slack_reply_sent':
      // #3752: live Slack conversation mirror. These are conversation-scoped
      // (no session_id), so they don't belong on the task web-bus — fold them
      // straight into the SlackMirror store and stop (no default fall-through
      // to a `task-progress` emit).
      pushSlackEvent(ev);
      break;
    case 'ping':
    case 'lag':
      // Diagnostic — no UI action; consumers that care can listen for the
      // typed AppEvent directly via `connectEventSource`.
      break;
    default:
      // Forward unknown events as progress so they're not invisible.
      emitWebEvent('task-progress', {
        task_id: ev.session_id ?? '',
        message: `[${ev.type}]`,
      });
  }
}
