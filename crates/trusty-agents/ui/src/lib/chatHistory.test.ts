// Chat rehydration tests (#4278).
//
// Why: the bug was that `messages` started empty on every load and nothing
// ever read the durable log back, so a reload discarded the conversation.
// `rehydrateChat_restores_the_persisted_conversation` is the direct regression
// proof: it drives the real store through the real rehydration path and
// asserts the conversation is there afterwards. Before this change the module
// under test did not exist and the store stayed empty — that is the failure
// this pins.
//
// The second failure mode is subtler and worth its own test: bootstrap is
// async, so a user can type before the history lands. A seed that overwrote
// the bucket would delete the turn they just sent — trading one data-loss bug
// for another.
//
// What: the REST wrapper's degradation, the pure mapper's role/order/timestamp
// contract, and the two store-level rehydration paths (initial seed, lazy
// older page).

import { beforeEach, describe, expect, test, vi } from 'vitest';
import { get } from 'svelte/store';

import {
  DEFAULT_HISTORY_LIMIT,
  fetchChatHistory,
  historyToMessages,
  loadOlderChat,
  rehydrateChat,
  resolveRehydrationTarget,
  type ChatHistoryPage,
} from './chatHistory';
import { addMessage, messages, type Message } from '../stores/app';

/** A backend page as the route serializes it. */
function page(overrides: Partial<ChatHistoryPage> = {}): ChatHistoryPage {
  return {
    available: true,
    messages: [
      { role: 'user', content: 'what did we decide?' },
      { role: 'assistant', content: 'we shipped the fragment gate' },
    ],
    total: 2,
    has_more: false,
    updated_at: '2026-08-30T12:00:00Z',
    ...overrides,
  };
}

/**
 * The `url` parameter is typed rather than inferred so `spy.mock.calls[0][0]`
 * reads back as a `string` — `svelte-check` rejects indexing an untyped mock's
 * empty-tuple call args.
 */
function stubFetch(body: unknown, ok = true) {
  const spy = vi.fn((_url: string, _init?: RequestInit) =>
    Promise.resolve({ ok, json: async () => body }),
  );
  vi.stubGlobal('fetch', spy);
  return spy;
}

beforeEach(() => {
  messages.set(new Map());
  vi.unstubAllGlobals();
});

// ---------------------------------------------------------------------------
// The regression this issue is about
// ---------------------------------------------------------------------------

describe('rehydrateChat', () => {
  test('rehydrateChat_restores_the_persisted_conversation', async () => {
    stubFetch(page());

    const result = await rehydrateChat('izzie', 'Izzie', 'ctrl');

    expect(result).toEqual({ seeded: 2, hasMore: false });
    const restored = get(messages).get('ctrl') ?? [];
    expect(restored.map((m) => m.content)).toEqual([
      'what did we decide?',
      'we shipped the fragment gate',
    ]);
    expect(restored.map((m) => m.role)).toEqual(['user', 'assistant']);
  });

  test('rehydrateChat_is_a_noop_when_nothing_is_persisted', async () => {
    // The backend reports `available: false` for an agent with no palace
    // bound, no session yet, or a down daemon. None is an error.
    stubFetch(page({ available: false, messages: [], total: 0 }));

    const result = await rehydrateChat('plain', 'Assistant', 'ctrl');

    expect(result).toEqual({ seeded: 0, hasMore: false });
    expect(get(messages).get('ctrl')).toBeUndefined();
  });

  test('rehydrateChat_does_not_clobber_a_message_typed_during_bootstrap', async () => {
    // The user typed while the history request was in flight.
    const typed: Message = {
      id: 'live-1',
      role: 'user',
      content: 'typed while loading',
      timestamp: 1,
    };
    addMessage('ctrl', typed);
    stubFetch(page());

    const result = await rehydrateChat('izzie', 'Izzie', 'ctrl');

    expect(result.seeded).toBe(0);
    expect(get(messages).get('ctrl')).toEqual([typed]);
  });

  test('rehydrateChat_reports_more_when_older_turns_remain', async () => {
    stubFetch(page({ has_more: true, total: 400 }));
    await expect(rehydrateChat('izzie', 'Izzie', 'ctrl')).resolves.toEqual({
      seeded: 2,
      hasMore: true,
    });
  });
});

describe('loadOlderChat', () => {
  test('loadOlderChat_prepends_the_previous_page', async () => {
    addMessage('ctrl', {
      id: 'history-0',
      role: 'user',
      content: 'newest',
      timestamp: 1,
    });
    stubFetch(
      page({
        messages: [{ role: 'user', content: 'older' }],
        total: 3,
        has_more: true,
      }),
    );

    const result = await loadOlderChat('izzie', 'Izzie', 'ctrl', 1);

    expect(result).toEqual({ seeded: 1, hasMore: true });
    // Older turns must land ABOVE what is already rendered.
    expect((get(messages).get('ctrl') ?? []).map((m) => m.content)).toEqual([
      'older',
      'newest',
    ]);
  });

  test('loadOlderChat_ids_do_not_collide_with_the_first_page', async () => {
    stubFetch(page({ messages: [{ role: 'user', content: 'older' }], total: 3 }));
    await loadOlderChat('izzie', 'Izzie', 'ctrl', 2);
    expect((get(messages).get('ctrl') ?? [])[0].id).toBe('history-2-0');
  });
});

