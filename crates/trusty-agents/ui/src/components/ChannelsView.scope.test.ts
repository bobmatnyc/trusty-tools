// The scope toggle and the read-only "also routed here" panel (#7609 slice 6).
//
// Why: this is the one test that mounts the WHOLE Channels view against a
// stubbed `fetch` with no module mocked, because the property under test spans
// three components and two routes — that an operator can see both scopes, and
// that an assistant's own list no longer reads as the complete picture of what
// wakes it. `ChannelsView.test.ts` keeps its module mocks for the per-assistant
// behaviours it was written for.

import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, tick, unmount } from 'svelte';
import ChannelsView from './ChannelsView.svelte';
import { activeAgentId, catalogAgents } from '../stores/app';
import type { GlobalChannel } from '../lib/channels';

const globalChannel = (overrides: Partial<GlobalChannel> = {}): GlobalChannel => ({
  id: 'gmail-personal', name: 'Personal mail', provider: 'gmail', target: '',
  enabled: true, send_enabled: false, receive_enabled: true,
  transport: 'gmail_history', poll_interval_secs: 60, instructions: '',
  event_types: [], route_to: ['izzie'],
  ingest_filter: { label_ids: [] },
  wake_filter: { from: [], include_labels: [], exclude_labels: [], subject_contains: [], snippet_contains: [] },
  ...overrides,
});

const assistantConfig = {
  agent: 'izzie', revision: 'r1',
  providers: [{ id: 'slack', name: 'Slack', configured: true, can_send: true, can_read: true, can_receive: true }],
  bindings: [{
    id: 'izzie-slack', name: 'Izzie standups', provider: 'slack', target: 'C123',
    enabled: true, send_enabled: true, receive_enabled: false,
    filter: { from: [], include_labels: [], exclude_labels: [], subject_contains: [], snippet_contains: [] },
    instructions: '',
  }],
};

let view: ReturnType<typeof mount> | undefined;
let globals: GlobalChannel[] = [];

async function settle() { for (let i = 0; i < 10; i++) { await Promise.resolve(); await tick(); } }
const button = (text: string) => [...document.querySelectorAll('button')].find(b => b.textContent?.trim() === text)!;

beforeEach(async () => {
  globals = [globalChannel(), globalChannel({ id: 'slack-ops', name: 'Ops room', provider: 'slack', target: 'C999', route_to: ['cto-assistant'] })];
  catalogAgents.set([{ name: 'izzie' }, { name: 'cto-assistant' }]);
  activeAgentId.set('izzie');
  vi.stubGlobal('fetch', async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.endsWith('/api/config')) return new Response(JSON.stringify({ auth_required: false, channel_write_token: 'minted' }), { status: 200 });
    if (url.endsWith('/api/agents')) return new Response(JSON.stringify({ agents: [{ name: 'izzie' }, { name: 'cto-assistant' }] }), { status: 200 });
    if (url.endsWith('/api/channels')) return new Response(JSON.stringify({ scope: 'global', revision: 'g1', channels: globals, providers: [] }), { status: 200 });
    if (url.includes('/api/agents/izzie/channels')) return new Response(JSON.stringify(assistantConfig), { status: 200 });
    return new Response('{}', { status: 404 });
  });
  view = mount(ChannelsView, { target: document.body });
  await settle();
});

afterEach(async () => {
  if (view) await unmount(view);
  view = undefined;
  document.body.innerHTML = '';
  vi.unstubAllGlobals();
  activeAgentId.set(null);
  catalogAgents.set([]);
});

it('renders the assistant list first and the global list after the toggle', async () => {
  expect(button('Assistant').getAttribute('aria-pressed')).toBe('true');
  expect(document.body.textContent).toContain('Izzie standups');
  // The Global scope is not merely hidden before it is chosen; it is unmounted.
  expect(document.querySelector('[data-global-channels]')).toBeNull();

  button('Global').click();
  await settle();
  expect(button('Global').getAttribute('aria-pressed')).toBe('true');
  const globalPanel = document.querySelector('[data-global-channels]') as HTMLElement;
  expect(globalPanel.textContent).toContain('Personal mail');
  expect(globalPanel.textContent).toContain('Ops room');
  expect(globalPanel.style.display).not.toBe('none');

  // Back to Assistant: the assistant list is still the one that was loaded,
  // never re-fetched, and the global editor keeps its state under the toggle.
  button('Assistant').click();
  await settle();
  expect(document.body.textContent).toContain('Izzie standups');
  expect((document.querySelector('[data-global-channels]') as HTMLElement).style.display).toBe('none');
});

it('names only the global channels routed to this assistant, read-only', async () => {
  const routed = document.querySelector('[aria-label="Global channels routed here"]') as HTMLElement;
  expect(routed.textContent).toContain('Personal mail');
  // `Ops room` routes to another assistant and must not appear here.
  expect(routed.textContent).not.toContain('Ops room');
  // Read-only: nothing in the panel edits the global record.
  expect(routed.querySelectorAll('input, select, textarea')).toHaveLength(0);
});

it('hands the operator to the Global scope from the routed-here panel', async () => {
  const routed = document.querySelector('[aria-label="Global channels routed here"]') as HTMLElement;
  (routed.querySelector('button') as HTMLButtonElement).click();
  await settle();
  expect(button('Global').getAttribute('aria-pressed')).toBe('true');
  expect((document.querySelector('[data-global-channels]') as HTMLElement).textContent).toContain('Ops room');
});

it('says nothing when no global channel routes here', async () => {
  await unmount(view!);
  document.body.innerHTML = '';
  globals = [globalChannel({ route_to: ['cto-assistant'] })];
  view = mount(ChannelsView, { target: document.body });
  await settle();
  expect(document.querySelector('[aria-label="Global channels routed here"]')).toBeNull();
  expect(document.body.textContent).toContain('Izzie standups');
});
