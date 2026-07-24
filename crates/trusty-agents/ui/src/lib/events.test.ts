// Vitest unit tests for the Events tab reducer + store (#3819, epic #3052).
// Mirrors slack-mirror.test.ts's structure: pure transform tests, then the
// bounded-store fold via pushEvent.

import { describe, it, expect, beforeEach } from 'vitest';
import { get } from 'svelte/store';
import type { AppEvent } from './transport';
import { eventRowFromEvent, sourceOf, pushEvent, eventLog, clearEventLog, EVENT_LOG_MAX } from './events';

beforeEach(() => clearEventLog());

describe('sourceOf', () => {
  it('classifies slack_* events as slack', () => {
    expect(sourceOf('slack_message_received')).toBe('slack');
    expect(sourceOf('slack_reply_sent')).toBe('slack');
  });
  it('classifies ping/lag as system', () => {
    expect(sourceOf('ping')).toBe('system');
    expect(sourceOf('lag')).toBe('system');
  });
  it('classifies everything else as task', () => {
    expect(sourceOf('agent_spawned')).toBe('task');
    expect(sourceOf('some_future_event')).toBe('task');
  });
});

describe('eventRowFromEvent', () => {
  it('summarizes known event types', () => {
    const row = eventRowFromEvent({ type: 'agent_spawned', agent: 'izzie' });
    expect(row.summary).toBe('izzie starting');
    expect(row.source).toBe('task');
  });

  it('summarizes slack events with the channel', () => {
    const row = eventRowFromEvent({
      type: 'slack_message_received',
      channel: 'C01',
      text: 'hello',
    });
    expect(row.summary).toBe('#C01: hello');
    expect(row.source).toBe('slack');
  });

  it('falls back to the bare type for unknown events, never an empty summary', () => {
    const row = eventRowFromEvent({ type: 'some_future_event' });
    expect(row.summary).toBe('some_future_event');
  });

  it('never returns an empty summary even for a known type with no text', () => {
    const row = eventRowFromEvent({ type: 'pm_thinking' } as AppEvent);
    expect(row.summary).toBe('pm_thinking');
  });

  it('classifies a gmail listener_event_received row as source "gmail" (#3820)', () => {
    const row = eventRowFromEvent({
      type: 'listener_event_received',
      listener_id: 'gmail-personal',
      provider: 'gmail',
      event_type: 'message.received',
      summary: 'dad@family.com: Dinner Sunday?',
      included: true,
    } as unknown as AppEvent);
    expect(row.source).toBe('gmail');
    expect(row.summary).toBe('dad@family.com: Dinner Sunday?');
  });

  it('falls back to "task" for a listener_event_received row from an unrecognized provider', () => {
    const row = eventRowFromEvent({
      type: 'listener_event_received',
      provider: 'google-calendar',
      summary: 'Standup at 9am',
    } as unknown as AppEvent);
    expect(row.source).toBe('task');
  });
});

describe('eventLog store via pushEvent', () => {
  it('appends rows in arrival order', () => {
    pushEvent({ type: 'session_started' });
    pushEvent({ type: 'session_done', status: 'ok' });
    const log = get(eventLog);
    expect(log.map((r) => r.type)).toEqual(['session_started', 'session_done']);
  });

  it('includes ping/lag rows (unlike the Slack mirror, which drops them)', () => {
    pushEvent({ type: 'ping' });
    expect(get(eventLog)).toHaveLength(1);
  });

  it('trims to EVENT_LOG_MAX, dropping the oldest', () => {
    for (let i = 0; i < EVENT_LOG_MAX + 5; i++) {
      pushEvent({ type: 'agent_spawned', agent: `a${i}` });
    }
    const log = get(eventLog);
    expect(log).toHaveLength(EVENT_LOG_MAX);
    expect(log[0].summary).toBe('a5 starting');
  });
});
