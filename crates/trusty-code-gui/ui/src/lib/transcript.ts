// Why: `WorkstreamActivity.svelte` (DOC-39 §4.6, the 8b Phase-1 card, refs
// #2983) needs two pure, testable pieces of derivation over
// `GET /sessions/{id}/transcript` and `Session.created_at`: which turns to
// render as the chat stream, and a human-readable elapsed duration. Both are
// pure functions of their inputs (no `Date.now()` calls inside, no DOM) so
// they're unit-testable without mounting the component — mirrors the split
// already established between `session-status.ts` (`pickActiveSession`) and
// its owning component.
// What: [`TurnRecord`]/[`TranscriptRecord`] mirror
// `crate::run_task::recorder::TurnRecord` and
// `crate::session::transcript::TranscriptRecord` (wire fields only — the
// nested `usage: TokenUsage` is typed `unknown` because nothing here reads
// it). [`selectChatEntries`] returns EVERY turn (issue #3446 — the pane is
// now a chat stream, not a bounded 5-line "recent activity" card, so there
// is no tail to select and no truncation: the operator reading a chat
// stream wants the actual message, not a 160-char preview) reduced to a
// small renderable shape; a tool-only turn (`text === ''`) renders "ran:
// toolA, toolB" rather than a blank line. [`formatElapsed`] takes `now` as
// an explicit parameter (rather than reading `Date.now()` internally)
// precisely so it stays deterministic and testable.
//
// (Issue #3446 — carried over from the prior `selectTranscriptTail`/
// `DEFAULT_TAIL_LENGTH`/`PREVIEW_MAX_CHARS`/`truncate`, all removed: the
// bounded, truncated "recent activity" card this module served is gone now
// that `WorkstreamActivity.svelte` fills the whole pane as the chat-shaped
// TUI-style stream Bob asked for — "the stream builds up." A bounded/
// truncated helper with no caller left in the app would be dead code; this
// module now has exactly the one shape its one caller needs.)
// Test: `transcript.test.ts`.

/** Mirrors `crate::run_task::recorder::TurnRecord` (wire fields only). */
export interface TurnRecord {
  role: string;
  model: string;
  text: string;
  tool_calls: string[];
  ran_test_command: boolean;
  usage: unknown;
}

/** Mirrors `crate::session::transcript::TranscriptRecord` (wire fields
 * only — `usage`/`goals` are typed `unknown`/`unknown[]` since
 * `WorkstreamActivity` doesn't render them). */
export interface TranscriptRecord {
  session_id: string;
  turns: TurnRecord[];
  usage: unknown;
  cost_usd: number | null;
  mode: string | null;
  compaction_events: number;
  goals: unknown[];
}

/** A `TurnRecord` reduced to what the chat stream actually renders — full
 * text, never truncated (issue #3446). */
export interface ChatEntry {
  agent: string;
  text: string;
}

/**
 * Build the renderable chat entry for one turn.
 *
 * Why: a tool-only turn (`text === ''`) is common — the assistant only
 * called tools and produced no prose that turn — and rendering an empty
 * string there would look like a bug, not a designed state. `tool_calls`
 * gives an honest substitute.
 * What: non-empty `text` -> the text VERBATIM (no truncation — issue #3446
 * drops the prior 160-char preview cap now that this is a full chat stream,
 * not a compact card); empty `text` with tool calls -> `"ran: toolA,
 * toolB"`; empty `text` with no tool calls -> a labeled `"(no output)"`
 * placeholder (turn recorded, nothing to show).
 * Test: `transcript.test.ts::tool-only-turn-renders-tool-names`,
 * `::empty-turn-with-no-tool-calls`, `::preserves-full-text-untruncated`.
 */
function turnToChatEntry(turn: TurnRecord): ChatEntry {
  let text: string;
  if (turn.text.length > 0) {
    text = turn.text;
  } else if (turn.tool_calls.length > 0) {
    text = `ran: ${turn.tool_calls.join(', ')}`;
  } else {
    text = '(no output)';
  }
  return { agent: turn.role, text };
}

/**
 * Select every turn for the chat stream, oldest first.
 *
 * Why: issue #3446 — the pane IS the stream now ("chat at the bottom, the
 * stream builds up"), so there is no bound to apply; a bounded/bandwidth
 * concern for a very long-running workstream is a legitimate follow-up
 * (virtualized scrolling), not something this ticket's "match the intent"
 * scope needs to solve.
 * What: returns `turns` mapped through [`turnToChatEntry`], in the SAME
 * order the wire array already carries (oldest first — `formatElapsed`'s
 * caller and the DOM render top-to-bottom, so oldest-first reads as
 * newest-at-the-bottom, matching Bob's "the stream builds up" framing). An
 * empty `turns` array (a session that has never run, or the transcript
 * hasn't started yet) returns `[]` — a valid, empty stream, not an error.
 * Test: `transcript.test.ts::empty-turns-returns-empty-stream`,
 * `::preserves-turn-order`.
 */
