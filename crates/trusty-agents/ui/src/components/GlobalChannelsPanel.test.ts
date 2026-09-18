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
let readFails = false;
let catalogFails = false;
// #8187: the create and delete halves. `providers` is what the daemon serves —
// including the opt-in `stub` adapter, which must never be offered — and
// `routable_assistants` is the dispatch roster, which is NOT the assistant
// catalog `/api/agents` answers with.
let posts: { url: string; body: string; authorization: string | undefined }[] = [];
let deletes: { url: string; authorization: string | undefined }[] = [];
let writeFailures: { status: number; body: Record<string, unknown> }[] = [];
let providers: { id: string; name: string }[] = [];
let routable: string[] = [];
let inertBindings: string[] = [];

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
  readFails = false;
  catalogFails = false;
  posts = [];
  deletes = [];
  writeFailures = [];
  inertBindings = [];
  providers = [{ id: 'telegram', name: 'Telegram' }, { id: 'stub', name: 'Stub' }, { id: 'gworkspace', name: 'Google Workspace' }];
  routable = ['izzie', 'cto-assistant', 'pm'];
  vi.stubGlobal('fetch', async (input: RequestInfo | URL, init?: RequestInit) => {
    const url = String(input), headers = (init?.headers ?? {}) as Record<string, string>;
    if (url.endsWith('/api/config')) return new Response(JSON.stringify({ auth_required: false, channel_write_token: 'minted' }), { status: 200 });
    if (url.endsWith('/api/agents')) {
      if (catalogFails) return new Response('boom', { status: 503 });
      return new Response(JSON.stringify({ agents: [{ name: 'izzie' }, { name: 'cto-assistant' }] }), { status: 200 });
    }
    if (init?.method === 'PUT') {
      puts.push({ body: String(init.body), authorization: headers.Authorization });
      const failure = putFailures.shift();
      if (failure) return new Response(JSON.stringify({ error: failure.error }), { status: failure.status });
      channels = JSON.parse(String(init.body)).channels;
      revision = 'r2';
    } else if (init?.method === 'POST') {
      posts.push({ url, body: String(init.body), authorization: headers.Authorization });
      const failure = writeFailures.shift();
      if (failure) return new Response(JSON.stringify(failure.body), { status: failure.status });
      channels = [...channels, JSON.parse(String(init.body)).channel];
      revision = 'r2';
    } else if (init?.method === 'DELETE') {
      deletes.push({ url, authorization: headers.Authorization });
      const failure = writeFailures.shift();
      if (failure) return new Response(JSON.stringify(failure.body), { status: failure.status });
      const id = decodeURIComponent(url.split('/api/channels/')[1].split('?')[0]);
      channels = channels.filter(channel => channel.id !== id);
      revision = 'r2';
      return new Response(JSON.stringify({ scope: 'global', revision, channels, providers, routable_assistants: routable, deleted: id, inert_bindings: inertBindings }), { status: 200 });
    } else {
      reads += 1;
      if (readFails) return new Response(JSON.stringify({ error: 'read refused' }), { status: 503 });
    }
    return new Response(JSON.stringify({ scope: 'global', revision, channels, providers, routable_assistants: routable }), { status: 200 });
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

// The catalog `GET /api/agents` serves is filtered to `role == "assistant" &&
// !hidden`, while the server validates `route_to` against the UNFILTERED
// dispatch roster. `pm` is the case that difference produces: a legal route the
// catalog never lists. Before the union, the select offered catalog names only,
// Svelte rebuilt `route_to` from the checked options, and `pm` was dropped from
// the payload by an edit that had nothing to do with it (critic HIGH).
it('keeps a route the assistant catalog does not list through an edit of the same select', async () => {
  channels = [channel({ route_to: ['izzie', 'pm'] })];
  await render();
  const routes = document.querySelector('[aria-label="gmail-personal routes to"]') as HTMLSelectElement;
  expect([...routes.options].map(o => o.value)).toEqual(['izzie', 'cto-assistant', 'pm']);
  expect([...routes.selectedOptions].map(o => o.value)).toEqual(['izzie', 'pm']);
  expect(document.body.textContent).toContain('not in the assistant catalog');

  // Add a catalog name; the off-catalog one must ride along untouched.
  [...routes.options].find(o => o.value === 'cto-assistant')!.selected = true;
  routes.dispatchEvent(new Event('change'));
  await settle();
  button('Save channels').click();
  await settle();
  expect(JSON.parse(puts[0].body).channels[0].route_to).toEqual(['izzie', 'cto-assistant', 'pm']);
});

it('lets an off-catalog route be deselected and put back', async () => {
  channels = [channel({ route_to: ['izzie', 'pm'] })];
  await render();
  const routes = document.querySelector('[aria-label="gmail-personal routes to"]') as HTMLSelectElement;
  const pm = [...routes.options].find(o => o.value === 'pm')!;
  pm.selected = false;
  routes.dispatchEvent(new Event('change'));
  await settle();
  // The option survives its own deselection, or the operator could never undo it.
  expect([...routes.options].map(o => o.value)).toContain('pm');
  pm.selected = true;
  routes.dispatchEvent(new Event('change'));
  await settle();
  expect(button('Save channels').disabled).toBe(true);
});

it('reports a catalog read failure instead of claiming there are no assistants', async () => {
  catalogFails = true;
  channels = [channel({ route_to: ['izzie'] })];
  await render();
  expect(document.body.textContent).toContain('The assistant list could not be read');
  expect(document.body.textContent).not.toContain('No assistants are configured on this host');
  // A stored route is still offered and still selected, so a save round-trips it.
  const routes = document.querySelector('[aria-label="gmail-personal routes to"]') as HTMLSelectElement;
  expect([...routes.selectedOptions].map(o => o.value)).toEqual(['izzie']);
  // And it is not accused of being off-catalog when the catalog is simply absent.
  expect(document.body.textContent).not.toContain('not in the assistant catalog');
});

it('raises onSaved only for a save that landed', async () => {
  const saved = vi.fn();
  view = mount(GlobalChannelsPanel, { target: document.body, props: { onSaved: saved } });
  await settle();
  putFailures.push({ status: 422, error: 'nope' });
  box('gmail-personal enabled').click();
  await settle();
  button('Save channels').click();
  await settle();
  expect(saved).not.toHaveBeenCalled();
  button('Save channels').click();
  await settle();
  expect(saved).toHaveBeenCalledTimes(1);
});

// #8187 — the add and delete halves. Both write through the per-channel routes
// under the revision the list was READ at, so the assertions are on the wire:
// which URL, which query string, which body keys.
const type = (label: string, value: string) => {
  const input = document.querySelector(`[aria-label="${label}"]`) as HTMLInputElement;
  input.value = value;
  input.dispatchEvent(new Event('input'));
};
const picker = (label: string) => document.querySelector(`[aria-label="${label}"]`) as HTMLSelectElement;
const deleteButton = (label: string) => document.querySelector(`[aria-label="Delete ${label}"]`) as HTMLButtonElement;
const alerts = () => [...document.querySelectorAll('[role="alert"]')].map(node => node.textContent ?? '').join(' ');

async function openAdd(id = 'ops-telegram', name = 'Ops alerts') {
  button('Add channel').click();
  await settle();
  type('New channel ID', id);
  type('New channel name', name);
  await settle();
}

it('creates a channel through POST at the revision the list was read at', async () => {
  await render();
  await openAdd();
  const routes = picker('New channel routes to');
  [...routes.options].find(option => option.value === 'pm')!.selected = true;
  routes.dispatchEvent(new Event('change'));
  await settle();
  button('Create channel').click();
  await settle();
  expect(posts).toHaveLength(1);
  expect(posts[0].url).toMatch(/\/api\/channels$/);
  expect(posts[0].authorization).toBe('Bearer minted');
  const body = JSON.parse(posts[0].body);
  // The whole body: `ChannelWrite` is `deny_unknown_fields`, and the revision
  // is the compare-and-swap — a create that omitted it would overwrite whatever
  // another writer had just published.
  expect(Object.keys(body)).toEqual(['revision', 'channel']);
  expect(body.revision).toBe('r1');
  expect(body.channel).toMatchObject({
    id: 'ops-telegram', name: 'Ops alerts', provider: 'telegram', target: '',
    enabled: false, send_enabled: false, receive_enabled: false, route_to: ['pm'],
  });
  // Nothing goes through the whole-list PUT, which would republish a stale list.
  expect(puts).toHaveLength(0);
  expect(document.body.textContent).toContain('Ops alerts added');
});

it('offers the providers the daemon serves, and never the test-only stub', async () => {
  // `discord` is an id this build has never heard of — it is in no type, no
  // constant and no list here. Offering it is what makes the page's provider
  // list the DAEMON's: an adapter shipped after this bundle was built is
  // pickable without a UI release. A list hard-coded here would pass the
  // `stub` half of this test and fail this line (critic MEDIUM-3).
  providers = [{ id: 'telegram', name: 'Telegram' }, { id: 'stub', name: 'Stub' }, { id: 'discord', name: 'Discord' }];
  await render();
  button('Add channel').click();
  await settle();
  expect([...picker('New channel provider').options].map(option => option.value)).toEqual(['telegram', 'discord']);
  expect(picker('New channel provider').textContent).toContain('Discord');
  expect(document.body.textContent).not.toContain('Stub');
});

it('routes a new channel to the dispatch roster, not the assistant catalog', async () => {
  await render();
  button('Add channel').click();
  await settle();
  // `pm` is routable and absent from `GET /api/agents`; a form built from the
  // catalog could never offer it, and the server accepts it.
  expect([...picker('New channel routes to').options].map(option => option.value)).toEqual(['izzie', 'cto-assistant', 'pm']);
});

it('shows a duplicate ID inline instead of issuing a create', async () => {
  await render();
  await openAdd('slack-ops', 'Another ops');
  button('Create channel').click();
  await settle();
  expect(posts).toHaveLength(0);
  expect(alerts()).toContain('already exists');
});

it('tells a duplicate ID from a lost compare-and-swap by the reloaded list', async () => {
  await render();
  await openAdd();
  // Another writer declared the same id between the read and this create. The
  // server answers 409 for that AND for a stale revision; only the stored list
  // says which happened.
  writeFailures.push({ status: 409, body: { error: 'A global channel with this ID already exists' } });
  channels = [...channels, channel({ id: 'ops-telegram', name: 'Theirs' })];
  revision = 'r9';
  button('Create channel').click();
  await settle();
  expect(alerts()).toContain('already exists');
  expect(document.body.textContent).not.toContain('Another writer changed the global channels');
  // The draft survives, so the operator renames rather than retypes.
  expect((document.querySelector('[aria-label="New channel ID"]') as HTMLInputElement).value).toBe('ops-telegram');
});

it('reloads and says so when a create loses the compare-and-swap', async () => {
  await render();
  await openAdd();
  const before = reads;
  writeFailures.push({ status: 409, body: { error: 'Channel settings changed. Reload before saving.' } });
  channels = [channel({ name: 'Renamed elsewhere' })];
  revision = 'r9';
  button('Create channel').click();
  await settle();
  expect(reads).toBe(before + 1);
  expect(document.body.textContent).toContain('Another writer changed the global channels');
  expect(document.body.textContent).toContain('Renamed elsewhere');
  expect(picker('New channel provider')).toBeTruthy();
});

it('deletes one channel through DELETE at the current revision', async () => {
  await render();
  deleteButton('Ops').click();
  await settle();
  expect(document.querySelector('[role="dialog"]')?.textContent).toContain('This removes the channel from this host');
  button('Delete channel').click();
  await settle();
  expect(deletes).toHaveLength(1);
  expect(deletes[0].url).toMatch(/\/api\/channels\/slack-ops\?revision=r1$/);
  expect(deletes[0].authorization).toBe('Bearer minted');
  expect(document.querySelector('[role="dialog"]')).toBeNull();
  expect(document.body.textContent).toContain('Ops deleted.');
  expect(document.body.textContent).not.toContain('slack · C123');
});

it('names the assistants a refused delete would strand, then forces explicitly', async () => {
  await render();
  writeFailures.push({ status: 409, body: { error: 'still bound', referenced_by: ['izzie', 'cto-assistant'] } });
  inertBindings = ['izzie', 'cto-assistant'];
  deleteButton('Personal mail').click();
  await settle();
  button('Delete channel').click();
  await settle();
  // The first click never forces: the operator has not been told the cost yet.
  expect(deletes).toHaveLength(1);
  expect(deletes[0].url).not.toContain('force');
  const dialog = document.querySelector('[role="dialog"]')!;
  expect(dialog.textContent).toContain('izzie, cto-assistant');
  expect(dialog.textContent).toContain('inert');
  button('Delete anyway').click();
  await settle();
  expect(deletes[1].url).toMatch(/\?revision=r1&force=true$/);
  expect(document.body.textContent).toContain('those bindings now address nothing');
  expect(document.body.textContent).toContain('izzie, cto-assistant');
});

it('reloads and says so when a delete loses the compare-and-swap', async () => {
  await render();
  const before = reads;
  writeFailures.push({ status: 409, body: { error: 'Channel settings changed. Reload before saving.' } });
  channels = [channel({ name: 'Renamed elsewhere' })];
  revision = 'r9';
  deleteButton('Ops').click();
  await settle();
  button('Delete channel').click();
  await settle();
  expect(reads).toBe(before + 1);
  expect(document.querySelector('[role="dialog"]')).toBeNull();
  expect(alerts()).toContain('Another writer changed the global channels');
  expect(document.body.textContent).toContain('Renamed elsewhere');
});

it('will not add or delete over an unsaved field edit', async () => {
  await render();
  box('gmail-personal enabled').click();
  await settle();
  // A create or delete publishes the STORED list; either would throw this edit
  // away without saying so.
  expect(button('Add channel').disabled).toBe(true);
  expect(deleteButton('Ops').disabled).toBe(true);
  expect(document.body.textContent).toContain('Save or discard these changes');
});

// Guarding only the ENTRY buttons left the list editable once an editor was
// open, and a create or delete that landed then called `apply(result)`, which
// replaces `channels` wholesale (critic HIGH). Two guards, tested separately:
// the list goes read-only, AND both writes re-check before going to the wire.
const listFieldset = () => document.querySelector('fieldset') as HTMLFieldSetElement;

it('makes the stored list read-only while either editor is open', async () => {
  await render();
  button('Add channel').click();
  await settle();
  expect(listFieldset().disabled).toBe(true);
  // The click an operator would make is inert, so the list cannot go dirty
  // underneath the open form and Save has nothing to publish.
  box('gmail-personal enabled').click();
  await settle();
  expect(box('gmail-personal enabled').checked).toBe(true);
  expect(button('Save channels').disabled).toBe(true);
  button('Cancel').click();
  await settle();
  expect(listFieldset().disabled).toBe(false);
  deleteButton('Ops').click();
  await settle();
  expect(listFieldset().disabled).toBe(true);
});

/** Drive a checkbox past a disabled fieldset, which blocks clicks but not events. */
function forceEdit(label: string) {
  const input = box(label);
  input.checked = !input.checked;
  input.dispatchEvent(new Event('change'));
}

it('refuses a create while the list is dirty instead of overwriting the edit', async () => {
  await render();
  await openAdd();
  // The race the read-only fieldset cannot catch: a change that reaches the
  // binding anyway. Without the re-check, `apply(result)` replaces `channels`
  // and the edit disappears under an "added" notice.
  forceEdit('gmail-personal enabled');
  await settle();
  button('Create channel').click();
  await settle();
  expect(posts).toHaveLength(0);
  expect(alerts()).toContain('unsaved changes');
  expect(box('gmail-personal enabled').checked).toBe(false);
});

it('refuses a delete while the list is dirty instead of overwriting the edit', async () => {
  await render();
  deleteButton('Ops').click();
  await settle();
  forceEdit('gmail-personal enabled');
  await settle();
  button('Delete channel').click();
  await settle();
  expect(deletes).toHaveLength(0);
  expect(alerts()).toContain('unsaved changes');
  expect(box('gmail-personal enabled').checked).toBe(false);
});

// The confirmation claimed `aria-modal` while focus stayed on the Delete
// button behind it, so Escape reached a handler on the backdrop that nothing
// was focused inside (critic MEDIUM-2).
it('closes the delete confirmation on Escape and hands focus back', async () => {
  await render();
  deleteButton('Ops').click();
  await settle();
  expect(document.activeElement?.textContent).toBe('Cancel');
  document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  await settle();
  expect(document.querySelector('[role="dialog"]')).toBeNull();
  expect(deletes).toHaveLength(0);
  // Focus returns to what opened it, not to the top of the document.
  expect(document.activeElement).toBe(deleteButton('Ops'));
});

it('leaves the confirmation open on Escape while the delete is in flight', async () => {
  await render();
  let release = () => {};
  writeFailures.push({ status: 409, body: { error: 'still bound', referenced_by: ['izzie'] } });
  const gate = new Promise<void>(resolve => { release = resolve; });
  const realFetch = globalThis.fetch;
  vi.stubGlobal('fetch', async (input: RequestInfo | URL, init?: RequestInit) => {
    if (init?.method === 'DELETE') await gate;
    return realFetch(input, init);
  });
  deleteButton('Ops').click();
  await settle();
  button('Delete channel').click();
  await settle();
  document.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
  await settle();
  // Dismissing mid-flight would leave the operator with no answer to a write
  // that is still going to land.
  expect(document.querySelector('[role="dialog"]')).not.toBeNull();
  release();
  await settle();
});

it('states why the create failed when the reload after the conflict also fails', async () => {
  await render();
  await openAdd();
  writeFailures.push({ status: 409, body: { error: 'Channel settings changed. Reload before saving.' } });
  readFails = true;
  button('Create channel').click();
  await settle();
  // The reload is what tells a duplicate id from a lost race. When it fails
  // too, the create's own refusal is the only thing left to say (critic LOW).
  expect(alerts()).toContain('Channel settings changed');
});