// ---------------------------------------------------------------------------
// The REST wrapper
// ---------------------------------------------------------------------------

describe('fetchChatHistory', () => {
  test('fetchChatHistory_requests_the_bounded_window', async () => {
    const spy = stubFetch(page());
    await fetchChatHistory('izzie');
    const url = spy.mock.calls[0][0];
    expect(url).toContain('/api/agents/izzie/chat-history');
    expect(url).toContain(`limit=${DEFAULT_HISTORY_LIMIT}`);
    expect(url).toContain('before=0');
  });

  test('fetchChatHistory_encodes_the_agent_name', async () => {
    const spy = stubFetch(page());
    await fetchChatHistory('cto assistant');
    expect(spy.mock.calls[0][0]).toContain('cto%20assistant');
  });

  test('fetchChatHistory_returns_empty_page_on_http_error', async () => {
    stubFetch({}, false);
    await expect(fetchChatHistory('izzie')).resolves.toMatchObject({
      available: false,
      messages: [],
    });
  });

  test('fetchChatHistory_returns_empty_page_on_network_failure', async () => {
    // A cold-start sidecar that is not listening yet must never throw into
    // bootstrap — it degrades to the same empty page.
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('ECONNREFUSED');
      }),
    );
    await expect(fetchChatHistory('izzie')).resolves.toMatchObject({
      available: false,
      messages: [],
    });
  });

  test('fetchChatHistory_survives_a_malformed_body', async () => {
    // The route's contract is a shape, but a proxy or a version skew can hand
    // back something else; a missing `messages` array must not throw.
    stubFetch({ available: true });
    await expect(fetchChatHistory('izzie')).resolves.toEqual({
      available: true,
      messages: [],
      total: 0,
      has_more: false,
      updated_at: null,
      reason: undefined,
    });
  });
});

// ---------------------------------------------------------------------------
// The pure mapper
// ---------------------------------------------------------------------------

describe('historyToMessages', () => {
  test('historyToMessages_maps_roles_and_preserves_order', () => {
    const out = historyToMessages(page(), 'Izzie', 999);
    expect(out.map((m) => m.role)).toEqual(['user', 'assistant']);
    expect(out.map((m) => m.content)).toEqual([
      'what did we decide?',
      'we shipped the fragment gate',
    ]);
    // Attribution matches a live conversation: assistant bubbles carry the
    // speaker, user bubbles do not.
    expect(out[1].speaker).toBe('Izzie');
    expect(out[0].speaker).toBeUndefined();
  });

  test('historyToMessages_stamps_session_time_not_a_fabricated_one', () => {
    // The store carries no per-message timestamp. Every bubble gets the
    // session's real `updated_at` rather than an invented per-turn time.
    const out = historyToMessages(page(), 'Izzie', 999);
    const sessionTime = Date.parse('2026-08-30T12:00:00Z');
    expect(out.every((m) => m.timestamp === sessionTime)).toBe(true);
  });

  test('historyToMessages_falls_back_to_now_without_a_session_time', () => {
    const out = historyToMessages(page({ updated_at: null }), 'Izzie', 4242);
    expect(out.every((m) => m.timestamp === 4242)).toBe(true);
  });

  test('historyToMessages_labels_unknown_roles_as_system', () => {
    // The history blob is untyped JSON; an unrecognised role must not be cast
    // blindly into a union it does not belong to.
    const out = historyToMessages(
      page({ messages: [{ role: 'tool', content: 'x' }] }),
      'Izzie',
      1,
    );
    expect(out[0].role).toBe('system');
  });

  test('historyToMessages_gives_every_message_a_distinct_id', () => {
    const out = historyToMessages(page(), 'Izzie', 1);
    expect(new Set(out.map((m) => m.id)).size).toBe(out.length);
  });
});

describe('resolveRehydrationTarget', () => {
  const roster = [
    { id: 'izzie', label: 'Izzie' },
    { id: 'ctrl', label: 'Concierge' },
  ];

  test('resolveRehydrationTarget_uses_the_selected_agent_and_its_label', () => {
    expect(resolveRehydrationTarget('izzie', roster)).toEqual({
      agentId: 'izzie',
      speaker: 'Izzie',
    });
  });

  test('resolveRehydrationTarget_maps_the_null_selection_to_ctrl', () => {
    // `activeAgentId` is null for the base ctrl/PM session — fetching
    // `persona-null` would silently rehydrate nothing.
    expect(resolveRehydrationTarget(null, roster)).toEqual({
      agentId: 'ctrl',
      speaker: 'Concierge',
    });
  });

  test('resolveRehydrationTarget_falls_back_when_the_roster_is_cold', () => {
    // On a cold start the catalog fetch may not have landed; a restored bubble
    // must still carry a name rather than `undefined`.
    expect(resolveRehydrationTarget('izzie', [])).toEqual({
      agentId: 'izzie',
      speaker: 'Assistant',
    });
  });
});
