/** Assistant-owned knowledge API. Requested jobs are not evidence of extracted entities. */
import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';
import type { WorkspaceRoot } from './workspaceFiles';
import { SettingsConflict } from './assistantMemoryPolicy';
import type { KnowledgeChatProjects } from './knowledgeProjects';
export type KnowledgeJobStatus = 'queued' | 'blocked_on_dependency' | 'retryable' | 'cancelled' | 'completed';
export interface KnowledgeSource { id: string; revision: string; kind: 'project' | 'gmail' | 'gdrive' | 'slack' | 'gcal'; display_name: string; dependency_reasons: string[] }
export interface KnowledgeStage { status: KnowledgeJobStatus; reason: string }
export interface KnowledgeJob {
  id: string; source_id: string; source_revision: string;
  window: { start: string; end: string }; status: KnowledgeJobStatus;
  indexing: KnowledgeStage; extraction: KnowledgeStage; cleanup: KnowledgeStage; publication: KnowledgeStage;
  dependency_reasons: string[];
}
export interface KnowledgePipeline {
  schema_version: number; assistant_id: string; revision: string; anchor_at: string;
  history_months: number; paused: boolean;
  store: { root: string; index_id: string; protected: boolean };
  assistant_projects?: string[];
  projects_by_chat: Record<string, string[]>;
  sources: KnowledgeSource[]; jobs: KnowledgeJob[];
}
export interface ExtractionCheckpoint { status: 'running' | 'publication_pending' | 'retryable' | 'completed' | 'cancelled'; attempts: number; next_attempt_at?: string; model?: string; last_error?: string | null }
export interface AssistantKnowledge {
  extraction?: Record<string, ExtractionCheckpoint>;
  assistant_projects_status?: {path: string; available: boolean; name: string; reason?: string}[];
  assistant: string; pipeline: KnowledgePipeline | null; sources: KnowledgeSource[];
  index: { connected: boolean; reason?: string }; store_issue: string | null;
}
async function request(assistant: string, method = 'GET', suffix = '', body?: unknown): Promise<AssistantKnowledge> {
  const token = getCurrentApiToken();
  const response = await fetch(`${apiBase()}/api/agents/${encodeURIComponent(assistant)}/knowledge/pipeline${suffix}`, {
    method, headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}), ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}) },
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  });
  if (!response.ok) {
    const error = await response.json().catch(() => ({}));
    const message = typeof error.error === 'string' ? error.error : `Knowledge request failed (${response.status})`;
    throw response.status === 409 ? new SettingsConflict(message) : new Error(message);
  }
  const result: AssistantKnowledge = await response.json();
  if (result.assistant !== assistant || (result.pipeline && result.pipeline.assistant_id !== assistant)) throw new Error('Knowledge response belongs to another assistant.');
  return result;
}
export const fetchAssistantKnowledge = (assistant: string) => request(assistant);
export const reconcileAssistantKnowledge = (assistant: string, revision: string | null) => request(assistant, 'POST', '', { revision });
export const setKnowledgePaused = (assistant: string, revision: string, paused: boolean) => request(assistant, 'PATCH', '', { revision, paused });
export const extendKnowledgeHistory = (assistant: string, revision: string, months = 1) => request(assistant, 'POST', '/backfill', { revision, months });
export const updateKnowledgeProjects = (assistant: string, revision: string, chat: KnowledgeChatProjects) => request(assistant, 'PUT', '/projects', { revision, ...chat });

export const updateAssistantProjects = (assistant: string, revision: string, projects: string[]) => request(assistant, 'PUT', '/projects', { revision, scope: 'assistant', projects });

/** Re-read before each serialized snapshot; optimistic revisions protect external concurrent edits. */
export async function syncKnowledgeProjects(assistant: string, chats: KnowledgeChatProjects[]): Promise<void> {
  if (!chats.length) return;
  let current = await fetchAssistantKnowledge(assistant);
  for (const chat of chats) {
    const existing = current.pipeline?.projects_by_chat[chat.chat_id];
    if (existing && JSON.stringify([...existing].sort()) === JSON.stringify([...chat.projects].sort())) continue;
    current = await updateKnowledgeProjects(assistant, current.pipeline?.revision ?? '', chat);
  }
}

/** Project status comes from the server; unavailable defaults remain visible. */
export function assistantDefaultRoots(value: AssistantKnowledge): WorkspaceRoot[] {
  return (value.pipeline?.assistant_projects ?? []).map(path => {
    const status = value.assistant_projects_status?.find(item => item.path === path);
    return { id: `assistant:${value.assistant}:${path}`, path, name: status?.name || path.split('/').pop() || path, available: status?.available ?? false };
  });
}
