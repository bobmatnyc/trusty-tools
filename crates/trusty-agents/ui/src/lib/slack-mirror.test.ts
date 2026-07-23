// Vitest unit tests for the Slack conversation mirror reducer + store (#3752).
//
// Why: The mirror pane's correctness lives in the pure event→bubble transform
// and the bounded store fold, not in Svelte markup — so we test those directly
// (no DOM/jsdom needed), mirroring retask.test.ts. This asserts the pane would
// render both conversation sides from synthetic SSE events with honest badges.
// What: Covers `slackBubbleFromEvent`, `tierBadgeLabel`, and the `slackMirror`
// store via `pushSlackEvent` (append order, non-mirror no-op, overflow trim).

import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import type { AppEvent } from './transport';
import {
  slackBubbleFromEvent,
  tierBadgeLabel,
  pushSlackEvent,
  slackMirror,
  clearSlackMirror,
  SLACK_MIRROR_MAX,
} from './slack-mirror';

const inbound = (over: Partial<AppEvent> = {}): AppEvent => ({
  type: 'slack_message_received',
  channel: 'C0123ABC',
  user_display: 'Masa',
  text: 'deploy status?',
  tier: 'all',
  ...over,
});

const reply = (over: Partial<AppEvent> = {}): AppEvent => ({
  type: 'slack_reply_sent',
  channel: 'C0123ABC',
  text: 'Deploy is green.',
  identity: 'CTO Bot (as itself)',
  ...over,
});

beforeEach(() => clearSlackMirror());

describe('slackBubbleFromEvent', () => {
  it('maps an inbound message with an honest RBAC tier badge', () => {
    const b = slackBubbleFromEvent(inbound());
    expect(b).not.toBeNull();
    expect(b).toMatchObject({
      kind: 'inbound',
      channel: 'C0123ABC',
      speaker: 'Masa',
      text: 'deploy status?',
      badge: 'ALL',
    });
  });

  it('maps a reply with the honest bot-identity badge', () => {
    const b = slackBubbleFromEvent(reply());
    expect(b).not.toBeNull();
    expect(b).toMatchObject({
      kind: 'reply',
      channel: 'C0123ABC',
      text: 'Deploy is green.',
      badge: 'CTO Bot (as itself)',
      speaker: '',
    });
  });

  it('falls back to a safe identity label when the backend omits one', () => {
    const b = slackBubbleFromEvent(reply({ identity: undefined }));
    expect(b?.badge).toBe('CTO Bot (as itself)');
  });

  it('returns null for unrelated event types', () => {
    expect(slackBubbleFromEvent({ type: 'pm_thinking', text: 'x' })).toBeNull();
    expect(slackBubbleFromEvent({ type: 'ping' })).toBeNull();
  });

  it('degrades missing string fields to empty strings instead of throwing', () => {
    const b = slackBubbleFromEvent({ type: 'slack_message_received' });
    expect(b).toMatchObject({ channel: '', text: '', speaker: '' });
    // Unknown/empty tier still yields a non-empty badge, never an invented state.
    expect(b?.badge).toBe('UNKNOWN');
  });
});

describe('tierBadgeLabel', () => {
  it('renders the three known tiers', () => {
    expect(tierBadgeLabel('all')).toBe('ALL');
    expect(tierBadgeLabel('analytics')).toBe('ANALYTICS');
    expect(tierBadgeLabel('read_only')).toBe('READ-ONLY');
  });
  it('passes an unknown tier through uppercased-as-is (no fabrication)', () => {
    expect(tierBadgeLabel('vip')).toBe('vip');
    expect(tierBadgeLabel('')).toBe('UNKNOWN');
  });
});

describe('slackMirror store via pushSlackEvent', () => {
  it('appends both conversation sides in arrival order', () => {
    pushSlackEvent(inbound());
    pushSlackEvent(reply());
    const log = get(slackMirror);
    expect(log.map((b) => b.kind)).toEqual(['inbound', 'reply']);
    expect(log[0].speaker).toBe('Masa');
    expect(log[1].badge).toBe('CTO Bot (as itself)');
  });

  it('is a no-op for non-mirror events', () => {
    expect(pushSlackEvent({ type: 'agent_message', agent: 'python', text: 'hi' })).toBeNull();
    expect(get(slackMirror)).toHaveLength(0);
  });

  it('trims to SLACK_MIRROR_MAX, dropping the oldest', () => {
    for (let i = 0; i < SLACK_MIRROR_MAX + 5; i++) {
      pushSlackEvent(inbound({ text: `msg-${i}` }));
    }
    const log = get(slackMirror);
    expect(log).toHaveLength(SLACK_MIRROR_MAX);
    // Oldest five dropped: the first retained bubble is msg-5.
    expect(log[0].text).toBe('msg-5');
    expect(log[log.length - 1].text).toBe(`msg-${SLACK_MIRROR_MAX + 4}`);
  });
});
