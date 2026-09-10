import { beforeEach, afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
import { writable } from 'svelte/store';
vi.mock('../stores/app', () => ({ activeAgentId: writable('alice') }));
vi.mock('../lib/projectTools', () => ({ fetchProjectSkills: vi.fn(), updateProjectSkillSources: vi.fn(), fetchProjectTools: vi.fn(), indexProject: vi.fn(), importProject: vi.fn() }));
import ProjectConfiguration from './ProjectConfiguration.svelte';
import { activeAgentId } from '../stores/app';
import { fetchProjectTools, importProject, fetchProjectSkills, updateProjectSkillSources } from '../lib/projectTools';
const root = { id: 'project', name: 'Project', path: '/registered/project' };
const status = { path: root.path, index: { connected: true, id: 'project-index', status: 'ready' }, knowledge: { available: true, agent: 'alice', tree: 'okg://alice', index: 'alice-index' } };
const skillStatus = { path: root.path, revision: 'rev1', sources: [{ id: 'claude', label: 'Claude skills', path: '/registered/project/.claude/skills', enabled: true, skills: [{ name: 'Code review', description: 'Review project code', path: '/registered/project/.claude/skills/review/SKILL.md' }] }] };
beforeEach(() => { vi.mocked(fetchProjectSkills).mockResolvedValue(skillStatus); });
let component: ReturnType<typeof mount> | undefined;
async function settle() { await Promise.resolve(); await tick(); await Promise.resolve(); await tick(); }
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.innerHTML = ''; activeAgentId.set('alice'); vi.resetAllMocks(); });
it('shows partial import failures and pending search items', async () => {
  vi.mocked(fetchProjectTools).mockResolvedValue(status);
  vi.mocked(importProject).mockResolvedValue({ path: root.path, agent: 'alice', tree: 'okg://alice', result: { ingest: { scanned: 4, ingested: 2, updated: 1, skipped: 0, errors: ['One document could not be read'] }, index: { indexed: 0, pending: 3, reason: 'Search unavailable' } } });
  component = mount(ProjectConfiguration, { target: document.body, props: { root, onClose: vi.fn() } }); await settle();
  const button = [...document.querySelectorAll('button')].find(item => item.textContent?.includes('Import project contents'))!;
  button.click(); await settle();
  expect(importProject).toHaveBeenCalledWith(root.path, 'alice');
  expect(document.body.textContent).toContain('3 pending');
  expect(document.body.textContent).toContain('One document could not be read');
  expect(document.body.textContent).toContain('Search unavailable');
});
it('ignores previous assistant status after selection changes', async () => {
  let complete!: (value: typeof status) => void;
  vi.mocked(fetchProjectTools).mockReturnValueOnce(new Promise(resolve => { complete = resolve; })).mockResolvedValueOnce({ ...status, knowledge: { ...status.knowledge, agent: 'bob', tree: 'okg://bob' } });
  component = mount(ProjectConfiguration, { target: document.body, props: { root, onClose: vi.fn() } }); await settle();
  activeAgentId.set('bob'); await settle(); complete(status); await settle();
  expect(document.body.textContent).toContain('okg://bob');
  expect(document.body.textContent).not.toContain('okg://alice');
});
it('disables importing without a bound knowledge store', async () => {
  vi.mocked(fetchProjectTools).mockResolvedValue({ ...status, knowledge: { ...status.knowledge, available: false, reason: 'No bound store' } });
  component = mount(ProjectConfiguration, { target: document.body, props: { root, onClose: vi.fn() } }); await settle();
  const button = [...document.querySelectorAll('button')].find(item => item.textContent?.includes('Import project contents'))!;
  expect(button.disabled).toBe(true); expect(importProject).not.toHaveBeenCalled();
  expect(document.body.textContent).toContain('No bound store');
});
it('keeps a late import result out of a newly selected assistant', async () => {
  vi.mocked(fetchProjectTools).mockImplementation(async (_path, agent) => ({ ...status, knowledge: { ...status.knowledge, agent: agent!, tree: `okg://${agent}` } }));
  let complete!: (value: Awaited<ReturnType<typeof importProject>>) => void;
  vi.mocked(importProject).mockReturnValue(new Promise(resolve => { complete = resolve; }));
  component = mount(ProjectConfiguration, { target: document.body, props: { root, onClose: vi.fn() } }); await settle();
  [...document.querySelectorAll('button')].find(item => item.textContent?.includes('Import project contents'))!.click(); await settle();
  activeAgentId.set('bob'); await settle();
  complete({ path: root.path, agent: 'alice', tree: 'okg://alice', result: { ingest: { scanned: 99, ingested: 99, updated: 0, skipped: 0, errors: [] }, index: { indexed: 99, pending: 0 } } }); await settle();
  expect(document.body.textContent).toContain('okg://bob');
  expect(document.body.textContent).not.toContain('99');
});