export function selectChatEntries(turns: TurnRecord[]): ChatEntry[] {
  return turns.map(turnToChatEntry);
}

/**
 * Format an elapsed duration as a short, human string.
 *
 * Why: `Session.created_at` is the only timestamp the REST wire shape
 * offers (no per-turn timestamps — see `TurnRecord`'s doc comment); the
 * monitor card ticks this locally once a second purely for redisplay
 * (never for network polling), so the function takes `now` as an explicit
 * parameter rather than calling `Date.now()` itself, keeping it
 * deterministic and unit-testable.
 * What: scheme is `"{s}s"` under a minute, `"{m}m {ss}s"` (seconds
 * zero-padded to 2 digits) under an hour, `"{h}h {mm}m"` (minutes
 * zero-padded to 2 digits, seconds dropped) at an hour or beyond — matches
 * the worked examples `"3s"` / `"4m 12s"` / `"1h 02m"`. Negative deltas
 * (clock skew, or `now` sampled before `created_at` parses) clamp to `0`
 * rather than rendering a negative duration.
 * Test: `transcript.test.ts::elapsed-*` — 0s, sub-minute, multi-minute,
 * multi-hour, and the exact-boundary cases (60s, 3600s).
 */
export function formatElapsed(createdAt: string, now: number): string {
  const created = Date.parse(createdAt);
  const deltaMs = Math.max(0, now - created);
  const totalSeconds = Math.floor(deltaMs / 1000);

  const hours = Math.floor(totalSeconds / 3600);
  const minutes = Math.floor((totalSeconds % 3600) / 60);
  const seconds = totalSeconds % 60;

  if (hours > 0) {
    return `${hours}h ${String(minutes).padStart(2, '0')}m`;
  }
  if (minutes > 0) {
    return `${minutes}m ${String(seconds).padStart(2, '0')}s`;
  }
  return `${seconds}s`;
}

// ---------------------------------------------------------------------------
// Markdown transcript export (issue #3526)
//
// Why: a workstream that ran 48 min to DEADLINE_EXCEEDED with a runaway loop
// left no way to pull the full transcript out for inspection. The DAEMON now
// renders the transcript as Markdown at `GET /sessions/{id}/transcript.md`
// (`crate::serve::rest::sessions::render_transcript_markdown`) — the single
// source of truth for the format, so the same bytes a developer `curl`s in
// local dev are what the GUI's "Download transcript" button saves. This module
// therefore holds only the ONE pure, GUI-side piece the daemon can't own: the
// timestamped download filename (the browser's local clock, not the daemon's).
// The serialization itself is deliberately NOT duplicated here — a second TS
// serializer would drift from the Rust one.
// Test: `transcript.test.ts`.

/** Zero-pad a positive integer to two digits (local helper for the
 * `YYYYMMDD-HHMMSS` filename stamp). */
function pad2(n: number): string {
  return String(n).padStart(2, '0');
}

/**
 * Build the download filename: `transcript-<workstream>-<YYYYMMDD-HHMMSS>.md`.
 *
 * Why: the timestamp is derived from the passed-in export instant (never
 * hardcoded), in LOCAL time so the operator who clicked "download" recognizes
 * the moment. The workstream id is sanitized to `[A-Za-z0-9._-]` (collapsing
 * any run of other characters to a single `-`) so the value is always a safe,
 * predictable filename regardless of how the id is shaped.
 * What: pure function of `(workstreamId, generatedAtMs)`.
 * Test: `transcript.test.ts::transcriptFilename-*`.
 */
export function transcriptFilename(workstreamId: string, generatedAtMs: number): string {
  const d = new Date(generatedAtMs);
  const stamp =
    `${d.getFullYear()}${pad2(d.getMonth() + 1)}${pad2(d.getDate())}` +
    `-${pad2(d.getHours())}${pad2(d.getMinutes())}${pad2(d.getSeconds())}`;
  const safeId = workstreamId.replace(/[^A-Za-z0-9._-]+/g, '-').replace(/^-+|-+$/g, '');
  const idPart = safeId.length > 0 ? safeId : 'workstream';
  return `transcript-${idPart}-${stamp}.md`;
}

