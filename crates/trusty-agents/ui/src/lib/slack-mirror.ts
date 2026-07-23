// Pure, framework-free reducer + store for the live Slack conversation mirror
// (#3752, epic #3052).
//
// Why: The GUI mirrors both sides of a live Slack conversation the CTO Bot is
// having — the inbound human message and the bot's reply — driven by the two
// `slack_message_received` / `slack_reply_sent` events that arrive over the
// existing `/api/events` SSE stream. Keeping the event→bubble transform as a
// pure function (mirroring lib/retask.ts) lets vitest verify rendering logic
// without mounting Svelte or a DOM; the Svelte component is then a thin view
// over the store.
// What: `slackBubbleFromEvent` maps one `AppEvent` to a `SlackBubble` (or
// `null` for unrelated events); `slackMirror` is the writable bubble log;
// `pushSlackEvent` folds an event into it (bounded). Badges are HONEST — the
// inbound badge is the resolved RBAC tier, the reply badge is the bot's own
// identity string as sent by the backend; this module never invents a label.
// Test: `slack-mirror.test.ts`.

import { writable } from 'svelte/store';
import type { AppEvent } from './transport';

/** One rendered line in the mirror pane. */
export interface SlackBubble {
  /** `inbound` = human message; `reply` = bot reply. Drives bubble side/color. */
  kind: 'inbound' | 'reply';
  /** Slack channel id the message belonged to. */
  channel: string;
  /** Message body (raw inbound text, or the posted reply text). */
  text: string;
  /**
   * Honest badge:
   *  - inbound → RBAC tier label (`all` / `analytics` / `read_only`)
   *  - reply   → bot identity (`CTO Bot (as itself)`)
   * Never a fabricated auth state.
   */
  badge: string;
  /** Display name of the human sender (inbound only; empty for replies). */
  speaker: string;
  /** Client receive time (ms) — used only for a stable list key + ordering. */
  received_at: number;
}

/** Cap on retained bubbles so a long-lived session can't grow unbounded. */
export const SLACK_MIRROR_MAX = 100;

/** Human-facing label for an inbound RBAC tier badge. Unknown → the raw value. */
export function tierBadgeLabel(tier: string): string {
  switch (tier) {
    case 'all':
      return 'ALL';
    case 'analytics':
      return 'ANALYTICS';
    case 'read_only':
      return 'READ-ONLY';
    default:
      return tier || 'UNKNOWN';
  }
}

/**
 * Why: The single place that decides whether an incoming SSE event is part of
 * the Slack mirror and, if so, what bubble it renders. Isolating it keeps the
 * transform testable and the component declarative.
 * What: Returns a `SlackBubble` for `slack_message_received` /
 * `slack_reply_sent`; returns `null` for every other event type so callers can
 * cheaply skip non-mirror traffic. Missing string fields degrade to empty
 * strings rather than throwing (the stream is best-effort telemetry).
 * Test: `slack-mirror.test.ts` — both kinds map correctly; other types → null.
 */
export function slackBubbleFromEvent(ev: AppEvent): SlackBubble | null {
  const str = (v: unknown): string => (typeof v === 'string' ? v : '');
  if (ev.type === 'slack_message_received') {
    return {
      kind: 'inbound',
      channel: str(ev.channel),
      text: str(ev.text),
      badge: tierBadgeLabel(str(ev.tier)),
      speaker: str(ev.user_display),
      received_at: Date.now(),
    };
  }
  if (ev.type === 'slack_reply_sent') {
    return {
      kind: 'reply',
      channel: str(ev.channel),
      text: str(ev.text),
      // Honest label straight from the backend — the bot speaks as itself.
      badge: str(ev.identity) || 'CTO Bot (as itself)',
      speaker: '',
      received_at: Date.now(),
    };
  }
  return null;
}

/** The rolling bubble log, oldest first. */
export const slackMirror = writable<SlackBubble[]>([]);

/**
 * Why: App.svelte's SSE bridge calls this for every event; a non-mirror event
 * is a cheap no-op so the call site doesn't need to pre-filter.
 * What: If `ev` maps to a bubble, appends it (trimming to `SLACK_MIRROR_MAX`).
 * Returns the bubble it pushed, or `null` when the event wasn't a mirror event
 * (useful for tests + callers that want to know whether anything happened).
 * Test: `slack-mirror.test.ts` — pushing both kinds grows the store in order;
 * unrelated events leave it unchanged; overflow trims the oldest.
 */
export function pushSlackEvent(ev: AppEvent): SlackBubble | null {
  const bubble = slackBubbleFromEvent(ev);
  if (!bubble) return null;
  slackMirror.update((log) => {
    const next = [...log, bubble];
    return next.length > SLACK_MIRROR_MAX ? next.slice(next.length - SLACK_MIRROR_MAX) : next;
  });
  return bubble;
}

/** Reset the mirror (used by tests and a future "clear" affordance). */
export function clearSlackMirror(): void {
  slackMirror.set([]);
}
