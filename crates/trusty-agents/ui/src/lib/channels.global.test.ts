// The global channel transport, asserted on the wire (#7609 slice 6).
//
// Why: `PUT /api/channels` deserializes into a `deny_unknown_fields` struct, so
// the exact JSON that leaves the page is the contract — echoing back the
// `providers` and `scope` the GET carries is a 422, found live on slice 5. A
// module mock would assert the mock's argument list; only a `fetch` stub sees
// the body the daemon parses.
//
// What: a fake `fetch` answers `/api/config` with a minted credential, records
// every request, and can be told to refuse the PUT with a given status.

import { afterEach, beforeEach, describe, expect, test, vi } from 'vitest';

import {
  CHANNEL_WRITE_CREDENTIAL_MESSAGE, channelErrorMessage, fetchGlobalChannels,
  isChannelConflict, saveGlobalChannels, type GlobalChannel,
} from './channels';
import { resetChannelWriteToken } from './channel-auth';

const MINTED = 'minted-credential-abc';

const channelFixture = (overrides: Partial<GlobalChannel> = {}): GlobalChannel => ({
  id: 'gmail-personal', name: 'Personal mail', provider: 'gmail', target: '',
  enabled: true, send_enabled: false, receive_enabled: true,
  transport: 'gmail_history', poll_interval_secs: 60, instructions: '',
  event_types: [], route_to: ['izzie'],
  ingest_filter: { label_ids: ['INBOX'] },
  wake_filter: { from: [], include_labels: [], exclude_labels: [], subject_contains: [], snippet_contains: [] },
  ...overrides,
});

interface Recorded { url: string; method: string; authorization: string | undefined; body: string | undefined }

let recorded: Recorded[] = [];
let putFailures: { status: number; error: string }[] = [];

beforeEach(() => {
  recorded = [];
  putFailures = [];
  resetChannelWriteToken();
  vi.stubGlobal('fetch', async (input: RequestInfo | URL, init?: RequestInit) => {
    const headers = (init?.headers ?? {}) as Record<string, string>;
    const url = String(input);
    recorded.push({ url, method: init?.method ?? 'GET', authorization: headers.Authorization, body: init?.body as string | undefined });
    if (url.endsWith('/api/config')) {
      return new Response(JSON.stringify({ auth_required: false, channel_write_token: MINTED }), { status: 200 });
    }
    const failure = init?.method === 'PUT' ? putFailures.shift() : undefined;
    if (failure) return new Response(JSON.stringify({ error: failure.error }), { status: failure.status });
    return new Response(JSON.stringify({ scope: 'global', revision: 'r2', channels: [channelFixture()], providers: [] }), { status: 200 });
  });
});

afterEach(() => {
  vi.unstubAllGlobals();
  resetChannelWriteToken();
});

const put = () => recorded.find(r => r.method === 'PUT');

describe('global channel writes', () => {
  test('sends only revision and channels, with the write credential', async () => {
    await saveGlobalChannels('r1', [channelFixture()]);
    const sent = put();
    expect(sent?.url).toMatch(/\/api\/channels$/);
    expect(sent?.authorization).toBe(`Bearer ${MINTED}`);
    // The whole body, not a subset: an extra key here is the 422 slice 5 hit.
    expect(Object.keys(JSON.parse(sent?.body ?? '{}'))).toEqual(['revision', 'channels']);
    expect(JSON.parse(sent?.body ?? '{}').revision).toBe('r1');
  });

  test('round-trips the fields the form never shows', async () => {
    // A migrated listener's transport, poll interval and ingest filter are not
    // on the form; a save that dropped them would reset the poller.
    await saveGlobalChannels('r1', [channelFixture()]);
    expect(JSON.parse(put()?.body ?? '{}').channels[0]).toMatchObject({
      transport: 'gmail_history', poll_interval_secs: 60, ingest_filter: { label_ids: ['INBOX'] },
    });
  });

  test('a read carries no write credential and needs no probe', async () => {
    const config = await fetchGlobalChannels();
    expect(config.channels[0].route_to).toEqual(['izzie']);
    expect(recorded.every(r => r.method === 'GET')).toBe(true);
    expect(recorded.some(r => r.url.endsWith('/api/config'))).toBe(false);
  });

  test('a refused write maps to our own sentence, never the daemon words', async () => {
    // Two refusals: `withChannelWriteAuth` retries once against a fresh probe.
    const refusal = 'Channel writes require an API token. Start the daemon with --api-token.';
    putFailures.push({ status: 401, error: refusal }, { status: 401, error: refusal });
    const cause = await saveGlobalChannels('r1', []).catch(e => e);
    expect((cause as { status?: number }).status).toBe(401);
    expect(channelErrorMessage(cause)).toBe(CHANNEL_WRITE_CREDENTIAL_MESSAGE);
    expect(channelErrorMessage(cause)).not.toContain(refusal);
  });

  test("a validation refusal shows the server's own error field", async () => {
    putFailures.push({ status: 422, error: 'unknown field `providers`' });
    const cause = await saveGlobalChannels('r1', []).catch(e => e);
    expect(channelErrorMessage(cause)).toBe('unknown field `providers`');
    putFailures.push({ status: 400, error: 'route_to names `ghost`, which is not an assistant on this host' });
    const bad = await saveGlobalChannels('r1', []).catch(e => e);
    expect(channelErrorMessage(bad)).toBe('route_to names `ghost`, which is not an assistant on this host');
  });

  test('a stale revision is recognised as a conflict', async () => {
    putFailures.push({ status: 409, error: 'The channel list changed elsewhere' });
    const cause = await saveGlobalChannels('stale', []).catch(e => e);
    expect(isChannelConflict(cause)).toBe(true);
    // A plain Error with no status still reads as a conflict, which is what a
    // caller raising its own 409 text relies on.
    expect(isChannelConflict(new Error('409 conflict'))).toBe(true);
    expect(isChannelConflict(new Error('The mailbox is unreachable'))).toBe(false);
  });
});
