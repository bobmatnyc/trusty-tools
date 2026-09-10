import { tmApi } from '../stores/app';
export interface ListenerFilter {
  from: string[]; include_labels: string[]; exclude_labels: string[];
  subject_contains: string[]; snippet_contains: string[];
}
export interface ListenerBinding {
  name: string; enabled: boolean; event_types: string[]; filter: ListenerFilter; instructions: string;
}
export interface ListenerConfiguration {
  agent: string; revision: string; listeners: ListenerBinding[]; inherited_names?: string[];
  available_listeners: { name: string; connector: string; identity: string | null; enabled: boolean }[];
}
export function fetchListeners(agent: string): Promise<ListenerConfiguration> {
  return tmApi(`/api/agents/${encodeURIComponent(agent)}/listeners`);
}
export function saveListeners(agent: string, revision: string, listeners: ListenerBinding[]): Promise<ListenerConfiguration> {
  return tmApi(`/api/agents/${encodeURIComponent(agent)}/listeners`, { method: 'PUT', body: JSON.stringify({ revision, listeners }) });
}
