import { beforeEach, describe, expect, it, vi } from 'vitest';
import { get, writable } from 'svelte/store';
vi.mock('./app', () => ({ activeProjectId: writable('ctrl'), activeAgentId: writable<string | null>(null) }));
vi.mock('../lib/workspaceFiles', () => ({ listWorkspaceRoots: vi.fn(), folderError: (error: unknown) => String(error) }));
const alpha = { id: 'a', name: 'Alpha', path: '/alpha' };
const beta = { id: 'b', name: 'Beta', path: '/beta' };
beforeEach(async () => {
  vi.resetModules(); vi.restoreAllMocks();
  const data = new Map<string, string>();
  vi.stubGlobal('localStorage', { getItem: (key: string) => data.get(key) ?? null, setItem: (key: string, value: string) => data.set(key, value) });
  const app = await import('./app'); app.activeProjectId.set('ctrl'); app.activeAgentId.set(null);
});
describe('workspace chat folders', () => {
  it('isolates attachments by assistant and project, restores working folders, and retains project browser history', async () => {
    const state = await import('./workspace');
    const app = await import('./app');
    state.attachRootToChat(alpha);
    app.activeAgentId.set('izzie');
    expect(get(state.chatProjectPath)).toBeNull();
    state.attachRootToChat(beta);
    app.activeProjectId.set('other');
    expect(get(state.currentChatRoots)).toEqual([]);
    app.activeProjectId.set('ctrl');
    expect(get(state.chatProjectPath)).toBe('/beta');
    app.activeAgentId.set(null);
    expect(get(state.chatProjectPath)).toBe('/alpha');
    expect(get(state.activeRoot)).toEqual(alpha);
    state.detachRootFromChat('a');
    expect(get(state.chatProjectPath)).toBeNull();
    expect(get(state.projectRoots)).toEqual([alpha, beta]);
  });
  it('restores persisted attachment and primary selection after module restart', async () => {
    let state = await import('./workspace');
    state.attachRootToChat(alpha);
    state.attachRootToChat(beta);
    state.selectChatRoot(alpha);
    vi.resetModules();
    state = await import('./workspace');
    expect(get(state.currentChatRoots)).toEqual([alpha, beta]);
    expect(get(state.chatProjectPath)).toBe('/alpha');
    expect(get(state.activeRoot)).toEqual(alpha);
  });
  it('browsing does not attach, and failed persistence is visible without losing in-session attachment', async () => {
    const state = await import('./workspace');
    state.rememberWorkspaceRoot(alpha);
    expect(get(state.currentChatRoots)).toEqual([]);
    vi.spyOn(localStorage, 'setItem').mockImplementation(() => { throw new Error('quota'); });
    state.attachRootToChat(alpha);
    expect(get(state.chatProjectPath)).toBe('/alpha');
    expect(get(state.workspaceError)).toContain('could not be saved');
  });
  it('rejects malformed persisted state visibly', async () => {
    localStorage.setItem('trusty-agents.workspace.v1', '{"projects":[],"chats":{"x":{"ids":1}}}');
    const state = await import('./workspace');
    expect(get(state.currentChatRoots)).toEqual([]);
    expect(get(state.workspaceError)).toContain('could not be loaded');
  });
  it('preserves a newly registered root when an earlier initial listing finishes', async () => {
    const client = await import('../lib/workspaceFiles');
    let resolveInitial!: (value: typeof alpha[]) => void;
    vi.mocked(client.listWorkspaceRoots).mockReturnValueOnce(new Promise(resolve => { resolveInitial = resolve; }));
    const state = await import('./workspace');
    const initial = state.loadWorkspaceRoots();
    state.rememberWorkspaceRoot(beta);
    resolveInitial([alpha]);
    await initial;
    expect(get(state.workspaceRoots)).toEqual([beta]);
    expect(get(state.activeRoot)).toEqual(beta);
    expect(get(state.currentChatRoots)).toEqual([]);
  });
  it('ignores an older root list response after a newer refresh', async () => {
    const client = await import('../lib/workspaceFiles');
    let resolveFirst!: (value: typeof alpha[]) => void;
    vi.mocked(client.listWorkspaceRoots).mockReturnValueOnce(new Promise(resolve => { resolveFirst = resolve; })).mockResolvedValueOnce([beta]);
    const state = await import('./workspace');
    const first = state.loadWorkspaceRoots();
    await state.loadWorkspaceRoots();
    resolveFirst([alpha]);
    await first;
    expect(get(state.workspaceRoots)).toEqual([beta]);
    expect(get(state.activeRoot)).toEqual(beta);
  });
});


