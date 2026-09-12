// #7360: memory ownership is server-selected; only cross-palace reading is editable.
//
// Split out of `assistantMemory.ts` during the rebase onto main: #7428's
// palace/fan-out client landed there first and owns that file's name.
import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';
export interface AssistantMemoryPolicy {
  assistant_id: string; namespace: string; revision: string; cross_palace_query: boolean;
}
export class SettingsConflict extends Error {}
async function request(assistant: string, patch?: { revision: string; cross_palace_query: boolean }): Promise<AssistantMemoryPolicy> {
  const token = getCurrentApiToken();
  const response = await fetch(`${apiBase()}/api/agents/${encodeURIComponent(assistant)}/memory-policy`, {
    method: patch ? 'PATCH' : 'GET',
    headers: { ...(token ? { Authorization: `Bearer ${token}` } : {}), ...(patch ? { 'Content-Type': 'application/json' } : {}) },
    ...(patch ? { body: JSON.stringify(patch) } : {}),
  });
  if (!response.ok) {
    const body = await response.json().catch(() => ({}));
    const message = body.error || `Memory settings failed (${response.status})`;
    throw response.status === 409 ? new SettingsConflict(message) : new Error(message);
  }
  const value: AssistantMemoryPolicy = await response.json();
  if (value.assistant_id !== assistant || typeof value.namespace !== 'string' || typeof value.revision !== 'string' || typeof value.cross_palace_query !== 'boolean') throw new Error('Invalid memory settings response.');
  return value;
}
export const fetchAssistantMemoryPolicy = (assistant: string) => request(assistant);
export const patchAssistantMemory = (assistant: string, revision: string, cross_palace_query: boolean) => request(assistant, { revision, cross_palace_query });
