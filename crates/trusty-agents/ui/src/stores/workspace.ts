import { derived, get, writable } from 'svelte/store';
import { assistantProjectChats } from '../lib/knowledgeProjects';
import { activeAgentId, activeProjectId } from './app';
import { folderError, listWorkspaceRoots, type WorkspaceRoot } from '../lib/workspaceFiles';

export const sidebarMode = writable<'history' | 'projects'>('history');
export const openedFile = writable<{ root: WorkspaceRoot; path: string } | null>(null);
export const activeRoot = writable<WorkspaceRoot | null>(null);
export const workspaceRoots = writable<WorkspaceRoot[]>([]);
export const registeredWorkspaceRoots = writable<WorkspaceRoot[]>([]);
export const workspaceError = writable<string | null>(null);
const STORAGE_KEY = 'trusty-agents.workspace.v1';
interface ChatFolders { ids: string[]; primary: string | null }
interface SavedWorkspace { locations?: Record<string, string>; projects: WorkspaceRoot[]; chats: Record<string, ChatFolders> }
function readSaved(): SavedWorkspace {
  try {
    const raw = localStorage.getItem(STORAGE_KEY);
    if (!raw) return { projects: [], chats: {} };
    const value = JSON.parse(raw);
    if (!Array.isArray(value.projects) || !value.chats || typeof value.chats !== 'object') throw new Error('Invalid saved folders');
    if (value.locations && (typeof value.locations !== 'object' || Object.values(value.locations).some(path => typeof path !== 'string'))) throw new Error('Invalid saved locations');
    for (const root of value.projects) {
      if (!root || typeof root.id !== 'string' || typeof root.path !== 'string' || typeof root.name !== 'string') throw new Error('Invalid saved folder');
    }
    for (const chat of Object.values(value.chats) as ChatFolders[]) {
      if (!chat || !Array.isArray(chat.ids) || !chat.ids.every(id => typeof id === 'string') || (chat.primary !== null && typeof chat.primary !== 'string')) throw new Error('Invalid saved chat folders');
    }
    return value;
  } catch {
    workspaceError.set('Saved project folders could not be loaded. Attachments made here may not survive a restart.');
    return { projects: [], chats: {} };
  }
}
const saved = writable<SavedWorkspace>(readSaved());
export const assistantKnowledgeAttachments = derived([saved, activeAgentId], ([$saved, assistant]) => assistantProjectChats($saved, assistant));
export const chatWorkspaceKey = derived([activeProjectId, activeAgentId], ([$project, $agent]) => JSON.stringify([$project, $agent]));
export const projectRoots = derived(saved, value => value.projects.filter(root => root.available !== false));
export const currentChatRoots = derived([saved, chatWorkspaceKey], ([$saved, key]) => $saved.projects.filter(root => $saved.chats[key]?.ids.includes(root.id)));
export const chatProjectPath = derived([saved, chatWorkspaceKey], ([$saved, key]) => {
  const primary = $saved.chats[key]?.primary;
  const root = $saved.projects.find(root => root.id === primary);
  return root?.available === false ? null : root?.path ?? null;
});
export const chatFolderError = derived([saved, chatWorkspaceKey], ([$saved, key]) => {
  const primary = $saved.chats[key]?.primary;
  const root = $saved.projects.find(root => root.id === primary);
  return root?.available === false ? `The working folder “${root.name}” is unavailable. Remove its attachment before sending, or restore the folder.` : null;
});
function save(value: SavedWorkspace): void {
  // Keep the usable in-session state even if disk persistence is unavailable, and surface that distinction.
  saved.set(value);
  try { localStorage.setItem(STORAGE_KEY, JSON.stringify(value)); workspaceError.set(null); }
  catch { workspaceError.set('Project folders are available for this session, but could not be saved for restart.'); }
}
export function rememberWorkspaceRoot(root: WorkspaceRoot): void {
  // A root registered after a list began must survive that older snapshot.
  rootsRequest++;
  workspaceRoots.update(roots => [...roots.filter(item => item.id !== root.id && item.path !== root.path), root]);
  activeRoot.set(root);
}
export function attachRootToChat(root: WorkspaceRoot): void {
  const value = get(saved), key = get(chatWorkspaceKey), chat = value.chats[key] ?? { ids: [], primary: null };
  save({ ...value, projects: [...value.projects.filter(item => item.id !== root.id && item.path !== root.path), root], chats: { ...value.chats, [key]: { ids: [...new Set([...chat.ids, root.id])], primary: root.id } } });
  rememberWorkspaceRoot(root);
}
export function detachRootFromChat(rootId: string): void {
  const value = get(saved), key = get(chatWorkspaceKey), chat = value.chats[key];
  if (!chat) return;
  const ids = chat.ids.filter(id => id !== rootId);
  save({ ...value, chats: { ...value.chats, [key]: { ids, primary: chat.primary === rootId ? ids[0] ?? null : chat.primary } } });
}
export function selectChatRoot(root: WorkspaceRoot): void {
  const value = get(saved), key = get(chatWorkspaceKey), chat = value.chats[key];
  if (!chat?.ids.includes(root.id)) return;
  save({ ...value, chats: { ...value.chats, [key]: { ...chat, primary: root.id } } });
  activeRoot.set(root);
}
let rootsRequest = 0;
let registeredPathsSnapshot: string[] = [];
export async function loadWorkspaceRoots(paths?: string[]): Promise<void> {
  // Generic app refreshes preserve the latest registry inputs; an explicit list replaces them.
  if (paths !== undefined) registeredPathsSnapshot = paths;
  const requestedPaths = registeredPathsSnapshot;
  const request = ++rootsRequest;
  try {
    const before = get(saved);
    const listed = await listWorkspaceRoots([...new Set([...requestedPaths.map(path => before.locations?.[path] ?? path), ...before.projects.map(root => root.path)])]);
    if (request !== rootsRequest) return;
    const roots = listed.filter(root => !before.locations?.[root.path] || before.locations[root.path] === root.path);
    const value = get(saved);
    const replacements = new Map(value.projects.map(root => [root.id, roots.find(item => item.path === root.path || item.aliases?.includes(root.path)) ?? { ...root, available: false }]));
    const projects = [...new Map([...replacements.values()].map(root => [root.id, root])).values()];
    const chats = Object.fromEntries(Object.entries(value.chats).map(([key, chat]) => [key, {
      ids: [...new Set(chat.ids.map(id => replacements.get(id)?.id ?? id))],
      primary: chat.primary ? replacements.get(chat.primary)?.id ?? chat.primary : null,
    }]));
    save({ ...value, projects, chats });
    const availableRoots = roots.filter(root => root.available !== false);
    workspaceRoots.set(availableRoots);
    const registeredPaths = new Set(requestedPaths.map(path => before.locations?.[path] ?? path));
    registeredWorkspaceRoots.set([...new Map(availableRoots.filter(root => registeredPaths.has(root.path) || root.aliases?.some(path => registeredPaths.has(path))).map(root => [root.path, root])).values()]);
    const current = get(activeRoot);
    // Native validation may exclude missing folders. Keep attachment history so users can locate them again.
    activeRoot.set(availableRoots.find(root => root.id === current?.id) ?? availableRoots.find(root => root.path === get(chatProjectPath)) ?? availableRoots[0] ?? null);
  } catch (error) { if (request === rootsRequest) workspaceError.set(folderError(error)); }
}
chatWorkspaceKey.subscribe(key => {
  const value = get(saved), primary = value.chats[key]?.primary;
  activeRoot.set(value.projects.find(root => root.id === primary && root.available !== false) ?? null);
  openedFile.set(null);
});

/** Reconnect an existing project without losing any chat attachment. */
export function relocateWorkspaceRoot(previous: WorkspaceRoot, root: WorkspaceRoot): void {
  const value = get(saved);
  const chats = Object.fromEntries(Object.entries(value.chats).map(([key, chat]) => [key, {
    ids: [...new Set(chat.ids.map(id => id === previous.id ? root.id : id))],
    primary: chat.primary === previous.id ? root.id : chat.primary,
  }]));
  const projects = value.projects.some(item => item.id === previous.id)
    ? [...value.projects.filter(item => item.id !== previous.id && item.id !== root.id), root]
    : value.projects;
  save({ ...value, projects, chats, locations: { ...value.locations, [previous.path]: root.path } });
  workspaceRoots.update(items => items.filter(item => item.id !== previous.id));
  rememberWorkspaceRoot(root);
}

export function detachUnavailableChatFolders(): void {
  const value = get(saved);
  const primary = value.chats[get(chatWorkspaceKey)]?.primary;
  const root = value.projects.find(item => item.id === primary);
  if (root?.available === false) detachRootFromChat(root.id);
}