it('reconciles duplicate directory aliases and preserves attachments across all chats', async () => {
  const state = await import('./workspace');
  const app = await import('./app');
  state.attachRootToChat(alpha);
  app.activeAgentId.set('izzie');
  state.attachRootToChat({ ...beta, path: '/alias-alpha' });
  const client = await import('../lib/workspaceFiles');
  vi.mocked(client.listWorkspaceRoots).mockResolvedValue([{ ...alpha, aliases: ['/alpha', '/alias-alpha'], available: true }]);
  await state.loadWorkspaceRoots(['/alias-alpha']);
  expect(get(state.projectRoots)).toHaveLength(1);
  expect(get(state.chatProjectPath)).toBe('/alpha');
  app.activeAgentId.set(null);
  expect(get(state.chatProjectPath)).toBe('/alpha');
});

it('marks missing folders unavailable and reconnects all chat links without duplicates', async () => {
  const state = await import('./workspace');
  state.attachRootToChat(alpha);
  const client = await import('../lib/workspaceFiles');
  vi.mocked(client.listWorkspaceRoots).mockResolvedValue([{ ...alpha, available: false }]);
  await state.loadWorkspaceRoots();
  expect(get(state.chatProjectPath)).toBeNull();
  expect(get(state.workspaceRoots)).toEqual([]);
  expect(get(state.projectRoots)).toEqual([]);
  expect(get(state.activeRoot)).toBeNull();
  state.relocateWorkspaceRoot(alpha, beta);
  expect(get(state.chatProjectPath)).toBe('/beta');
  vi.mocked(client.listWorkspaceRoots).mockResolvedValue([beta, { ...alpha, available: false }]);
  await state.loadWorkspaceRoots(['/alpha']);
  expect(vi.mocked(client.listWorkspaceRoots).mock.calls.slice(-1)[0][0]).toEqual(['/beta']);
  expect(get(state.workspaceRoots)).toEqual([beta]);
});


it('removes a missing chat attachment only when requested', async () => {
  const state = await import('./workspace');
  state.attachRootToChat({ ...alpha, available: false });
  expect(get(state.chatFolderError)).toContain('unavailable');
  state.detachUnavailableChatFolders();
  expect(get(state.currentChatRoots)).toEqual([]);
  expect(get(state.chatFolderError)).toBeNull();
});


it('offers registered canonical roots only, omitting arbitrary and missing folders', async () => {
  const client = await import('../lib/workspaceFiles');
  const state = await import('./workspace');
  const canonical = { ...alpha, aliases: ['/alias-alpha', '/alpha'] };
  vi.mocked(client.listWorkspaceRoots).mockResolvedValue([canonical, beta, { id: 'gone', name: 'Gone', path: '/gone', available: false }]);
  await state.loadWorkspaceRoots(['/alias-alpha', '/alpha', '/gone']);
  expect(get(state.registeredWorkspaceRoots)).toEqual([canonical]);
  expect(get(state.workspaceRoots)).toEqual([canonical, beta]);
});


it('preserves registered candidates during a generic app refresh', async () => {
  const client = await import('../lib/workspaceFiles');
  const state = await import('./workspace');
  vi.mocked(client.listWorkspaceRoots).mockResolvedValue([alpha, beta]);
  await state.loadWorkspaceRoots(['/alpha']);
  await state.loadWorkspaceRoots();
  expect(get(state.registeredWorkspaceRoots)).toEqual([alpha]);
  expect(vi.mocked(client.listWorkspaceRoots)).toHaveBeenLastCalledWith(['/alpha']);
});
