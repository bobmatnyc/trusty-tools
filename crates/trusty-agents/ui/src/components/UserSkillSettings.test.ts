import { afterEach, beforeEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
vi.mock('../stores/app', () => ({ tmApi: vi.fn() }));
import { tmApi } from '../stores/app';
import UserSkillSettings from './UserSkillSettings.svelte';
import { userSkillSettings } from '../lib/userSkillSettings';
const snapshot = { path: '/home/user', revision: 'one', sources: [{ id: '.claude/skills', label: 'Claude skills', path: '/home/user/.claude/skills', enabled: true, skills: [{ name: 'Transit', description: 'Train guidance', path: '/home/user/.claude/skills/transit/SKILL.md' }] }] };
let app: ReturnType<typeof mount> | undefined;
async function settle() { await Promise.resolve(); await tick(); await Promise.resolve(); await tick(); }
beforeEach(() => { userSkillSettings.set({ data:null, loading:false, saving:false, error:'', notice:'', draft:{} }); vi.mocked(tmApi).mockResolvedValue(snapshot); });
afterEach(async () => { if (app) await unmount(app); app = undefined; document.body.innerHTML = ''; vi.resetAllMocks(); });
it('lists user skills and saves global source switches with a revision', async () => {
  app = mount(UserSkillSettings, { target: document.body }); await settle();
  expect(document.body.textContent).toContain('Transit');
  expect(document.body.textContent).toContain('all assistants');
  vi.mocked(tmApi).mockResolvedValue({ ...snapshot, revision: 'two', sources: [{ ...snapshot.sources[0], enabled: false }] });
  (document.querySelector('input') as HTMLInputElement).click(); await settle();
  expect(tmApi).toHaveBeenLastCalledWith('/api/user-skills', { method: 'PATCH', body: JSON.stringify({ revision: 'one', sources: [{ id: '.claude/skills', enabled: false }] }) });
  expect(document.body.textContent).toContain('Saved for all projects');
});
it('retains a conflicted draft and restores server state on reload', async () => {
  app = mount(UserSkillSettings, { target: document.body }); await settle();
  vi.mocked(tmApi).mockRejectedValueOnce(new Error('409 conflict'));
  (document.querySelector('input') as HTMLInputElement).click(); await settle();
  expect((document.querySelector('input') as HTMLInputElement).checked).toBe(false);
  expect(document.querySelector('[role=alert]')?.textContent).toContain('Not saved');
  document.querySelector('button')!.click(); await settle();
  expect((document.querySelector('input') as HTMLInputElement).checked).toBe(true);
});
it('shows a service failure rather than an empty registry', async () => {
  vi.mocked(tmApi).mockRejectedValue(new Error('Unavailable'));
  app = mount(UserSkillSettings, { target: document.body }); await settle();
  expect(document.querySelector('[role=alert]')?.textContent).toContain('could not be loaded');
  expect(document.body.textContent).not.toContain('No supported');
});
it('shows source scan diagnostics alongside available user skills', async () => {
  vi.mocked(tmApi).mockResolvedValue({ ...snapshot, sources: [{ ...snapshot.sources[0], error: 'Folder could not be read' }] });
  app = mount(UserSkillSettings, { target: document.body }); await settle();
  expect(document.querySelector('[role=alert]')?.textContent).toContain('Folder could not be read');
  expect(document.body.textContent).toContain('Transit');
});
it('keeps an in-flight save authoritative after closing and reopening the pane', async () => {
  app = mount(UserSkillSettings, { target: document.body }); await settle();
  let complete!: (value: typeof snapshot) => void;
  vi.mocked(tmApi).mockImplementationOnce(() => new Promise(resolve => { complete = resolve as typeof complete; }));
  (document.querySelector('input') as HTMLInputElement).click(); await settle();
  await unmount(app); app = mount(UserSkillSettings, { target: document.body }); await settle();
  expect(tmApi).toHaveBeenCalledTimes(2);
  expect((document.querySelector('input') as HTMLInputElement).disabled).toBe(true);
  complete({ ...snapshot, revision: 'two', sources: [{ ...snapshot.sources[0], enabled: false }] }); await settle();
  expect((document.querySelector('input') as HTMLInputElement).checked).toBe(false);
  (document.querySelector('input') as HTMLInputElement).click(); await settle();
  const calls = vi.mocked(tmApi).mock.calls;
  expect(JSON.parse(calls[calls.length - 1][1]!.body as string).revision).toBe('two');
});
