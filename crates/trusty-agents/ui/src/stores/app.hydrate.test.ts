// Chat-rehydration store contracts (#4278).
//
// Why: rehydration writes into the same store live chat appends to, and the
// cursor it arms outlives the view it was armed for. Three guards keep a
// restored conversation from damaging a live one, and each is pinned here.
// `hydrateMessages` refusing a non-empty bucket makes a late-arriving history
// harmless. `prependMessages` putting older turns FIRST makes lazy loading read
// as history rather than as new messages. `canLoadOlderChat` comparing the
// cursor against live identity is what stops the "load earlier" control from
// paging one agent's conversation into another's view — a continuous
// `persona-{agent}` session always reports more history, so the affordance
// never withdraws on its own.
//
// What: seed into an empty bucket, refuse a non-empty one, ignore an empty
// page; the prepend ordering; and the cursor gate across agent and project
// switches, including recovery on switching back.

import { beforeEach, describe, expect, test } from 'vitest';
import { get } from 'svelte/store';

import {
  activeAgentId,
  activeProjectId,
  addMessage,
  canLoadOlderChat,
  chatHistoryCursor,
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
  chatHistoryCursor.set(null);
  activeAgentId.set(null);
  activeProjectId.set('ctrl');
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

describe('canLoadOlderChat', () => {
  /** Arm the cursor the way a successful seed for izzie/ctrl would. */
  function seedCursor(hasMore = true) {
    chatHistoryCursor.set({
      agentId: 'izzie',
      speaker: 'Izzie',
      projectId: 'ctrl',
      start: 100,
      hasMore,
    });
  }

  test('canLoadOlderChat_is_true_for_the_view_the_cursor_was_armed_for', () => {
    activeAgentId.set('izzie');
    activeProjectId.set('ctrl');
    seedCursor();
    expect(get(canLoadOlderChat)).toBe(true);
  });

  test('canLoadOlderChat_goes_false_when_the_agent_switches', () => {
    // The regression: `persona-izzie` runs to thousands of messages and always
    // reports more history, so a cursor gated on `hasMore` alone kept offering
    // izzie's conversation after the user moved to another agent.
    activeAgentId.set('izzie');
    activeProjectId.set('ctrl');
    seedCursor();
    expect(get(canLoadOlderChat)).toBe(true);

    activeAgentId.set('cto-assistant');

    expect(get(canLoadOlderChat)).toBe(false);
    expect(get(chatHistoryCursor)?.hasMore).toBe(true);
  });

  test('canLoadOlderChat_goes_false_when_the_project_switches', () => {
    activeAgentId.set('izzie');
    activeProjectId.set('ctrl');
    seedCursor();

    activeProjectId.set('other-project');

    expect(get(canLoadOlderChat)).toBe(false);
  });

  test('canLoadOlderChat_recovers_when_the_user_switches_back', () => {
    // Nothing resets the cursor, so returning to the same view re-qualifies it
    // — the history is still on screen and still pageable.
    activeAgentId.set('izzie');
    activeProjectId.set('ctrl');
    seedCursor();

    activeAgentId.set('cto-assistant');
    expect(get(canLoadOlderChat)).toBe(false);
    activeAgentId.set('izzie');

    expect(get(canLoadOlderChat)).toBe(true);
  });

  test('canLoadOlderChat_maps_the_null_selection_to_ctrl', () => {
    // `activeAgentId` is null for the base ctrl session; the cursor records the
    // resolved id, so the two must compare equal.
    activeAgentId.set(null);
    activeProjectId.set('ctrl');
    chatHistoryCursor.set({
      agentId: 'ctrl',
      speaker: 'Concierge',
      projectId: 'ctrl',
      start: 10,
      hasMore: true,
    });
    expect(get(canLoadOlderChat)).toBe(true);
  });

  test('canLoadOlderChat_is_false_without_a_cursor', () => {
    activeAgentId.set('izzie');
    chatHistoryCursor.set(null);
    expect(get(canLoadOlderChat)).toBe(false);
  });

  test('canLoadOlderChat_is_false_at_the_start_of_history', () => {
    activeAgentId.set('izzie');
    activeProjectId.set('ctrl');
    seedCursor(false);
    expect(get(canLoadOlderChat)).toBe(false);
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
