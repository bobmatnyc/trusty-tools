// Token-streaming glue for the chat GUI.
//
// Why: The Rust backend now publishes `agent_message_delta` events on the
// `/api/events` SSE stream — one per coalesced batch of tokens as a
// conversational reply is produced, then a terminal `done: true` marker. The
// browser must (a) recognize that event shape and translate it onto the
// existing web-bus (`bridgeDelta`), and (b) accumulate the fragments into the
// in-flight reply bubble WITHOUT double-rendering when the authoritative
// `task-complete` result finally lands (`StreamAccumulator`). Both concerns
// are pure and live here so they can be unit-tested without mounting the
// Svelte component or a live EventSource.
// What: `bridgeDelta` maps an `AppEvent` of type `agent_message_delta` to the
// `task-delta` web-bus payload (or `null` for any other event). `StreamAccumulator`
// tracks per-task buffered text and a "is this task streaming?" flag so the
// component can (1) append each fragment, (2) suppress the polling loop's
// "Running…" progress ticks from clobbering the live text, and (3) drop its
// buffer on completion so the final `PmResponse` narrative replaces — not
// appends to — the accumulation.
// Test: `chatStream.test.ts`.

import type { AppEvent } from './transport';

/** Web-bus payload for an accumulated token batch. */
export interface DeltaPayload {
  /** Task/session id the delta belongs to (matches the poll loop's id). */
  task_id: string;
  /** Incremental text fragment (empty on the terminal `done` marker). */
  text: string;
  /** Speaker attribution (#3739); empty ⇒ keep the request-time stamp. */
  agent: string;
  /** True exactly once, last, when the stream has finished. */
  done: boolean;
}

/**
 * Why: `App.svelte`'s SSE bridge fans typed server events onto the legacy
 * web-bus. Centralizing the `agent_message_delta` shape check here keeps the
 * bridge's giant `switch` mechanical and makes the mapping unit-testable.
 * What: Returns a `DeltaPayload` when `ev` is an `agent_message_delta`,
 * defaulting missing fields (`text`/`agent` → `''`, `done` → `false`);
 * returns `null` for every other event type.
 * Test: `bridgeDelta_maps_agent_message_delta`, `bridgeDelta_ignores_others`.
 */
export function bridgeDelta(ev: AppEvent): DeltaPayload | null {
  if (ev.type !== 'agent_message_delta') return null;
  return {
    task_id: (ev.session_id as string) ?? '',
    text: (ev.text as string) ?? '',
    agent: (ev.agent as string) ?? '',
    done: (ev.done as boolean) ?? false,
  };
}

/**
 * Per-task token accumulator with dedupe support.
 *
 * Why: SSE deltas arrive incrementally while, in browser mode, the
 * `send_message` poll loop ALSO emits periodic `task-progress` ("Running…")
 * ticks and a final `task-complete` carrying the authoritative narrative. The
 * component needs to (1) grow the bubble from deltas, (2) ignore progress
 * ticks for a task that is actively streaming so they don't overwrite the live
 * text, and (3) forget the buffer once the task completes so the final
 * narrative replaces the accumulation instead of stacking on top of it.
 * What: A thin wrapper over a `Map<taskId, string>` plus the streaming-flag
 * query (a task is "streaming" iff it currently has a buffer).
 * Test: `StreamAccumulator_*`.
 */
export class StreamAccumulator {
  private buffers = new Map<string, string>();

  /**
   * Append a fragment and return the full accumulated text for the task.
   * Marks the task as streaming.
   */
  append(taskId: string, text: string): string {
    const next = (this.buffers.get(taskId) ?? '') + text;
    this.buffers.set(taskId, next);
    return next;
  }

  /** Full accumulated text so far, or `undefined` if the task never streamed. */
  get(taskId: string): string | undefined {
    return this.buffers.get(taskId);
  }

  /** Whether the task currently has streamed text (used to gate progress ticks). */
  isStreaming(taskId: string): boolean {
    return this.buffers.has(taskId);
  }

  /**
   * Forget a task's buffer (call on `task-complete`/`task-error`) so the
   * authoritative narrative replaces the accumulation and future progress
   * ticks are no longer suppressed. Returns whether anything was cleared.
   */
  finalize(taskId: string): boolean {
    return this.buffers.delete(taskId);
  }
}
