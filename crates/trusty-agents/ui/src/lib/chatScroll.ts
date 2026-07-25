// Chat scroll-anchoring predicate (PR #3895 code-critic HIGH-3).
//
// Why: `ChatView` used to force `scrollTop = scrollHeight` after EVERY render.
// That is wrong twice over: it yanks a user who scrolled up to read history
// back to the newest message the moment a streaming delta lands, and — since
// the chat keeps rendering while the agent-config takeover covers it — it
// silently resets the covered chat's scroll offset, contradicting the whole
// "leaving config returns to chat exactly as it was" contract. The
// generally-correct chat behavior is to auto-scroll only while the reader is
// already parked at the bottom, which is what this predicate decides.
// What: A pure metrics check, so the rule is unit-testable without a layout
// engine (jsdom has none). `epsilon` absorbs sub-pixel/fractional scroll
// offsets and the last line's bottom padding — a reader within a few dozen
// pixels of the end is "at the bottom" as far as intent goes.
// Test: `chatScroll.test.ts`.

/** The metrics subset of a scroll container this predicate needs. */
export interface ScrollMetrics {
  scrollTop: number;
  scrollHeight: number;
  clientHeight: number;
}

/** Default slack, in CSS pixels, for "close enough to the bottom". */
export const BOTTOM_EPSILON_PX = 32;

export function isPinnedToBottom(
  metrics: ScrollMetrics,
  epsilon: number = BOTTOM_EPSILON_PX,
): boolean {
  const distanceFromBottom = metrics.scrollHeight - metrics.scrollTop - metrics.clientHeight;
  return distanceFromBottom <= epsilon;
}
