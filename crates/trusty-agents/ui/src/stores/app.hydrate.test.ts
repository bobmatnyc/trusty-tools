// `hydrateMessages` / `prependMessages` store contracts (#4278).
//
// Why: rehydration writes into the same store live chat appends to, so the
// seeding rules are what keep a restored conversation from destroying a live
// one. `hydrateMessages` refusing a non-empty bucket is the guard that makes a
// late-arriving history harmless; `prependMessages` putting older turns FIRST
// is what makes lazy loading read as history rather than as new messages.
//
// What: seed into an empty bucket, refuse a non-empty one, ignore an empty
// page, and the prepend ordering.

import { beforeEach, describe, expect, test } from 'vitest';
import { get } from 'svelte/store';

import {
  addMessage,
  hydrateMessages,
  messages,
  prependMessages,
  type Message,
} from './app';

function msg(id: string, content: string): Message {
  return { id, role: 'user', content, timestamp: 0 };
}

beforeEach(() => {
  messages.set(new Map());
});

describe('hydrateMessages', () => {
  test('hydrateMessages_seeds_an_empty_bucket', () => {
    expect(hydrateMessages('ctrl', [msg('h-0', 'a'), msg('h-1', 'b')])).toBe(true);
    expect((get(messages).get('ctrl') ?? []).map((m) => m.content)).toEqual(['a', 'b']);
  });

  test('hydrateMessages_never_clobbers_existing_messages', () => {
    addMessage('ctrl', msg('live-1', 'typed'));
    expect(hydrateMessages('ctrl', [msg('h-0', 'restored')])).toBe(false);
    expect((get(messages).get('ctrl') ?? []).map((m) => m.content)).toEqual(['typed']);
  });

  test('hydrateMessages_ignores_an_empty_history', () => {
    expect(hydrateMessages('ctrl', [])).toBe(false);
    expect(get(messages).get('ctrl')).toBeUndefined();
  });

  test('hydrateMessages_is_idempotent', () => {
    const history = [msg('h-0', 'a')];
    hydrateMessages('ctrl', history);
    // A second call (a re-fired bootstrap) must not double the conversation.
    expect(hydrateMessages('ctrl', history)).toBe(false);
    expect(get(messages).get('ctrl')).toHaveLength(1);
  });

  test('hydrateMessages_keeps_buckets_independent', () => {
    hydrateMessages('ctrl', [msg('h-0', 'a')]);
    hydrateMessages('other', [msg('h-0', 'b')]);
    expect((get(messages).get('ctrl') ?? [])[0].content).toBe('a');
    expect((get(messages).get('other') ?? [])[0].content).toBe('b');
  });
});

describe('prependMessages', () => {
  test('prependMessages_puts_older_turns_first', () => {
    addMessage('ctrl', msg('n-0', 'newest'));
    prependMessages('ctrl', [msg('o-0', 'older'), msg('o-1', 'less old')]);
    expect((get(messages).get('ctrl') ?? []).map((m) => m.content)).toEqual([
      'older',
      'less old',
      'newest',
    ]);
  });

  test('prependMessages_ignores_an_empty_page', () => {
    addMessage('ctrl', msg('n-0', 'newest'));
    prependMessages('ctrl', []);
    expect(get(messages).get('ctrl')).toHaveLength(1);
  });
});
