import { tmApi } from '../stores/app';

export interface ProjectToolsStatus {
  path: string;
  index: { connected: boolean; id: string | null; status: string | { status?: string; chunk_count?: number; last_indexed?: string | null; last_walk_error?: string | null; walk_truncated_by_budget?: boolean; stuck_mid_walk?: boolean } | null; reason?: string };
  knowledge: { available: boolean; agent: string; tree: string | null; index: string | null; reason?: string };
}
export interface ProjectImportResult {
  path: string; agent: string; tree: string;
  result: {
    ingest: { scanned: number; ingested: number; updated: number; skipped: number; errors: string[] };
    index: { indexed: number; pending: number; reason?: string; errors?: string[] };
  };
}
export function fetchProjectTools(path: string, agent: string | null): Promise<ProjectToolsStatus> {
  const params = new URLSearchParams({ path });
  if (agent) params.set('agent', agent);
  return tmApi(`/api/project-tools?${params}`);
}
export function indexProject(path: string): Promise<{ status: string }> {
  return tmApi('/api/project-tools/index', { method: 'POST', body: JSON.stringify({ path }) });
}
export function importProject(path: string, agent: string): Promise<ProjectImportResult> {
  return tmApi('/api/project-tools/import', { method: 'POST', body: JSON.stringify({ path, agent }) });
}

export interface ProjectSkillSource {
  id: string;
  label: string;
  path: string;
  enabled: boolean;
  skills: { name: string; description: string; path: string }[];
  error?: string;
}
export interface ProjectSkillsStatus {
  path: string;
  revision: string;
  sources: ProjectSkillSource[];
}
export function fetchProjectSkills(path: string): Promise<ProjectSkillsStatus> {
  return tmApi(`/api/project-tools/skills?${new URLSearchParams({ path })}`);
}
export function updateProjectSkillSources(path: string, revision: string, sources: { id: string; enabled: boolean }[]): Promise<ProjectSkillsStatus> {
  return tmApi('/api/project-tools/skills', { method: 'PATCH', body: JSON.stringify({ path, revision, sources }) });
}
