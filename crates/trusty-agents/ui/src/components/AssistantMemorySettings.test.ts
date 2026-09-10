import { afterEach, expect, it, vi } from 'vitest';
import { mount, tick, unmount } from 'svelte';
vi.mock('../stores/app', () => ({ getCurrentApiToken: () => 'token' }));
vi.mock('../lib/api-config', () => ({ apiBase: () => '' }));
import AssistantMemorySettings from './AssistantMemorySettings.svelte';
const policy = (enabled = false) => ({ assistant_id: 'alice', namespace: 'alice-palace', revision: enabled ? 'r2' : 'r1', cross_palace_query: enabled });
let component: ReturnType<typeof mount> | undefined;
async function settle() { for (let i = 0; i < 8; i++) { await Promise.resolve(); await tick(); } }
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.innerHTML = ''; vi.unstubAllGlobals(); });
it('persists only the opted-in query policy and waits for acknowledgement', async () => {
  let finish!: (value: unknown) => void;
  const fetcher = vi.fn().mockResolvedValueOnce({ ok: true, json: async () => policy() }).mockReturnValueOnce(new Promise(resolve => { finish = resolve; }));
  vi.stubGlobal('fetch', fetcher);
  component = mount(AssistantMemorySettings, { target: document.body, props: { agentName: 'alice' } }); await settle();
  const input = document.querySelector('input')!;
  expect(input.checked).toBe(false); input.click(); await settle();
  expect(input.checked).toBe(false); expect(input.disabled).toBe(true);
  expect(JSON.parse(fetcher.mock.calls[1][1].body)).toEqual({ revision: 'r1', cross_palace_query: true });
  finish({ ok: true, json: async () => policy(true) }); await settle();
  expect(input.checked).toBe(true); expect(document.body.textContent).toContain('alice-palace');
});
it('reloads stale settings without replaying the rejected write', async () => {
  const fetcher = vi.fn().mockResolvedValueOnce({ ok: true, json: async () => policy() })
    .mockResolvedValueOnce({ ok: false, status: 409, json: async () => ({ error: 'stale' }) })
    .mockResolvedValueOnce({ ok: true, json: async () => policy() });
  vi.stubGlobal('fetch', fetcher);
  component = mount(AssistantMemorySettings, { target: document.body, props: { agentName: 'alice' } }); await settle();
  document.querySelector('input')!.click(); await settle();
  expect(document.body.textContent).toContain('repeat your change');
  expect(fetcher.mock.calls.filter(call => call[1].method === 'PATCH')).toHaveLength(1);
});
