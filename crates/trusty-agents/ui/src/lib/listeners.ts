import { tmApi } from '../stores/app';
import { withChannelWriteAuth } from './channel-auth';
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
// #7609: this route is deprecated AND gated. A listener binding's
// `instructions` reach the wake prompt as trusted text, so `PUT
// /api/agents/{name}/listeners` now requires the same channel-write credential
// the Channels tab uses; without it the Listeners tab 401s on every save.
export function saveListeners(agent: string, revision: string, listeners: ListenerBinding[]): Promise<ListenerConfiguration> {
  return withChannelWriteAuth(headers =>
    tmApi(`/api/agents/${encodeURIComponent(agent)}/listeners`, { method: 'PUT', headers, body: JSON.stringify({ revision, listeners }) }),
  );
}
