// The channel-write credential, asserted at the TRANSPORT boundary (#7609).
//
// Why: the gate is a header on one HTTP request. Mocking `tmApi` or the
// `channel-auth` module would assert that this test's own mock was called;
// mocking `fetch` asserts what actually leaves the page, which is the only
// thing the daemon sees. Both write paths are covered — the per-assistant
// scope and, since #7609 slice 6, the global one, because a second write path
// forgetting the gate is exactly how the deprecated Listeners route 401'd
// after slice 5 gated it (critic HIGH-2).
//
// What: a fake `fetch` answers `/api/config` with a minted credential and
// records every request, so each case can read back the `Authorization` header
// the PUT carried. Also pins the two caching rules (critic MEDIUM-5): a failed
// probe is never cached, and a 401 retries once against a fresh probe.

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';

import { saveChannels, saveGlobalChannels } from './channels';
import { resetChannelWriteToken } from './channel-auth';

const MINTED = 'minted-credential-abc';

// The daemon's own refusal text, copied from `channel_auth.rs`'s `REFUSAL`.
// The fixture answers with THIS rather than a convenient "401 unauthorized",
// because the retry used to branch on the message and this string contains
// neither token — a friendlier fixture hid the defect (critic round 3, HIGH).
const REFUSAL =
  'Channel writes require an API token. Start the daemon with --api-token (or ' +
  'TAGENT_API_TOKEN); this process accepts channel reads only.';

interface Recorded {
  url: string;
  method: string;
  authorization: string | undefined;
}

let recorded: Recorded[] = [];
let configResponses: (() => Response)[] = [];
let putStatus: number[] = [];

function ok(body: unknown): Response {
  return new Response(JSON.stringify(body), {
    status: 200,
    headers: { 'Content-Type': 'application/json' },
  });
}

function record(input: RequestInfo | URL, init?: RequestInit): Recorded {
  const headers = (init?.headers ?? {}) as Record<string, string>;
  return {
    url: String(input),
    method: init?.method ?? 'GET',
    authorization: headers.Authorization,
  };
}

beforeEach(() => {
  recorded = [];
  configResponses = [];
  putStatus = [];
  resetChannelWriteToken();
  vi.stubGlobal('localStorage', {
    getItem: () => null,
    setItem: () => {},
    removeItem: () => {},
  });
  vi.stubGlobal('fetch', async (input: RequestInfo | URL, init?: RequestInit) => {
    const entry = record(input, init);
    recorded.push(entry);
    if (entry.url.endsWith('/api/config')) {
      const next = configResponses.shift();
      return next ? next() : ok({ auth_required: false, channel_write_token: MINTED });
    }
    const status = putStatus.shift() ?? 200;
    if (status !== 200) {
      return new Response(JSON.stringify({ error: REFUSAL }), { status });
    }
    return ok({ agent: 'fixture', revision: 'r2', bindings: [], channels: [], providers: [] });
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  resetChannelWriteToken();
});

function put(): Recorded | undefined {
  return recorded.find(r => r.method === 'PUT');
}

describe('channel-write credential on the wire', () => {
  test('saveChannels sends the minted credential on its PUT', async () => {
    await saveChannels('fixture', 'r1', []);
    const sent = put();
    expect(sent?.url).toContain('/api/agents/fixture/channels');
    expect(sent?.authorization).toBe(`Bearer ${MINTED}`);
  });

  test('saveGlobalChannels sends it too — the global scope is gated as well', async () => {
    await saveGlobalChannels('r1', []);
    const sent = put();
    expect(sent?.url).toMatch(/\/api\/channels$/);
    expect(sent?.authorization).toBe(`Bearer ${MINTED}`);
  });

  test('a failed probe is not cached, so the next save re-probes', async () => {
    // First probe fails outright; the save goes out unauthenticated and the
    // daemon refuses it.
    configResponses.push(() => new Response('nope', { status: 500 }));
    putStatus.push(401, 401);
    await expect(saveChannels('fixture', 'r1', [])).rejects.toThrow();
    expect(put()?.authorization).toBeUndefined();

    // The second save probes again — nothing cached the failure — and succeeds.
    recorded = [];
    await saveChannels('fixture', 'r1', []);
    expect(put()?.authorization).toBe(`Bearer ${MINTED}`);
  });

  test('a 401 retries once against a freshly probed credential', async () => {
    // The daemon restarted and minted a new credential under a live page: the
    // first PUT carries the stale one and 401s, the retry carries the new one.
    await saveChannels('fixture', 'r1', []);
    recorded = [];
    configResponses.push(() =>
      ok({ auth_required: false, channel_write_token: 'rotated-credential' }),
    );
    putStatus.push(401);
    await saveChannels('fixture', 'r1', []);
    const puts = recorded.filter(r => r.method === 'PUT');
    expect(puts).toHaveLength(2);
    expect(puts[0].authorization).toBe(`Bearer ${MINTED}`);
    expect(puts[1].authorization).toBe('Bearer rotated-credential');
  });
});
