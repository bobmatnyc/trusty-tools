import { afterEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
import { get, writable, type Writable } from 'svelte/store';
vi.mock('svelte/transition', () => ({ slide: () => ({ duration: 0 }) }));
vi.mock('../lib/projectTools', () => ({ fetchProjectTools: vi.fn().mockResolvedValue({ index: {}, available: true }), indexProject: vi.fn(), importProject: vi.fn() }));
vi.mock('../lib/transport', () => ({ isDesktop: () => true }));
vi.mock('../stores/app', () => ({ activeAgentId: writable(null), projects: writable([]), projectsList: writable([]), fetchProjects: vi.fn().mockResolvedValue(undefined), tmApi: vi.fn() }));
vi.mock('../lib/workspaceFiles', () => ({
  folderError: () => 'This folder is missing. Locate it.', listWorkspaceFiles: vi.fn(), chooseWorkspaceRoot: vi.fn(), registerWorkspaceRoot: vi.fn(), registerProjectFolder: vi.fn(), DESKTOP_FILES_MESSAGE: 'Desktop only',
}));
vi.mock('../stores/workspace', () => ({
  chatWorkspaceKey: writable('chat-a'), registeredWorkspaceRoots: writable([]), activeRoot: writable(null), workspaceRoots: writable([]), openedFile: writable(null), projectRoots: writable([]), currentChatRoots: writable([]), chatProjectPath: writable(null), workspaceError: writable(null),
  loadWorkspaceRoots: vi.fn(), rememberWorkspaceRoot: vi.fn(), attachRootToChat: vi.fn(), detachRootFromChat: vi.fn(), selectChatRoot: vi.fn(),
}));
import FileNavigator from './FileNavigator.svelte';
import { activeRoot, openedFile, projectRoots, currentChatRoots, registeredWorkspaceRoots, attachRootToChat, chatWorkspaceKey, loadWorkspaceRoots } from '../stores/workspace';
import { projectsList, fetchProjects, tmApi } from '../stores/app';
import { listWorkspaceFiles, chooseWorkspaceRoot } from '../lib/workspaceFiles';
let component: ReturnType<typeof mount> | undefined;
afterEach(async () => { if (component) await unmount(component); component = undefined; activeRoot.set(null); openedFile.set(null); document.body.innerHTML = ''; (currentChatRoots as Writable<import('../lib/workspaceFiles').WorkspaceRoot[]>).set([]); registeredWorkspaceRoots.set([]); vi.clearAllMocks(); });
describe('file navigator asynchronous navigation', () => {
  it('ignores late directory contents from the previous root and opens files from the current root', async () => {
    let finishOld!: (value: { entries: {name: string; path: string; is_dir: boolean; size: null}[] }) => void;
    vi.mocked(listWorkspaceFiles).mockReturnValueOnce(new Promise(resolve => { finishOld = resolve; })).mockResolvedValueOnce({ entries: [{ name: 'current.md', path: 'current.md', is_dir: false, size: 7 }] });
    const old = { id: 'old', name: 'Old', path: '/old' };
    activeRoot.set(old);
    (currentChatRoots as Writable<import('../lib/workspaceFiles').WorkspaceRoot[]>).set([old]);
    component = mount(FileNavigator, { target: document.body });
    await tick();
    (document.querySelector('button[title="/old"]') as HTMLButtonElement).click(); await tick();
    const current = { id: 'current', name: 'Current', path: '/current' };
    activeRoot.set(current);
    await tick(); await Promise.resolve(); await tick();
    finishOld({ entries: [{ name: 'stale.md', path: 'stale.md', is_dir: false, size: null }] });
    await Promise.resolve(); await tick();
    expect(document.body.textContent).toContain('current.md');
    expect(document.body.textContent).not.toContain('stale.md');
    const file = document.querySelector('button[title="current.md"]') as HTMLButtonElement;
    file.click();
    expect(get(openedFile)).toEqual({ root: current, path: 'current.md' });
  });
});


it('omits unavailable attached folders from project lists without reading them', async () => {
  const root = { id: 'missing', name: 'Missing', path: '/gone', available: false };
  activeRoot.set(root); (projectRoots as Writable<import("../lib/workspaceFiles").WorkspaceRoot[]>).set([root]); (currentChatRoots as Writable<import("../lib/workspaceFiles").WorkspaceRoot[]>).set([root]);
  component = mount(FileNavigator, { target: document.body });
  await tick();
  expect(document.querySelector('[aria-label="Projects attached to this chat"]')?.textContent).not.toContain('Missing');
  expect(listWorkspaceFiles).not.toHaveBeenCalled();
  expect(document.querySelector('[aria-label="Detach Missing from this chat"]')).toBeNull();
  (projectRoots as Writable<import("../lib/workspaceFiles").WorkspaceRoot[]>).set([]); (currentChatRoots as Writable<import("../lib/workspaceFiles").WorkspaceRoot[]>).set([]);
});


it('hides dotfiles and dotfolders by default and reveals them on request', async () => {
  vi.mocked(listWorkspaceFiles).mockResolvedValue({ entries: [
    { name: '.env', path: '.env', is_dir: false, size: 10 },
    { name: '.git', path: '.git', is_dir: true, size: null },
    { name: 'README.md', path: 'README.md', is_dir: false, size: 20 },
  ] });
  const root = { id: 'root', name: 'Root', path: '/root' };
  activeRoot.set(root); (currentChatRoots as Writable<import('../lib/workspaceFiles').WorkspaceRoot[]>).set([root]);
  component = mount(FileNavigator, { target: document.body });
  await tick();
  (document.querySelector('button[title="/root"]') as HTMLButtonElement).click();
  await tick(); await Promise.resolve(); await tick();
  expect(document.querySelector('button[title="README.md"]')).not.toBeNull();
  expect(document.querySelector('button[title=".env"]')).toBeNull();
  expect(document.querySelector('button[title=".git"]')).toBeNull();
  const toggle = document.querySelector('[aria-label="Show hidden files"]') as HTMLButtonElement;
  toggle.click(); await tick();
  expect(document.querySelector('button[title=".env"]')).not.toBeNull();
  expect(document.querySelector('button[title=".git"]')).not.toBeNull();
  expect(toggle.getAttribute('aria-pressed')).toBe('true');
  toggle.click(); await tick();
  expect(document.querySelector('button[title=".env"]')).toBeNull();
});


it('starts with current chat projects, drills into files and returns to the landing', async () => {
  const root = { id: 'root', name: 'Root', path: '/root' };
  (currentChatRoots as Writable<import('../lib/workspaceFiles').WorkspaceRoot[]>).set([root]);
  vi.mocked(listWorkspaceFiles).mockResolvedValue({ entries: [] });
  component = mount(FileNavigator, { target: document.body }); await tick();
  expect(document.querySelector('[aria-label="Projects attached to this chat"]')).not.toBeNull();
  expect(listWorkspaceFiles).not.toHaveBeenCalled();
  (document.querySelector('button[title="/root"]') as HTMLButtonElement).click(); await tick();
  expect(document.body.textContent).toContain('Back to Projects');
  expect(listWorkspaceFiles).toHaveBeenCalledWith('root', '');
  const back = [...document.querySelectorAll('button')].find(button => button.textContent?.includes('Back to Projects'))!;
  back.click(); await tick();
  expect(document.querySelector('[aria-label="Projects attached to this chat"]')).not.toBeNull();
  (document.querySelector('[aria-label="Configure Root"]') as HTMLButtonElement).click(); await tick();
  expect(document.querySelector('[aria-label="Project configuration"]')).not.toBeNull();
  (document.querySelector('[aria-label="Back to projects"]') as HTMLButtonElement).click(); await tick();
  expect(document.querySelector('[aria-label="Project configuration"]')).toBeNull();
});

it('offers only unattached registered projects and resets the drill-down on chat switch', async () => {
  const root = { id: 'root', name: 'Root', path: '/root' }, next = { id: 'next', name: 'Next', path: '/next' };
  (currentChatRoots as Writable<import('../lib/workspaceFiles').WorkspaceRoot[]>).set([root]);
  registeredWorkspaceRoots.set([root, next, { id: 'gone', name: 'Gone', path: '/gone', available: false }]);
  vi.mocked(listWorkspaceFiles).mockResolvedValue({ entries: [] });
  component = mount(FileNavigator, { target: document.body }); await tick();
  [...document.querySelectorAll('button')].find(button => button.textContent?.includes('Add project'))!.click(); await tick();
  const select = document.querySelector('#registered-project') as HTMLSelectElement;
  expect([...select.options].map(option => option.value)).toEqual(['', 'next']);
  select.value = 'next'; select.dispatchEvent(new Event('change')); await tick();
  expect(attachRootToChat).toHaveBeenCalledWith(next);
  (document.querySelector('button[title="/root"]') as HTMLButtonElement).click(); await tick();
  (chatWorkspaceKey as Writable<string>).set('chat-b'); await tick();
  expect(document.body.textContent).not.toContain('Back to Projects');
});


it('refreshes the project registry before reconciling externally added folders', async () => {
  component = mount(FileNavigator, { target: document.body }); await tick();
  vi.mocked(loadWorkspaceRoots).mockClear();
  vi.mocked(fetchProjects).mockImplementationOnce(async () => {
    projectsList.set([{ id: 'new', name: 'New', path: '/new', status: 'idle' }]);
  });
  (document.querySelector('[aria-label="Refresh projects"]') as HTMLButtonElement).click();
  await Promise.resolve(); await tick();
  expect(fetchProjects).toHaveBeenLastCalledWith(true);
  expect(loadWorkspaceRoots).toHaveBeenLastCalledWith(['/new']);
  expect(loadWorkspaceRoots).toHaveBeenCalledTimes(1);
  projectsList.set([]);
});

it('keeps registered choices available and displays registry refresh failures', async () => {
  registeredWorkspaceRoots.set([{ id: 'kept', name: 'Kept', path: '/kept' }]);
  component = mount(FileNavigator, { target: document.body }); await tick();
  vi.mocked(loadWorkspaceRoots).mockClear();
  vi.mocked(fetchProjects).mockRejectedValueOnce(new Error('offline'));
  (document.querySelector('[aria-label="Refresh projects"]') as HTMLButtonElement).click();
  await Promise.resolve(); await tick();
  expect(document.querySelector('[role="alert"]')?.textContent).toContain('Registered projects could not be loaded');
  expect(get(registeredWorkspaceRoots)).toHaveLength(1);
  expect(loadWorkspaceRoots).not.toHaveBeenCalled();
});

async function openFolderPicker() {
  [...document.querySelectorAll('button')].find(button => button.textContent?.includes('Add project'))!.click(); await tick();
  const picker = [...document.querySelectorAll('button')].find(button => button.textContent?.includes('Choose folder'));
  expect(picker, 'Add project must allow choosing an unregistered directory').toBeDefined();
  picker!.click(); await Promise.resolve(); await tick();
}
it('registers a canonical chosen directory before attaching it to the chat', async () => {
  const root = { id: 'new', name: 'New folder', path: '/canonical/new' };
  let completeRegistration!: () => void;
  vi.mocked(chooseWorkspaceRoot).mockResolvedValueOnce(root);
  vi.mocked(tmApi).mockImplementationOnce(() => new Promise(resolve => { completeRegistration = () => resolve({ path: root.path }); }));
  component = mount(FileNavigator, { target: document.body }); await tick();
  await openFolderPicker();
  expect(tmApi).toHaveBeenCalledWith('/api/projects', { method: 'POST', body: JSON.stringify({ path: root.path }) });
  expect(attachRootToChat).not.toHaveBeenCalled();
  completeRegistration(); await Promise.resolve(); await tick();
  expect(attachRootToChat).toHaveBeenCalledWith(root);
});
it('cancelling the native directory picker does not register or attach anything', async () => {
  vi.mocked(chooseWorkspaceRoot).mockResolvedValueOnce(null);
  component = mount(FileNavigator, { target: document.body }); await tick(); await openFolderPicker();
  expect(tmApi).not.toHaveBeenCalled(); expect(attachRootToChat).not.toHaveBeenCalled();
});
it('preserves the chat when project registration fails', async () => {
  vi.mocked(chooseWorkspaceRoot).mockResolvedValueOnce({ id: 'new', name: 'New', path: '/new' });
  vi.mocked(tmApi).mockRejectedValueOnce(new Error('Registration unavailable'));
  component = mount(FileNavigator, { target: document.body }); await tick(); await openFolderPicker();
  await Promise.resolve(); await tick();
  expect(attachRootToChat).not.toHaveBeenCalled();
  expect(document.body.textContent).toContain('Registration unavailable');
});
it('does not attach the chosen project to a different chat after an in-flight registration', async () => {
  (chatWorkspaceKey as Writable<string>).set('origin');
  vi.mocked(chooseWorkspaceRoot).mockResolvedValueOnce({ id: 'new', name: 'New', path: '/new' });
  let completeRegistration!: () => void;
  vi.mocked(tmApi).mockImplementationOnce(() => new Promise(resolve => { completeRegistration = () => resolve({ path: '/new' }); }));
  component = mount(FileNavigator, { target: document.body }); await tick(); await openFolderPicker();
  (chatWorkspaceKey as Writable<string>).set('different'); await tick();
  completeRegistration(); await Promise.resolve(); await tick();
  expect(attachRootToChat).not.toHaveBeenCalled();
});