it('lists discovered skills and saves source enablement using its revision', async () => {
  vi.mocked(fetchProjectTools).mockResolvedValue(status);
  vi.mocked(updateProjectSkillSources).mockResolvedValue({ ...skillStatus, revision: 'rev2', sources: [{ ...skillStatus.sources[0], enabled: false }] });
  component = mount(ProjectConfiguration, { target: document.body, props: { root, onClose: vi.fn() } }); await settle();
  expect(document.body.textContent).toContain('Code review');
  expect(document.body.textContent).toContain('discovery does not grant extra tools');
  const checkbox = document.querySelector('[aria-label="Project skills"] input') as HTMLInputElement;
  expect(checkbox.checked).toBe(true);
  checkbox.click(); await settle();
  expect(updateProjectSkillSources).toHaveBeenCalledWith(root.path, 'rev1', [{ id: 'claude', enabled: false }]);
  expect(checkbox.checked).toBe(false);
  expect(document.body.textContent).toContain('Saved. Changes apply to new turns.');
});

it('retains unsaved source selection on conflict without claiming success', async () => {
  vi.mocked(fetchProjectTools).mockResolvedValue(status);
  vi.mocked(updateProjectSkillSources).mockRejectedValue(new Error('409 conflict'));
  component = mount(ProjectConfiguration, { target: document.body, props: { root, onClose: vi.fn() } }); await settle();
  const checkbox = document.querySelector('[aria-label="Project skills"] input') as HTMLInputElement;
  checkbox.click(); await settle();
  expect(checkbox.checked).toBe(false);
  expect(document.body.textContent).toContain('Not saved: these settings changed elsewhere.');
  expect(document.body.textContent).not.toContain('Saved. Changes apply');
  [...document.querySelectorAll('button')].find(button => button.textContent?.includes('Reload skill settings'))!.click(); await settle();
  expect((document.querySelector('[aria-label="Project skills"] input') as HTMLInputElement).checked).toBe(true);
});

it('keeps index and knowledge settings usable if skill discovery fails', async () => {
  vi.mocked(fetchProjectTools).mockResolvedValue(status);
  vi.mocked(fetchProjectSkills).mockRejectedValue(new Error('Discovery unavailable'));
  component = mount(ProjectConfiguration, { target: document.body, props: { root, onClose: vi.fn() } }); await settle();
  expect(document.body.textContent).toContain('Project skills could not be loaded');
  expect(document.body.textContent).toContain('project-index');
  expect(document.body.textContent).toContain('okg://alice');
});

it('retains the persisted project skill state when the assistant changes during saving', async () => {
  vi.mocked(fetchProjectTools).mockResolvedValue(status);
  let finish!: (value: typeof skillStatus) => void;
  vi.mocked(updateProjectSkillSources).mockReturnValue(new Promise(resolve => finish = resolve));
  component = mount(ProjectConfiguration, { target: document.body, props: { root, onClose: vi.fn() } }); await settle();
  (document.querySelector('[aria-label="Project skills"] input') as HTMLInputElement).click(); await settle();
  activeAgentId.set('bob'); await settle();
  finish({ ...skillStatus, revision: 'rev2', sources: [{ ...skillStatus.sources[0], enabled: false }] }); await settle();
  expect((document.querySelector('[aria-label="Project skills"] input') as HTMLInputElement).checked).toBe(false);
  expect(document.body.textContent).toContain('Saved. Changes apply');
  expect(fetchProjectSkills).toHaveBeenCalledTimes(1);
  vi.mocked(updateProjectSkillSources).mockResolvedValue({ ...skillStatus, revision: 'rev3' });
  (document.querySelector('[aria-label="Project skills"] input') as HTMLInputElement).click(); await settle();
  expect(updateProjectSkillSources).toHaveBeenLastCalledWith(root.path, 'rev2', [{ id: 'claude', enabled: true }]);
});
