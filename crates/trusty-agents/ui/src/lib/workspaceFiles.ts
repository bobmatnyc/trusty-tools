import { invoke, isDesktop } from './transport';

export interface WorkspaceRoot { id: string; name: string; path: string; available?: boolean; aliases?: string[] }
export interface WorkspaceEntry { name: string; path: string; is_dir: boolean; size: number | null }
export interface WorkspaceFile { path: string; kind: 'markdown' | 'code' | 'image' | 'unsupported'; content: string; mime?: string; size: number }
export interface WorkspaceDiff { available: boolean; diff: string; reason?: string }
export const DESKTOP_FILES_MESSAGE = 'Open the desktop app to browse local files and attach project folders.';
function desktop() { if (!isDesktop()) throw new Error(DESKTOP_FILES_MESSAGE); }
export async function listWorkspaceRoots(paths: string[] = []): Promise<WorkspaceRoot[]> {
  desktop(); return invoke('workspace_list_roots', { paths });
}
export async function registerWorkspaceRoot(path: string): Promise<WorkspaceRoot> {
  desktop(); return invoke('workspace_register_root', { path });
}
export async function chooseWorkspaceRoot(): Promise<WorkspaceRoot | null> {
  desktop();
  const { open } = await import('@tauri-apps/plugin-dialog');
  const path = await open({ directory: true, multiple: false, title: 'Choose a folder' });
  return typeof path === 'string' ? registerWorkspaceRoot(path) : null;
}
export async function listWorkspaceFiles(rootId: string, path = ''): Promise<{ entries: WorkspaceEntry[] }> {
  desktop(); return invoke('workspace_list_files', { rootId, path });
}
export async function readWorkspaceFile(rootId: string, path: string): Promise<WorkspaceFile> {
  desktop(); return invoke('workspace_read_file', { rootId, path });
}
export async function diffWorkspaceFile(rootId: string, path: string): Promise<WorkspaceDiff> {
  desktop(); return invoke('workspace_diff_file', { rootId, path });
}

export function folderError(error: unknown): string {
  const message = String(error);
  if (/os error 2|not found|no such file|unknown workspace|has moved/i.test(message)) return 'This folder is missing or has moved. Choose its current location to reconnect it.';
  if (/permission denied|os error 13/i.test(message)) return 'This folder cannot be accessed. Check its permissions or choose another folder.';
  return message;
}
