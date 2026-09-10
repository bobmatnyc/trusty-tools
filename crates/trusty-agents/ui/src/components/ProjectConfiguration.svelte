<script lang="ts">
  import { onDestroy } from 'svelte';
  import { ArrowLeft, RefreshCw, X } from 'lucide-svelte';
  import { activeAgentId } from '../stores/app';
  import type { WorkspaceRoot } from '../lib/workspaceFiles';
  import UserSkillSettings from './UserSkillSettings.svelte';
  import { fetchProjectTools, indexProject, importProject, fetchProjectSkills, updateProjectSkillSources, type ProjectSkillsStatus, type ProjectToolsStatus, type ProjectImportResult } from '../lib/projectTools';
  export let root: WorkspaceRoot;
  export let onClose: () => void;
  let status: ProjectToolsStatus | null = null;
  let result: ProjectImportResult | null = null;
  let loading = false, busy = '', error = '', notice = '';
  let generation = 0, previous = '';
  let skills: ProjectSkillsStatus | null = null;
  let skillDraft: Record<string, boolean> = {};
  let skillsLoading = false, skillsSaving = false, skillsError = '', skillsNotice = '';
  let skillsRequest = 0, skillsGeneration = 0, skillsPath = '';
  $: if (root.path !== skillsPath) { skillsPath = root.path; skillsGeneration++; void refreshSkills(); }
  $: scope = JSON.stringify([root.path, $activeAgentId]);
  $: if (scope !== previous) { previous = scope; result = null; notice = ''; busy = ''; void refresh(); }
  $: indexState = typeof status?.index.status === 'string' ? status.index.status
    : status?.index.status && typeof status.index.status === 'object' && 'status' in status.index.status
      ? String(status.index.status.status) : null;
  $: indexDetails = status?.index.status && typeof status.index.status === 'object' ? status.index.status : null;
  async function refresh() {
    const token = ++generation, path = root.path, agent = $activeAgentId;
    loading = true; error = ''; status = null;
    try { const next = await fetchProjectTools(path, agent); if (token === generation) status = next; }
    catch (cause) { if (token === generation) error = cause instanceof Error ? cause.message : String(cause); }
    finally { if (token === generation) loading = false; }
  }
  async function refreshSkills(token = skillsGeneration, path = root.path) {
    const request = ++skillsRequest;
    skillsLoading = true; skillsSaving = false; skillsError = ''; skillsNotice = ''; skills = null; skillDraft = {};
    try {
      const next = await fetchProjectSkills(path);
      if (token !== skillsGeneration || request !== skillsRequest) return;
      skills = next; skillDraft = Object.fromEntries(next.sources.map(source => [source.id, source.enabled]));
    } catch (cause) {
      if (token === skillsGeneration && request === skillsRequest) skillsError = 'Project skills could not be loaded. ' + String(cause);
    } finally { if (token === skillsGeneration && request === skillsRequest) skillsLoading = false; }
  }
  async function toggleSkillSource(id: string, enabled: boolean) {
    if (!skills || skillsSaving) return;
    skillDraft = { ...skillDraft, [id]: enabled };
    const token = skillsGeneration, request = ++skillsRequest, path = root.path, revision = skills.revision;
    const sources = skills.sources.map(source => ({ id: source.id, enabled: skillDraft[source.id] }));
    skillsSaving = true; skillsError = ''; skillsNotice = '';
    try {
      const next = await updateProjectSkillSources(path, revision, sources);
      if (token !== skillsGeneration || request !== skillsRequest) return;
      skills = next; skillDraft = Object.fromEntries(next.sources.map(source => [source.id, source.enabled]));
      skillsNotice = 'Saved. Changes apply to new turns.';
    } catch (cause) {
      if (token === skillsGeneration && request === skillsRequest) skillsError = /409|conflict/i.test(String(cause))
        ? 'Not saved: these settings changed elsewhere. Your selection is still shown. Reload skill settings before trying again.'
        : 'Not saved. Your selection is still shown. ' + String(cause);
    } finally { if (token === skillsGeneration && request === skillsRequest) skillsSaving = false; }
  }
  async function run(action: 'index' | 'import') {
    if (busy || !status) return;
    const token = generation, path = root.path, agent = $activeAgentId;
    busy = action; error = ''; notice = ''; result = null;
    try {
      if (action === 'index') {
        const response = await indexProject(path);
        if (token !== generation) return;
        notice = response.status === 'already_running' ? 'Indexing is already running.' : 'Indexing requested. Refresh to check progress.';
      } else if (agent) {
        const response = await importProject(path, agent);
        if (token !== generation) return;
        result = response;
      }
    } catch (cause) { if (token === generation) error = cause instanceof Error ? cause.message : String(cause); }
    finally { if (token === generation) busy = ''; }
  }
  onDestroy(() => { generation++; skillsGeneration++; });
