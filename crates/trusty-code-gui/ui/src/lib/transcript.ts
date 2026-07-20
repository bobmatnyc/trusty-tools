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
