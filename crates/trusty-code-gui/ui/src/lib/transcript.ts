// Why: `SessionMonitor.svelte` (DOC-39 §4.6, the 8b Phase-1 card, refs
// #2983) needs two pure, testable pieces of derivation over
// `GET /sessions/{id}/transcript` and `Session.created_at`: which turns to
// show in a bounded "recent activity" tail, and a human-readable elapsed
// duration. Both are pure functions of their inputs (no `Date.now()` calls
// inside, no DOM) so they're unit-testable without mounting the component —
// mirrors the split already established between `session-status.ts`
// (`pickActiveSession`) and its owning component.
// What: [`TurnRecord`]/[`TranscriptRecord`] mirror
// `crate::run_task::recorder::TurnRecord` and
// `crate::session::transcript::TranscriptRecord` (wire fields only — the
// nested `usage: TokenUsage` is typed `unknown` because nothing here reads
// it). [`selectTranscriptTail`] returns the last `n` turns (default 5 — see
// its own doc comment) reduced to a small renderable shape; a tool-only
// turn (`text === ''`) renders "ran: toolA, toolB" rather than a blank
// line. [`formatElapsed`] takes `now` as an explicit parameter (rather than
// reading `Date.now()` internally) precisely so it stays deterministic and
// testable.
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
 * `SessionMonitor` doesn't render them). */
export interface TranscriptRecord {
  session_id: string;
  turns: TurnRecord[];
  usage: unknown;
  cost_usd: number | null;
  mode: string | null;
  compaction_events: number;
  goals: unknown[];
}

/** A `TurnRecord` reduced to what the monitor card's tail actually
 * renders. */
export interface TranscriptTailEntry {
  agent: string;
  preview: string;
}

/**
 * How many trailing turns `selectTranscriptTail` returns by default.
 *
 * Why: the monitor card is a compact, fixed-height summary widget (not a
 * scrolling transcript viewer — that's a later slice), so the tail needs a
 * small bound. 5 is enough recent context to tell what the session is
 * currently doing without the card growing unboundedly as a session runs
 * for hundreds of turns.
 */
export const DEFAULT_TAIL_LENGTH = 5;

/**
 * Truncation length for a single turn's text preview.
 *
 * Why: mirrors the truncation convention documented on
 * `crate::events::preview` (used for `args_preview`/`result_preview` on
 * SSE event payloads) — a fixed character budget keeps one turn's preview
 * from dominating the card, applied client-side here since transcript
 * turns aren't pre-truncated on the wire the way event previews are.
 */
export const PREVIEW_MAX_CHARS = 160;

/**
 * Truncate `s` to at most `max` characters, appending a Unicode ellipsis
 * when truncation occurred.
 *
 * Why: same behavior as `crate::events::preview` (truncate, don't just cut
 * mid-word silently) reimplemented client-side for turn text, which the
 * daemon does not pre-truncate.
 * What: returns `s` unchanged when `s.length <= max`.
 * Test: `transcript.test.ts::truncate-*` (exercised indirectly via
 * `selectTranscriptTail`).
 */
function truncate(s: string, max: number): string {
  if (s.length <= max) return s;
  return `${s.slice(0, max)}…`;
}

/**
 * Build the small renderable preview for one turn.
 *
 * Why: a tool-only turn (`text === ''`) is common — the assistant only
 * called tools and produced no prose that turn — and rendering an empty
 * string there would look like a bug, not a designed state. `tool_calls`
 * gives an honest substitute.
 * What: non-empty `text` -> truncated prose; empty `text` with tool calls
 * -> `"ran: toolA, toolB"`; empty `text` with no tool calls -> a labeled
 * `"(no output)"` placeholder (turn recorded, nothing to show).
 * Test: `transcript.test.ts::tool-only-turn-renders-tool-names`,
 * `::empty-turn-with-no-tool-calls`.
 */
function turnToTailEntry(turn: TurnRecord): TranscriptTailEntry {
  let preview: string;
  if (turn.text.length > 0) {
    preview = truncate(turn.text, PREVIEW_MAX_CHARS);
  } else if (turn.tool_calls.length > 0) {
    preview = `ran: ${turn.tool_calls.join(', ')}`;
  } else {
    preview = '(no output)';
  }
  return { agent: turn.role, preview };
}

/**
 * Select the trailing `n` turns for the monitor card's "recent activity"
 * list, reduced to `TranscriptTailEntry`.
 *
 * Why: `SessionMonitor` renders a bounded, glanceable activity feed, not
 * the full transcript — this is the one derivation step between the raw
 * `TranscriptRecord.turns` array and that rendering.
 * What: returns `turns.slice(-n)` (so ordering is preserved: oldest of the
 * kept turns first, most recent last — the component renders top-to-bottom,
 * so the most recent turn lands where the eye naturally finishes reading),
 * mapped through `turnToTailEntry`. An empty `turns` array (a session that
 * has never run, or the transcript hasn't started yet) returns `[]` — a
 * valid, empty tail, not an error.
 * Test: `transcript.test.ts::empty-turns-returns-empty-tail`,
 * `::more-turns-than-n-returns-only-the-tail`.
 */
export function selectTranscriptTail(
  turns: TurnRecord[],
  n: number = DEFAULT_TAIL_LENGTH,
): TranscriptTailEntry[] {
  return turns.slice(-n).map(turnToTailEntry);
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