</script>
<section class="configuration" aria-label="Project configuration">
  <header><button type="button" aria-label="Back to projects" on:click={onClose}><ArrowLeft size={16} /></button><strong>Configure project</strong><button type="button" style="margin-left:auto" aria-label="Close project configuration" on:click={onClose}><X size={16} /></button></header>
  <div class="body">
    <h2>{root.name}</h2><p class="path">{root.path}</p>
    {#if loading}<p role="status">Loading project settings…</p>{/if}
    {#if error}<p class="error" role="alert">{error}</p>{/if}
    {#if status}
      <section><h3>Search index</h3>
        {#if status.index.id}<p>{status.index.id}</p>{:else}<p>No index yet.</p>{/if}
        {#if indexState}<p class="muted">{indexState}</p>{/if}
        {#if typeof indexDetails?.chunk_count === 'number'}<p class="muted">{indexDetails.chunk_count} searchable passages</p>{/if}
        {#if indexDetails?.last_indexed}<p class="muted">Last indexed: {indexDetails.last_indexed}</p>{/if}
        {#if indexDetails?.last_walk_error}<p class="error">{indexDetails.last_walk_error}</p>{/if}
        {#if indexDetails?.stuck_mid_walk}<p class="error">Indexing was interrupted. Reindex to retry.</p>{/if}
        {#if indexDetails?.walk_truncated_by_budget}<p class="muted">The last scan reached its limit; some files may not be indexed.</p>{/if}
        {#if status.index.reason}<p class="muted">{status.index.reason}</p>{/if}
        <button class="action" type="button" disabled={!!busy || !status.index.connected} on:click={() => run('index')}>{busy === 'index' ? 'Starting…' : status.index.id ? 'Reindex project' : 'Create index'}</button>
      </section>
      <section><h3>Knowledge Graph</h3>
        {#if status.knowledge.available}<p>Import into <strong>{status.knowledge.agent}</strong></p><p class="path">{status.knowledge.tree}</p><p class="muted">Import supported text documents and code. Repeated imports update changed content.</p>
        {:else}<p class="muted">{status.knowledge.reason || 'Select an assistant with a connected knowledge store.'}</p>{/if}
        <button class="action" type="button" disabled={!!busy || !status.knowledge.available || !$activeAgentId} on:click={() => run('import')}>{busy === 'import' ? 'Importing…' : 'Import project contents'}</button>
      </section>
    {/if}
    <section aria-label="Project skills">
      <h3>Project skills</h3>
      <p class="muted">Skills installed in this project's supported folders are discovered automatically. Enabled sources apply to new turns; discovery does not grant extra tools.</p>
      {#if skillsLoading}<p role="status">Loading project skills…</p>{/if}
      {#if skillsError}<p class="error" role="alert">{skillsError}</p>{/if}
      {#if skills}
        {#if skills.sources.length === 0}<p class="muted">No supported skill folders found in this project.</p>{/if}
        {#each skills.sources as source (source.id)}
          <div class="skill-source">
            <label class="source-toggle"><input type="checkbox" checked={skillDraft[source.id]} disabled={skillsSaving || skillsLoading} on:change={event => toggleSkillSource(source.id, event.currentTarget.checked)} /><strong>{source.label}</strong></label>
            <p class="path">{source.path}</p>
            {#if source.error}<p class="error">{source.error}</p>{/if}
            <details>
              <summary>{source.skills.length} {source.skills.length === 1 ? 'skill' : 'skills'} discovered</summary>
              {#each source.skills as skill (skill.path)}
                <div class="skill-entry"><strong>{skill.name}</strong>{#if skill.description}<p class="muted">{skill.description}</p>{/if}<p class="path">{skill.path}</p></div>
              {/each}
            </details>
          </div>
        {/each}
      {/if}
      {#if skillsSaving}<p role="status">Saving skill sources…</p>{/if}
      {#if skillsNotice}<p role="status">{skillsNotice}</p>{/if}
      <button class="action" type="button" disabled={skillsLoading || skillsSaving} on:click={() => refreshSkills()}>Reload skill settings</button>
    </section>
    <UserSkillSettings />
    {#if notice}<p role="status">{notice}</p>{/if}
    {#if result}<div role="status"><p>Imported {result.result.ingest.ingested} new items, updated {result.result.ingest.updated}, unchanged {result.result.ingest.skipped}.</p>
      <p>{result.result.index.indexed} indexed; {result.result.index.pending} pending.</p>
      {#if result.result.index.reason}<p class="muted">{result.result.index.reason}</p>{/if}
      {#each [...result.result.ingest.errors, ...(result.result.index.errors ?? [])] as issue}<p class="error">{issue}</p>{/each}
    </div>{/if}
    <button class="refresh" type="button" disabled={loading || !!busy} on:click={refresh}><RefreshCw size={13} />Refresh status</button>
  </div>
</section>
<style>
  .configuration { height:100%; min-height:0; display:flex; flex-direction:column; font-size:12px; color:rgb(var(--color-text-primary)); }
  header { display:flex; align-items:center; gap:9px; padding:12px; border-bottom:1px solid rgb(var(--color-border)); }
  .body { padding:14px; overflow:auto; min-height:0; }
  h2 { font-weight:600; font-size:14px; } h3 { font-weight:600; margin-bottom:8px; }
  .body section { padding:16px 0; border-bottom:1px solid rgb(var(--color-border) / .6); }
  p { margin:7px 0; overflow-wrap:anywhere; } .path { font-size:10px; opacity:.65; } .muted { opacity:.7; }
  button { cursor:pointer; } button:disabled { opacity:.45; cursor:default; }
  button:focus-visible { outline:2px solid rgb(var(--color-primary)); outline-offset:2px; }
  .action { border:1px solid rgb(var(--color-border)); border-radius:5px; padding:7px 10px; margin-top:6px; width:100%; }
  .refresh { display:flex; align-items:center; gap:6px; margin-top:15px; }
  .source-toggle { display:flex; align-items:center; gap:8px; }
  .source-toggle input { accent-color:rgb(var(--color-primary)); }
  .skill-source { padding:10px 0; }
  .skill-source summary { cursor:pointer; font-size:11px; }
  .skill-entry { padding:8px 0 8px 10px; border-left:1px solid rgb(var(--color-border)); margin-top:8px; }
  .skill-entry p { margin:4px 0; }
  .error { color:rgb(var(--color-warning)); }
</style>
