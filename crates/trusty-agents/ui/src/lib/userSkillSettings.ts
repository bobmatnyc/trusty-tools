import { get, writable } from 'svelte/store';
import { tmApi } from '../stores/app';
import type { ProjectSkillsStatus } from './projectTools';

interface State {
  data: ProjectSkillsStatus | null;
  loading: boolean; saving: boolean; error: string; notice: string;
  draft: Record<string, boolean>;
}
// User settings are global. Keep transactions and failed drafts alive across pane changes.
export const userSkillSettings = writable<State>({ data: null, loading: false, saving: false, error: '', notice: '', draft: {} });
const selection = (data: ProjectSkillsStatus) => Object.fromEntries(data.sources.map(s => [s.id, s.enabled]));
export async function loadUserSkills(force = false) {
  const current = get(userSkillSettings);
  if (current.loading || current.saving || (!force && (current.data || current.error))) return;
  userSkillSettings.update(s => ({ ...s, loading: true, error: '', notice: '' }));
  try {
    const data = await tmApi<ProjectSkillsStatus>('/api/user-skills');
    userSkillSettings.update(s => ({ ...s, data, draft: selection(data) }));
  } catch (cause) {
    userSkillSettings.update(s => ({ ...s, error: 'User skills could not be loaded. ' + String(cause) }));
  } finally { userSkillSettings.update(s => ({ ...s, loading: false })); }
}
export async function toggleUserSkillSource(id: string, enabled: boolean) {
  const current = get(userSkillSettings);
  if (!current.data || current.saving || current.loading) return;
  const draft = { ...current.draft, [id]: enabled };
  userSkillSettings.update(s => ({ ...s, draft, saving: true, error: '', notice: '' }));
  try {
    const data = await tmApi<ProjectSkillsStatus>('/api/user-skills', { method: 'PATCH',
      body: JSON.stringify({ revision: current.data.revision, sources: current.data.sources.map(s => ({ id: s.id, enabled: draft[s.id] })) }) });
    userSkillSettings.update(s => ({ ...s, data, draft: selection(data), notice: 'Saved for all projects. Changes apply to new turns.' }));
  } catch (cause) {
    userSkillSettings.update(s => ({ ...s, error: /409|conflict/i.test(String(cause))
      ? 'Not saved: settings changed elsewhere. Your selection is still shown. Reload before trying again.'
      : 'Not saved. Your selection is still shown. ' + String(cause) }));
  } finally { userSkillSettings.update(s => ({ ...s, saving: false })); }
}
