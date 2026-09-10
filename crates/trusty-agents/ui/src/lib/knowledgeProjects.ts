/** Project knowledge scope follows explicit chat attachments, never the project catalog. */
import type { WorkspaceRoot } from './workspaceFiles';

export interface KnowledgeChatProjects { chat_id: string; projects: string[] }
interface AttachmentState {
  projects: WorkspaceRoot[];
  chats: Record<string, { ids: string[]; primary: string | null }>;
}

/** Preserve empty known chats so detaching the last folder revokes its source scope. */
export function assistantProjectChats(value: AttachmentState, assistant: string | null): KnowledgeChatProjects[] {
  if (!assistant) return [];
  return Object.entries(value.chats).flatMap(([key, chat]) => {
    let identity: unknown;
    try { identity = JSON.parse(key); } catch { return []; }
    if (!Array.isArray(identity) || identity.length !== 2 || identity[1] !== assistant) return [];
    return [{ chat_id: key, projects: [...new Set(value.projects
      .filter(root => root.available !== false && chat.ids.includes(root.id))
      .map(root => root.path))].sort() }];
  }).sort((a, b) => a.chat_id.localeCompare(b.chat_id));
}