// ---------------------------------------------------------------------------
// Live SSE delta stream (tcode streaming epic #3696, Slice 3)
//
// Why: Slice 0 (`crate::events::Event::AgentMessageDelta`) added the wire
// event this module's `TurnRecord`/`selectChatEntries` pipeline cannot yet
// consume — that pipeline only ever sees a turn once it's fully complete,
// batch-replaced every `POLL_MS` (`WorkstreamActivity.svelte`'s
// `GET /sessions/{id}/transcript` poll). This section adds the pure,
// unit-testable reducer side of Slice 3: given the deltas arriving over
// `GET /sessions/{id}/events` (SSE), fold them into in-progress "bubbles" the
// component can render alongside `chatEntries` while a turn is still
// streaming. Kept here (not in the component) for the same reason
// `selectChatEntries`/`formatElapsed` are — pure functions of their inputs,
// no `EventSource`/DOM/`Date.now()` inside, so the folding logic is testable
// without mounting anything.
// What: [`AgentMessageDeltaEvent`] mirrors
// `crate::events::Event::AgentMessageDelta`'s wire fields PLUS the owning
// `SessionEventEnvelope`'s `seq` (the component flattens envelope + event
// into this one shape before calling [`applyDelta`], since `seq` — not
// anything on the event payload itself — is what orders bubbles and lets a
// reconnect dedup against ring-buffer replay). [`StreamBubble`] is the
// per-`(agent_id, turn_id)` accumulator; [`applyDelta`] is the reducer.
// Test: `transcript.test.ts` (`applyDelta` describe block).

/** Mirrors `crate::events::Event::AgentMessageDelta`'s wire fields, plus the
 * owning `SessionEventEnvelope.seq` (see the section Why above for why the
 * two are flattened into one shape here rather than kept as a nested
 * envelope/event pair). */
export interface AgentMessageDeltaEvent {
  session_id: string;
  agent: string;
  agent_id: string;
  turn_id: string;
  delta: string;
  done: boolean;
  seq: number;
}

/** One in-progress (or just-completed) streamed turn, keyed by the tuple
 * `(agentId, turnId)` — never `turnId` alone, since two concurrently
 * delegated sub-agents can share a `turn_id` counter value (the Slice 0
 * contract rule documented on `Event::AgentMessageDelta`). `seq` is the
 * envelope `seq` of the FIRST delta seen for this key, used only to order
 * bubbles relative to each other; it does not change as later deltas for the
 * same key arrive. */
export interface StreamBubble {
  agentId: string;
  turnId: string;
  agent: string;
  text: string;
  done: boolean;
  seq: number;
}

/**
 * Fold one `agent_message_delta` event into a bubble list.
 *
 * Why: the reducer, not the component, owns the tuple-keying rule — see
 * `StreamBubble`'s doc for why `(agent_id, turn_id)` and never `turn_id`
 * alone. Pure and immutable (returns a new array, never mutates `bubbles`)
 * so the component can call it directly from an `EventSource.onmessage`
 * handler and reassign `$state` in one step, and so it's callable from a
 * test with hand-built inputs, no mounting required.
 * What: no existing bubble for `(delta.agent_id, delta.turn_id)` -> appends a
 * new one seeded with `delta.text`/`delta.done`/`delta.seq`. An existing
 * bubble -> returns a copy with `delta.delta` appended to `text` and `done`
 * OR'd in (once a key's bubble is marked done it stays done, even if a
 * malformed producer somehow sent another non-done delta for the same key
 * afterward). The returned array is always sorted ascending by each bubble's
 * (first-seen) `seq`, so two bubbles built from deltas delivered out of
 * arrival order still render in the right order — the arrival order an
 * `EventSource` guarantees in practice, but the sort keeps the function's
 * output correct even given adversarial/test input order.
 * Test: `transcript.test.ts::applyDelta-*`.
 */
export function applyDelta(bubbles: StreamBubble[], delta: AgentMessageDeltaEvent): StreamBubble[] {
  const idx = bubbles.findIndex((b) => b.agentId === delta.agent_id && b.turnId === delta.turn_id);
  let next: StreamBubble[];
  if (idx === -1) {
    next = [
      ...bubbles,
      {
        agentId: delta.agent_id,
        turnId: delta.turn_id,
        agent: delta.agent,
        text: delta.delta,
        done: delta.done,
        seq: delta.seq,
      },
    ];
  } else {
    const existing = bubbles[idx];
    const updated: StreamBubble = {
      ...existing,
      text: existing.text + delta.delta,
      done: existing.done || delta.done,
    };
    next = bubbles.slice();
    next[idx] = updated;
  }
  return next.sort((a, b) => a.seq - b.seq);
}
