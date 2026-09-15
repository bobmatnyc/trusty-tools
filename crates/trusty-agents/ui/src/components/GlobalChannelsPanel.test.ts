// The Global scope editor, driven through a stubbed `fetch` (#7609 slice 6).
//
// Why: no module is mocked here. The things worth pinning — that Save stays
// disabled until a field actually differs, that the PUT body is exactly
// `{revision, channels}`, that a 401 says OUR sentence and a 409 reloads — are
// all properties of what crosses the transport, and a `vi.mock('../lib/channels')`
// would assert the test's own double instead.

import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, tick, unmount } from 'svelte';
import GlobalChannelsPanel from './GlobalChannelsPanel.svelte';
import { CHANNEL_WRITE_CREDENTIAL_MESSAGE, type GlobalChannel } from '../lib/channels';

const channel = (overrides: Partial<GlobalChannel> = {}): GlobalChannel => ({
  id: 'gmail-personal', name: 'Personal mail', provider: 'gmail', target: '',
  enabled: true, send_enabled: false, receive_enabled: true,
  transport: 'gmail_history', poll_interval_secs: 60, instructions: '',
  event_types: [], route_to: ['izzie'],
  ingest_filter: { label_ids: ['INBOX'] },
  wake_filter: { from: [], include_labels: [], exclude_labels: [], subject_contains: [], snippet_contains: [] },
  ...overrides,
});

let view: ReturnType<typeof mount> | undefined;
let channels: GlobalChannel[] = [];
let revision = 'r1';
let puts: { body: string; authorization: string | undefined }[] = [];
let putFailures: { status: number; error: string }[] = [];
let reads = 0;

// A refused write runs probe -> PUT -> 401 -> re-probe -> PUT, so the
// microtask budget has to cover four network round trips, not one.
async function settle() { for (let i = 0; i < 40; i++) { await Promise.resolve(); await tick(); } }
const button = (text: string) => [...document.querySelectorAll('button')].find(b => b.textContent?.includes(text))!;
const box = (label: string) => document.querySelector(`[aria-label="${label}"]`) as HTMLInputElement;

beforeEach(() => {
  channels = [channel(), channel({ id: 'slack-ops', name: 'Ops', provider: 'slack', target: 'C123', send_enabled: true, route_to: [] })];
  revision = 'r1';
  puts = [];
  putFailures = [];
  reads = 0;
  vi.stubGlobal('fetch', async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input), headers = (init?.headers ?? {}) as Record<string, string>;
    if (url.endsWith('/api/config')) return new Response(JSON.stringify({ auth_required: false, channel_write_token: 'minted' }), { status: 200 });
    if (url.endsWith('/api/agents')) return new Response(JSON.stringify({ agents: [{ name: 'izzie' }, { name: 'cto-assistant' }] }), { status: 200 });
    if (init?.method === 'PUT') {
      puts.push({ body: String(init.body), authorization: headers.Authorization });
      const failure = putFailures.shift();
      if (failure) return new Response(JSON.stringify({ error: failure.error }), { status: failure.status });
      channels = JSON.parse(String(init.body)).channels;
      revision = 'r2';
    } else reads += 1;
    return new Response(JSON.stringify({ scope: 'global', revision, channels, providers: [] }), { status: 200 });
  });
});

afterEach(async () => {
  if (view) await unmount(view);
  view = undefined;
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
});

async function render() { view = mount(GlobalChannelsPanel, { target: document.body }); await settle(); }

it('lists each global channel with its provider, destination and switches', async () => {
  await render();
  expect(document.body.textContent).toContain('Personal mail');
  expect(document.body.textContent).toContain('account-wide');
  expect(document.body.textContent).toContain('slack · C123');
  expect(box('gmail-personal receive updates').checked).toBe(true);
  // No destination means nothing to send to, so the switch is not offered.
  expect(box('gmail-personal allow sending').disabled).toBe(true);
  expect(box('slack-ops allow sending').disabled).toBe(false);
});

it('offers every assistant on the host as a route target, preselecting the stored ones', async () => {
  await render();
  const routes = document.querySelector('[aria-label="gmail-personal routes to"]') as HTMLSelectElement;
  expect([...routes.options].map(o => o.value)).toEqual(['izzie', 'cto-assistant']);
  expect([...routes.selectedOptions].map(o => o.value)).toEqual(['izzie']);
});

it('keeps Save disabled until a field differs from the loaded state', async () => {
  await render();
  expect(button('Save channels').disabled).toBe(true);
  box('gmail-personal enabled').click();
  await settle();
  expect(button('Save channels').disabled).toBe(false);
  // Back to the loaded value: no field differs, so there is nothing to save.
  box('gmail-personal enabled').click();
  await settle();
  expect(button('Save channels').disabled).toBe(true);
});

it('sends exactly the revision and the channels, credential attached', async () => {
  await render();
  const routes = document.querySelector('[aria-label="gmail-personal routes to"]') as HTMLSelectElement;
  [...routes.options].forEach(option => { option.selected = true; });
  routes.dispatchEvent(new Event('change'));
  await settle();
  button('Save channels').click();
  await settle();
  expect(puts).toHaveLength(1);
  expect(puts[0].authorization).toBe('Bearer minted');
  const body = JSON.parse(puts[0].body);
  expect(Object.keys(body)).toEqual(['revision', 'channels']);
  expect(body.revision).toBe('r1');
  expect(body.channels[0].route_to).toEqual(['izzie', 'cto-assistant']);
  // Untouched plumbing survives the round trip.
  expect(body.channels[0]).toMatchObject({ transport: 'gmail_history', poll_interval_secs: 60, ingest_filter: { label_ids: ['INBOX'] } });
  expect(document.body.textContent).toContain('Channels saved.');
  expect(button('Save channels').disabled).toBe(true);
});

it('says the write needs a credential in our words, not the daemon-s', async () => {
  await render();
  const refusal = 'Channel writes require an API token. Start the daemon with --api-token.';
  putFailures.push({ status: 401, error: refusal }, { status: 401, error: refusal });
  box('gmail-personal enabled').click();
  await settle();
  button('Save channels').click();
  await settle();
  expect(document.querySelector('[role="alert"]')?.textContent).toBe(CHANNEL_WRITE_CREDENTIAL_MESSAGE);
  expect(document.body.textContent).not.toContain('require an API token');
});

it('reloads the list when another writer wins the compare-and-swap', async () => {
  await render();
  const before = reads;
  putFailures.push({ status: 409, error: 'The channel list changed elsewhere' });
  channels = [channel({ name: 'Renamed elsewhere' })];
  revision = 'r9';
  box('gmail-personal enabled').click();
  await settle();
  button('Save channels').click();
  await settle();
  expect(reads).toBe(before + 1);
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('Another writer changed the global channels');
  expect(document.body.textContent).toContain('Renamed elsewhere');
  // The reloaded list is the new baseline, so Save is disabled again and the
  // next one would carry r9 rather than the revision that just lost.
  expect(button('Save channels').disabled).toBe(true);
});

it("shows the server's own wording for a validation refusal", async () => {
  await render();
  putFailures.push({ status: 422, error: 'unknown field `providers`' });
  box('gmail-personal enabled').click();
  await settle();
  button('Save channels').click();
  await settle();
  expect(document.querySelector('[role="alert"]')?.textContent).toBe('unknown field `providers`');
});
