/**
 * Per-assistant memory palace and its opt-in fan-out (#7428).
 *
 * The stored `palace` is routinely null: an assistant's palace is DERIVED
 * (its `[[stores]]` binding, else this setting, else its instance id), so
 * `resolved` is the only field worth rendering as "your palace".
 */
import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';

export type PalaceSource = 'binding' | 'config' | 'instance-id' | 'unresolved';

export interface AssistantMemory {
  assistant: string;
  /** The stored override; null whenever the palace is derived. */
  palace: string | null;
  /** Other assistant INSTANCE ids this one also recalls from. */
  fan_out: string[];
  resolved: { own: string | null; source: PalaceSource; fan_out_palaces: string[] };
  /** Other assistant instances that may be selected. Never includes self. */
  available: string[];
}

async function request(assistant: string, body?: unknown): Promise<AssistantMemory> {
  const token = getCurrentApiToken();
  const response = await fetch(`${apiBase()}/api/assistants/${encodeURIComponent(assistant)}/memory`, {
    method: body === undefined ? 'GET' : 'PUT',
    headers: {
      ...(token ? { Authorization: `Bearer ${token}` } : {}),
      ...(body !== undefined ? { 'Content-Type': 'application/json' } : {}),
    },
    ...(body !== undefined ? { body: JSON.stringify(body) } : {}),
  });
  if (!response.ok) {
    const error = await response.json().catch(() => ({}));
    throw new Error(typeof error.error === 'string' ? error.error : `Memory request failed (${response.status})`);
  }
  const result: AssistantMemory = await response.json();
  // Same guard as the knowledge pipeline: a response for a different assistant
  // would render one assistant's memory settings under another's name.
  if (result.assistant !== assistant) throw new Error('Memory response belongs to another assistant.');
  return result;
}

export const fetchAssistantMemory = (assistant: string) => request(assistant);
export const saveAssistantFanOut = (assistant: string, current: AssistantMemory, fanOut: string[]) =>
  request(assistant, { palace: current.palace, fan_out: fanOut });
