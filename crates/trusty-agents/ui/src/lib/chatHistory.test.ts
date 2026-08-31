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
import {
  addMessage,
  chatHistoryCursor,
  messages,
  type Message,
} from '../stores/app';

/** A backend page as the route serializes it. */
function page(overrides: Partial<ChatHistoryPage> = {}): ChatHistoryPage {
  return {
    available: true,
    messages: [
      { role: 'user', content: 'what did we decide?' },
      { role: 'assistant', content: 'we shipped the fragment gate' },
    ],
    start: 0,
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
function stubFetch(body: unknown, ok = true, status = ok ? 200 : 503) {
  const spy = vi.fn((_url: string, _init?: RequestInit) =>
    Promise.resolve({ ok, status, json: async () => body }),
  );
  vi.stubGlobal('fetch', spy);
  return spy;
}

beforeEach(() => {
  messages.set(new Map());
  chatHistoryCursor.set(null);
  vi.unstubAllGlobals();
});

// ---------------------------------------------------------------------------
// The regression this issue is about
// ---------------------------------------------------------------------------

describe('rehydrateChat', () => {
  test('rehydrateChat_restores_the_persisted_conversation', async () => {
    stubFetch(page());

    const result = await rehydrateChat('izzie', 'Izzie', 'ctrl');

    expect(result).toEqual({ seeded: 2, hasMore: false, reason: undefined });
    const restored = get(messages).get('ctrl') ?? [];
    expect(restored.map((m) => m.content)).toEqual([
      'what did we decide?',
      'we shipped the fragment gate',
    ]);
    expect(restored.map((m) => m.role)).toEqual(['user', 'assistant']);
  });

  test('rehydrateChat_surfaces_the_backend_reason', async () => {
    // Without this the caller cannot tell a first-run empty conversation from a
    // broken one — every failure mode renders identically.
    stubFetch(
      page({
        available: false,
        messages: [],
        reason: 'this agent binds no memory palace',
      }),
    );
    const result = await rehydrateChat('plain', 'Assistant', 'ctrl');
    expect(result.reason).toBe('this agent binds no memory palace');
  });

  test('rehydrateChat_surfaces_a_transport_failure_as_a_reason', async () => {
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => {
        throw new Error('ECONNREFUSED');
      }),
    );
    const result = await rehydrateChat('izzie', 'Izzie', 'ctrl');
    expect(result.seeded).toBe(0);
    expect(result.reason).toContain('unreachable');
  });

  test('rehydrateChat_surfaces_an_http_error_as_a_reason', async () => {
    stubFetch({}, false);
    const result = await rehydrateChat('izzie', 'Izzie', 'ctrl');
    expect(result.reason).toContain('HTTP 503');
  });

  test('rehydrateChat_is_a_noop_when_nothing_is_persisted', async () => {
    // The backend reports `available: false` for an agent with no palace
    // bound, no session yet, or a down daemon. None is an error.
    stubFetch(page({ available: false, messages: [], total: 0 }));

    const result = await rehydrateChat('plain', 'Assistant', 'ctrl');

    expect(result.seeded).toBe(0);
    expect(result.hasMore).toBe(false);
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
    await expect(rehydrateChat('izzie', 'Izzie', 'ctrl')).resolves.toMatchObject({
      seeded: 2,
      hasMore: true,
    });
  });
});

