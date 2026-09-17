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
  CHANNEL_WRITE_CREDENTIAL_MESSAGE, channelErrorMessage, channelReferences,
  createGlobalChannel, deleteGlobalChannel, fetchGlobalChannels, isChannelConflict,
  offerableProviders, saveGlobalChannels, updateGlobalChannel,
  type ChannelProvider, type GlobalChannel,
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

/** A refusal the stub should answer with: a plain sentence, or a whole body. */
type Refusal = { status: number; error?: string; body?: Record<string, unknown> };

let recorded: Recorded[] = [];
let putFailures: Refusal[] = [];
let writeFailures: Refusal[] = [];

beforeEach(() => {
  recorded = [];
  putFailures = [];
  writeFailures = [];
  resetChannelWriteToken();
  vi.stubGlobal('fetch', async (input: RequestInfo | URL, init?: RequestInit) => {
    const headers = (init?.headers ?? {}) as Record<string, string>;
    const url = String(input);
    recorded.push({ url, method: init?.method ?? 'GET', authorization: headers.Authorization, body: init?.body as string | undefined });
    if (url.endsWith('/api/config')) {
      return new Response(JSON.stringify({ auth_required: false, channel_write_token: MINTED }), { status: 200 });
    }
    // #8187: the per-channel writes have their own queue, so a test that
    // refuses a DELETE cannot accidentally refuse the whole-list PUT.
    const failure = init?.method === 'PUT' ? putFailures.shift()
      : init?.method === 'POST' || init?.method === 'DELETE' ? writeFailures.shift() : undefined;
    if (failure) return new Response(JSON.stringify(failure.body ?? { error: failure.error }), { status: failure.status });
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

  test('a validation refusal that happens to say "conflict" is not one', async () => {
    // Nothing constrains the server's wording, and treating this as a lost
    // compare-and-swap would reload the list over the operator's draft
    // (critic MEDIUM-1). A known status is the whole answer.
    putFailures.push({ status: 422, error: 'route_to conflicts with an assistant binding' });
    const cause = await saveGlobalChannels('r1', []).catch(e => e);
    expect(isChannelConflict(cause)).toBe(false);
    expect(channelErrorMessage(cause)).toBe('route_to conflicts with an assistant binding');
  });
});

// The three per-channel writes (#8038 create/update, #8187 delete). Same
// credential, same compare-and-swap; what is asserted here is the wire shape
// the daemon's `deny_unknown_fields` structs and query extractor parse.
describe('per-channel global writes', () => {
  const sent = (method: string) => recorded.find(r => r.method === method);

  test('a create posts the revision and one channel, with the write credential', async () => {
    const { transport: _t, poll_interval_secs: _p, ...fresh } = channelFixture({ id: 'ops-slack', provider: 'slack' });
    await createGlobalChannel('r1', fresh);
    const post = sent('POST');
    expect(post?.url).toMatch(/\/api\/channels$/);
    expect(post?.authorization).toBe(`Bearer ${MINTED}`);
    const body = JSON.parse(post?.body ?? '{}');
    expect(Object.keys(body)).toEqual(['revision', 'channel']);
    expect(body.revision).toBe('r1');
    expect(body.channel.id).toBe('ops-slack');
    // Connector plumbing is the daemon's default to choose, not a constant
    // copied into the page: omitted here, filled by `Channel`'s serde defaults.
    expect(body.channel).not.toHaveProperty('transport');
    expect(body.channel).not.toHaveProperty('poll_interval_secs');
  });

  test('an update puts to the channel-s own path, id taken from the record', async () => {
    await updateGlobalChannel('r1', channelFixture({ id: 'mail/personal' }));
    // The id is percent-encoded, and the body repeats it — the server answers
    // 400 when the two disagree, so they cannot be passed separately.
    expect(sent('PUT')?.url).toMatch(/\/api\/channels\/mail%2Fpersonal$/);
    expect(JSON.parse(sent('PUT')?.body ?? '{}').channel.id).toBe('mail/personal');
  });

  test('a delete carries the revision in the query and omits force unless asked', async () => {
    await deleteGlobalChannel('r1', 'gmail-personal');
    expect(sent('DELETE')?.url).toMatch(/\/api\/channels\/gmail-personal\?revision=r1$/);
    expect(sent('DELETE')?.authorization).toBe(`Bearer ${MINTED}`);
    expect(sent('DELETE')?.url).not.toContain('force');
  });

  test('a forced delete says so in the query string', async () => {
    await deleteGlobalChannel('r1', 'gmail-personal', true);
    expect(sent('DELETE')?.url).toMatch(/\?revision=r1&force=true$/);
  });

  test('a refused delete yields the assistants it named, and nothing else does', async () => {
    writeFailures.push({ status: 409, body: { error: 'still bound', referenced_by: ['izzie', 'cto-assistant'] } });
    const refused = await deleteGlobalChannel('r1', 'gmail-personal').catch(e => e);
    expect(channelReferences(refused)).toEqual(['izzie', 'cto-assistant']);
    // A lost compare-and-swap is a 409 too, and forcing would not fix it.
    writeFailures.push({ status: 409, error: 'Channel settings changed. Reload before saving.' });
    const stale = await deleteGlobalChannel('stale', 'gmail-personal').catch(e => e);
    expect(channelReferences(stale)).toBeNull();
    expect(isChannelConflict(stale)).toBe(true);
    // And a non-409 carrying the key is not a force case either.
    expect(channelReferences(Object.assign(new Error('no'), { status: 400, body: { referenced_by: ['izzie'] } }))).toBeNull();
  });

  test('only the providers the daemon serves are offered, never the test-only stub', async () => {
    const provider = (id: string): ChannelProvider =>
      ({ id, name: id, configured: true, can_send: true, can_read: false } as unknown as ChannelProvider);
    const served = [provider('telegram'), provider('stub'), provider('gworkspace')];
    expect(offerableProviders(served).map(p => p.id)).toEqual(['telegram', 'gworkspace']);
    expect(offerableProviders(undefined)).toEqual([]);
  });
});
