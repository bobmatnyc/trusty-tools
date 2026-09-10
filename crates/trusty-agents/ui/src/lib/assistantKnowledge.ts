/** Assistant-owned knowledge API. Requested jobs are not evidence of extracted entities. */
import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';
import type { KnowledgeChatProjects } from './knowledgeProjects';
export type KnowledgeJobStatus = 'queued' | 'blocked_on_dependency' | 'retryable' | 'cancelled';
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
  projects_by_chat: Record<string, string[]>;
  sources: KnowledgeSource[]; jobs: KnowledgeJob[];
}
export interface AssistantKnowledge {
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
    throw new Error(typeof error.error === 'string' ? error.error : `Knowledge request failed (${response.status})`);
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

/** Re-read before each serialized snapshot; optimistic revisions protect external concurrent edits. */
export async function syncKnowledgeProjects(assistant: string, chats: KnowledgeChatProjects[]): Promise<void> {
  if (!chats.length) return;
  let current = await fetchAssistantKnowledge(assistant);
  if (!current.pipeline) current = await reconcileAssistantKnowledge(assistant, null);
  for (const chat of chats) {
    if (!current.pipeline) throw new Error('Assistant knowledge has not been initialized.');
    const existing = current.pipeline.projects_by_chat[chat.chat_id];
    if (existing && JSON.stringify([...existing].sort()) === JSON.stringify([...chat.projects].sort())) continue;
    current = await updateKnowledgeProjects(assistant, current.pipeline.revision, chat);
  }
}
