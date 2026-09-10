import { afterEach, expect, it, vi } from 'vitest';
import { mount, tick, unmount } from 'svelte';
vi.mock('../stores/app', () => ({ tmApi: vi.fn(), getCurrentApiToken: () => '' }));
vi.mock('../lib/api-config', () => ({ apiBase: () => '' }));
vi.mock('../lib/transport', () => ({ isDesktop: () => false }));
vi.mock('../stores/workspace', () => ({ setAssistantDefaultProjects: vi.fn() }));
import { tmApi } from '../stores/app';
import AssistantProjectSettings from './AssistantProjectSettings.svelte';
let component: ReturnType<typeof mount> | undefined;
async function settle() { for (let i = 0; i < 8; i++) { await Promise.resolve(); await tick(); } }
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.innerHTML = ''; vi.unstubAllGlobals(); vi.clearAllMocks(); });
it('saves assistant selections before knowledge initialization without mutating chat attachments', async () => {
  vi.mocked(tmApi).mockResolvedValue([{ id: 'folder', name: 'Documents', path: '/documents', available: true }]);
  const fetcher = vi.fn().mockResolvedValueOnce({ ok: true, json: async () => ({ assistant: 'alice', pipeline: null }) })
    .mockResolvedValueOnce({ ok: true, json: async () => ({ assistant: 'alice', pipeline: { assistant_id: 'alice', revision: 'r1', assistant_projects: ['/documents'], projects_by_chat: {} }, assistant_projects_status: [{ path: '/documents', name: 'Documents', available: true }] }) });
  vi.stubGlobal('fetch', fetcher);
  component = mount(AssistantProjectSettings, { target: document.body, props: { agentName: 'alice' } }); await settle();
  document.querySelector<HTMLInputElement>('input[type=checkbox]')!.click(); await settle();
  expect(fetcher.mock.calls[1][1]).toMatchObject({ method: 'PUT', body: '{"revision":"","scope":"assistant","projects":["/documents"]}' });
  expect(document.querySelector<HTMLInputElement>('input[type=checkbox]')!.checked).toBe(true);
  expect(document.querySelector('[role=alert]')).toBeNull();
});
it('retains unavailable saved folders and permits their removal', async () => {
  vi.mocked(tmApi).mockResolvedValue([]);
  const value = { assistant: 'alice', pipeline: { assistant_id: 'alice', revision: 'r1', assistant_projects: ['/gone'], projects_by_chat: {} }, assistant_projects_status: [{ path: '/gone', name: 'Gone', available: false }] };
  const fetcher = vi.fn().mockResolvedValueOnce({ ok: true, json: async () => value })
    .mockResolvedValueOnce({ ok: true, json: async () => ({ ...value, pipeline: { ...value.pipeline, revision: 'r2', assistant_projects: [] } }) });
  vi.stubGlobal('fetch', fetcher);
  component = mount(AssistantProjectSettings, { target: document.body, props: { agentName: 'alice' } }); await settle();
  const input = document.querySelector<HTMLInputElement>('input[type=checkbox]')!;
  expect(input.checked).toBe(true); expect(input.disabled).toBe(false);
  expect(document.body.textContent).toContain('Unavailable');
  input.click(); await settle();
  expect(JSON.parse(fetcher.mock.calls[1][1].body).projects).toEqual([]);
});
