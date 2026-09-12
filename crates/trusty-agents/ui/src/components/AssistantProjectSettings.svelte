<script lang="ts">
  import { fetchAssistantKnowledge, updateAssistantProjects, assistantDefaultRoots, type AssistantKnowledge } from '../lib/assistantKnowledge';
  import { SettingsConflict } from '../lib/assistantMemoryPolicy';
  import { registerProjectFolder, chooseWorkspaceRoot, type WorkspaceRoot } from '../lib/workspaceFiles';
  import { setAssistantDefaultProjects } from '../stores/workspace';
  import { tmApi } from '../stores/app';
  import { isDesktop } from '../lib/transport';
  let { agentName }: { agentName: string } = $props();
  let value = $state<AssistantKnowledge | null>(null);
  let projects = $state<WorkspaceRoot[]>([]);
  let path = $state('');
  let error = $state('');
  let saving = $state(false);
  let generation = 0;
  const desktop = isDesktop();
  const selected = $derived(value?.pipeline?.assistant_projects ?? []);
  const options = $derived([...new Map([...projects, ...selected.map(path => {
    const status = value?.assistant_projects_status?.find(item => item.path === path);
    return { id: path, path, name: status?.name || path.split('/').pop() || path, available: status?.available ?? false };
  })].map(root => [root.path, root])).values()]);
  function accept(next: AssistantKnowledge) {
    value = next;
    setAssistantDefaultProjects(next.assistant, assistantDefaultRoots(next));
  }
  $effect(() => {
    const owner = agentName, request = ++generation;
    value = null; projects = []; path = ''; error = ''; saving = false;
    void Promise.all([fetchAssistantKnowledge(owner), tmApi<WorkspaceRoot[]>('/api/projects?all=true')])
      .then(([next, inventory]) => { if (request === generation) { accept(next); projects = inventory.map(root => ({ ...root, available: root.available === true })); } })
      .catch(cause => { if (request === generation) error = String(cause); });
    return () => { generation++; };
  });
  async function save(next: string[], request = generation) {
    if (!value || request !== generation) return;
    const owner = agentName;
    try {
      const updated = await updateAssistantProjects(owner, value.pipeline?.revision ?? '', next);
      if (request === generation) {
        accept(updated);
        window.dispatchEvent(new Event('assistant-project-settings-changed'));
      }
    } catch (cause) {
      if (request !== generation) return;
      error = String(cause);
      if (cause instanceof SettingsConflict) {
        try {
          const latest = await fetchAssistantKnowledge(owner);
          if (request === generation) { accept(latest); error = 'Project settings changed elsewhere. Current selections loaded; repeat your change to save it.'; }
        } catch (reloadError) { if (request === generation) { value = null; error = String(reloadError); } }
      }
    }
  }
  async function toggle(event: Event, path: string) {
    if (saving || !value) return;
    const input = event.target as HTMLInputElement, enabled = input.checked, request = generation;
    input.checked = selected.includes(path);
    saving = true; error = '';
    try { await save(enabled ? [...selected, path] : selected.filter(item => item !== path), request); }
    finally { if (request === generation) saving = false; }
  }
  async function addFolder(native = false) {
    if (saving || !value) return;
    const request = generation;
    saving = true; error = '';
    try {
      let root: WorkspaceRoot | null;
      if (native) {
        root = await chooseWorkspaceRoot();
        if (!root || request !== generation) return;
        root = await registerProjectFolder(root.path);
      } else root = await registerProjectFolder(path);
      if (!root || request !== generation) return;
      projects = [...projects.filter(item => item.path !== root.path), root]; path = '';
      await save([...new Set([...selected, root.path])], request);
    } catch (cause) { if (request === generation) error = String(cause); }
    finally { if (request === generation) saving = false; }
  }
</script>
<section class="space-y-2 rounded border border-foundry-light-border dark:border-foundry-border p-3" aria-label="Assistant project settings">
  <h3 class="text-sm font-semibold">Saved projects</h3>
  <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">Selected projects are available in this assistant’s chats and knowledge. Chat attachments are saved separately.</p>
  {#if value}
    {#each options as root (root.path)}
      <label class="flex items-start gap-2 text-xs">
        <input type="checkbox" checked={selected.includes(root.path)} disabled={saving || (!selected.includes(root.path) && root.available === false)} onchange={event => toggle(event, root.path)} />
        <span>{root.name} <span class="break-all text-foundry-light-muted dark:text-foundry-text/60">{root.path}</span>
          {#if root.available === false}<span class="block text-amber-600">Unavailable — restore this folder or uncheck to remove it.</span>{/if}
        </span>
      </label>
    {/each}
    <label class="block text-xs">Absolute folder path on the server
      <input class="mt-1 w-full rounded border border-foundry-light-border dark:border-foundry-border bg-transparent p-2" bind:value={path} placeholder="/Users/me/Documents/My project" />
    </label>
    <div class="flex gap-3 text-xs">
      <button class="underline disabled:opacity-50" disabled={saving || !path.trim()} onclick={() => addFolder()}>Add saved project</button>
      {#if desktop}<button class="underline disabled:opacity-50" disabled={saving} onclick={() => addFolder(true)}>Choose folder…</button>{/if}
    </div>
    {#if saving}<p class="text-xs" role="status">Saving project settings…</p>{/if}
  {:else if !error}<p class="text-xs" role="status">Loading saved projects…</p>{/if}
  {#if error}<p class="text-xs text-red-600 dark:text-red-400" role="alert">{error}</p>{/if}
</section>