describe('loadOlderChat', () => {
  /** Arm the cursor the way a completed `rehydrateChat` would. */
  function armCursor(start: number, hasMore = true) {
    chatHistoryCursor.set({ agentId: 'izzie', speaker: 'Izzie', start, hasMore });
  }

  test('loadOlderChat_prepends_the_previous_page', async () => {
    addMessage('ctrl', {
      id: 'history-4',
      role: 'user',
      content: 'newest',
      timestamp: 1,
    });
    armCursor(4);
    stubFetch(
      page({
        messages: [{ role: 'user', content: 'older' }],
        start: 3,
        total: 5,
        has_more: true,
      }),
    );

    const result = await loadOlderChat('ctrl');

    expect(result).toMatchObject({ seeded: 1, hasMore: true });
    // Older turns must land ABOVE what is already rendered.
    expect((get(messages).get('ctrl') ?? []).map((m) => m.content)).toEqual([
      'older',
      'newest',
    ]);
  });

  test('loadOlderChat_advances_the_cursor', async () => {
    armCursor(4);
    stubFetch(
      page({ messages: [{ role: 'user', content: 'older' }], start: 3, has_more: true }),
    );

    await loadOlderChat('ctrl');

    // The next request must ask for the window ending where this one began.
    expect(get(chatHistoryCursor)).toEqual({
      agentId: 'izzie',
      speaker: 'Izzie',
      start: 3,
      hasMore: true,
    });
  });

  test('loadOlderChat_sends_the_cursor_as_until', async () => {
    armCursor(6);
    const spy = stubFetch(page({ messages: [{ role: 'user', content: 'x' }], start: 2 }));
    await loadOlderChat('ctrl');
    expect(spy.mock.calls[0][0]).toContain('until=6');
  });

  test('loadOlderChat_is_a_noop_without_a_cursor', async () => {
    const spy = stubFetch(page());
    await expect(loadOlderChat('ctrl')).resolves.toEqual({ seeded: 0, hasMore: false });
    expect(spy).not.toHaveBeenCalled();
  });

  test('loadOlderChat_is_a_noop_once_the_cursor_is_exhausted', async () => {
    armCursor(0, false);
    const spy = stubFetch(page());
    await loadOlderChat('ctrl');
    expect(spy).not.toHaveBeenCalled();
  });

  test('loadOlderChat_disarms_the_cursor_at_the_start_of_history', async () => {
    armCursor(2);
    stubFetch(page({ messages: [{ role: 'user', content: 'oldest' }], start: 0, has_more: false }));
    await loadOlderChat('ctrl');
    expect(get(chatHistoryCursor)?.hasMore).toBe(false);
  });

  test('loadOlderChat_ids_do_not_collide_with_the_first_page', async () => {
    armCursor(4);
    stubFetch(page({ messages: [{ role: 'user', content: 'older' }], start: 3 }));
    await loadOlderChat('ctrl');
    expect((get(messages).get('ctrl') ?? [])[0].id).toBe('history-3-0');
  });
});

describe('the cursor rehydrateChat arms', () => {
  test('rehydrateChat_arms_the_cursor_from_the_served_start', async () => {
    stubFetch(page({ start: 98, total: 100, has_more: true }));
    await rehydrateChat('izzie', 'Izzie', 'ctrl');
    expect(get(chatHistoryCursor)).toEqual({
      agentId: 'izzie',
      speaker: 'Izzie',
      start: 98,
      hasMore: true,
    });
  });

  test('rehydrateChat_does_not_arm_the_cursor_when_the_seed_was_refused', async () => {
    // Live messages won the bucket, so paging older into it would interleave a
    // restored conversation with an unrelated live one.
    addMessage('ctrl', { id: 'live-1', role: 'user', content: 'typed', timestamp: 1 });
    stubFetch(page({ start: 98, has_more: true }));

    const result = await rehydrateChat('izzie', 'Izzie', 'ctrl');

    expect(result.hasMore).toBe(false);
    expect(get(chatHistoryCursor)).toBeNull();
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
  });

  test('fetchChatHistory_sends_until_only_when_paging_back', async () => {
    // The newest page must NOT pin an index — omitting `until` is what lets the
    // server serve the true newest page including turns that landed since.
    const first = stubFetch(page());
    await fetchChatHistory('izzie');
    expect(first.mock.calls[0][0]).not.toContain('until=');

    const second = stubFetch(page());
    await fetchChatHistory('izzie', 50, 0);
    // `until=0` is a real cursor value (the start of history), not "unset".
    expect(second.mock.calls[0][0]).toContain('until=0');
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
    await expect(fetchChatHistory('izzie')).resolves.toMatchObject({
      available: true,
      messages: [],
      start: 0,
      total: 0,
      has_more: false,
      updated_at: null,
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
