<script lang="ts">
  import { onMount, onDestroy } from 'svelte';
  import { get } from 'svelte/store';
  import { Folder, File, ArrowLeft, ArrowUp, RefreshCw, Plus, Eye, EyeOff, Settings } from 'lucide-svelte';
  import ProjectConfiguration from './ProjectConfiguration.svelte';
  import { projects, projectsList, fetchProjects } from '../stores/app';
  import { isDesktop } from '../lib/transport';
  import { folderError, listWorkspaceFiles, DESKTOP_FILES_MESSAGE, type WorkspaceEntry, type WorkspaceRoot } from '../lib/workspaceFiles';
  import { activeRoot, openedFile, registeredWorkspaceRoots, currentChatRoots, chatProjectPath, chatWorkspaceKey, workspaceError, loadWorkspaceRoots, attachRootToChat, detachRootFromChat, selectChatRoot } from '../stores/workspace';
  const desktop = isDesktop();
  let browsing = false, adding = false, path = '', loading = false, refreshingProjects = false;
  let configurationRoot: WorkspaceRoot | null = null;
  let entries: WorkspaceEntry[] = [];
  let showHidden = false;
  $: visibleEntries = showHidden ? entries : entries.filter(entry => !entry.name.startsWith('.'));
  $: attached = $currentChatRoots.filter(root => root.available !== false);
  $: candidates = $registeredWorkspaceRoots.filter(root => root.available !== false && !attached.some(item => item.id === root.id || item.path === root.path));
  let error: string | null = null;
  let request = 0;
  let previousRootId: string | null | undefined = undefined;
  let previousChat = '';
  $: if (previousChat !== $chatWorkspaceKey) { previousChat = $chatWorkspaceKey; browsing = false; adding = false; configurationRoot = null; request++; entries = []; }
  $: projectPaths = [...new Set([...$projects, ...$projectsList].map(project => project.path).filter((path): path is string => !!path))];
  let reconciledPaths = '';
  $: if (desktop && !refreshingProjects && JSON.stringify(projectPaths) !== reconciledPaths) {
    reconciledPaths = JSON.stringify(projectPaths);
    void loadWorkspaceRoots(projectPaths);
  }
  $: rootState = browsing && $activeRoot ? JSON.stringify([$activeRoot.id, $activeRoot.path, $activeRoot.available]) : null;
  $: if (rootState !== previousRootId) { previousRootId = rootState; void loadDirectory(browsing ? $activeRoot : null, ''); }
  async function loadDirectory(root: WorkspaceRoot | null, nextPath: string) {
    const token = ++request;
    path = nextPath; entries = []; error = null; loading = !!root;
    if (!root) { loading = false; return; }
    if (root.available === false) { loading = false; error = folderError('folder not found'); return; }
    try {
      const result = await listWorkspaceFiles(root.id, nextPath);
      if (token !== request) return;
      entries = result.entries.slice().sort((a, b) => Number(b.is_dir) - Number(a.is_dir) || a.name.localeCompare(b.name));
    } catch (cause) { if (token === request) error = folderError(cause); }
    finally { if (token === request) loading = false; }
  }
  function browse(root: WorkspaceRoot) { activeRoot.set(root); browsing = true; showHidden = false; }
  function addProject(event: Event) {
    const root = candidates.find(root => root.id === (event.target as HTMLSelectElement).value);
    if (root) { attachRootToChat(root); adding = false; }
  }
  async function refreshProjects() {
    if (refreshingProjects) return;
    refreshingProjects = true; error = null;
    try {
      await fetchProjects(true);
      const paths = [...new Set([...get(projects), ...get(projectsList)].map(project => project.path).filter((path): path is string => !!path))];
      // Mark this snapshot reconciled before the reactive statement can issue a duplicate request.
      reconciledPaths = JSON.stringify(paths);
      await loadWorkspaceRoots(paths);
    } catch (cause) { error = 'Registered projects could not be loaded. ' + String(cause); }
    finally { refreshingProjects = false; }
  }
  async function refreshFiles() { await loadWorkspaceRoots(projectPaths); if (browsing) await loadDirectory($activeRoot, path); }
  function openEntry(entry: WorkspaceEntry) {
    if (!$activeRoot) return;
    if (entry.is_dir) void loadDirectory($activeRoot, entry.path);
    else openedFile.set({ root: $activeRoot, path: entry.path });
  }
  onMount(() => { if (desktop) { void fetchProjects(true).catch(cause => { error = 'Registered projects could not be loaded. ' + String(cause); }); } });
  onDestroy(() => { request++; });
</script>
<div class="navigator">
  {#if !desktop}<p class="notice">{DESKTOP_FILES_MESSAGE}</p>
  {:else if configurationRoot}
    <ProjectConfiguration root={configurationRoot} onClose={() => configurationRoot = null} />
  {:else}
    {#if $workspaceError}<p class="notice error" role="alert">{$workspaceError}</p>{/if}
    {#if !browsing}
      <div class="root-controls">
        <div class="drawer-heading"><strong>Projects in this chat</strong><button type="button" aria-label="Refresh projects" disabled={refreshingProjects} on:click={refreshProjects}><RefreshCw size={14} /></button></div>
        <button type="button" class="choose" aria-expanded={adding} on:click={() => adding = !adding}><Plus size={14} />Add project</button>
        {#if adding}
          <label for="registered-project">Registered project</label>
          <select id="registered-project" on:change={addProject}>
            <option value="">Select a project…</option>
            {#each candidates as root (root.id)}<option value={root.id}>{root.name} — {root.path}</option>{/each}
          </select>
          {#if !candidates.length}<p class="hint">No additional registered projects with available folders.</p>{/if}
        {/if}
      </div>
      {#if error}<p class="notice error" role="alert">{error}</p>{/if}
      <section class="project-list" aria-label="Projects attached to this chat">
        {#if !attached.length}<p class="notice">Add a registered project to browse its files in this chat.</p>{/if}
        {#each attached as root (root.id)}
          <div class="project-item">
            <div class="project-heading">
              <button class="project-name" type="button" title={root.path} on:click={() => browse(root)}><Folder size={15} /><span>{root.name}</span></button>
              <button type="button" aria-label={`Configure ${root.name}`} title={`Configure ${root.name}`} on:click={() => configurationRoot = root}><Settings size={15} /></button>
            </div>
            <p class="project-path" title={root.path}>{root.path}</p>
            <div class="project-actions">
              <button type="button" class:primary={$chatProjectPath === root.path} on:click={() => selectChatRoot(root)}>{$chatProjectPath === root.path ? 'Working folder' : 'Use for chat'}</button>
              <button type="button" aria-label={`Detach ${root.name} from this chat`} on:click={() => detachRootFromChat(root.id)}>Detach</button>
            </div>
          </div>
        {/each}
      </section>
    {:else}
      <button class="back-button" type="button" on:click={() => browsing = false}><ArrowLeft size={15} />Back to Projects</button>
      <div class="navigator-body">
        {#if $activeRoot}
          <div class="directory-bar">
            <button type="button" aria-label="Parent folder" disabled={!path} on:click={() => loadDirectory($activeRoot, path.split('/').slice(0, -1).join('/'))}><ArrowUp size={15} /></button>
            <button type="button" class="breadcrumb" title={`${$activeRoot.path}/${path}`} on:click={() => loadDirectory($activeRoot, '')}>{path || $activeRoot.name}</button>
            <button type="button" aria-label="Show hidden files" title={showHidden ? 'Hide dotfiles and dotfolders' : 'Show dotfiles and dotfolders'} aria-pressed={showHidden} on:click={() => showHidden = !showHidden}>{#if showHidden}<Eye size={14} />{:else}<EyeOff size={14} />{/if}</button>
            <button type="button" aria-label="Refresh files" on:click={refreshFiles}><RefreshCw size={14} /></button>
          </div>
        {/if}
        <div class="file-list" aria-label="Files" aria-busy={loading}>
          {#if error}<p class="notice error" role="alert">{error}</p>
          {:else if loading}<p class="notice" role="status">Loading files…</p>
          {:else if !$activeRoot}<p class="notice">This project folder is unavailable.</p>
          {:else if !visibleEntries.length}<p class="notice">{entries.length ? 'No visible files. Use “Show hidden files” to reveal hidden items.' : 'This folder is empty.'}</p>
          {:else}{#each visibleEntries as entry (entry.path)}
            <button type="button" class="file-row" class:selected={$openedFile?.root.id === $activeRoot?.id && $openedFile?.path === entry.path} title={entry.name} on:click={() => openEntry(entry)}>{#if entry.is_dir}<Folder size={15} />{:else}<File size={15} />{/if}<span>{entry.name}</span></button>
          {/each}{/if}
        </div>
      </div>
    {/if}
  {/if}
</div>
<style>
  .navigator { display:flex; flex-direction:column; height:100%; min-height:0; font-size:12px; color:rgb(var(--color-text-primary)); }
  button { cursor:pointer; } button:disabled { opacity:.45; cursor:default; }
  button:focus-visible, select:focus-visible { outline:2px solid rgb(var(--color-primary)); outline-offset:2px; }
  .back-button { display:flex; align-items:center; gap:7px; padding:11px 14px; border-bottom:1px solid rgb(var(--color-border) / .65); font-weight:600; }
  .project-heading { display:flex; align-items:center; gap:8px; }
  .project-name { min-width:0; flex:1; }
  .project-list { overflow:auto; padding:0 12px 12px; }
  .navigator-body { position:relative; display:flex; flex:1; flex-direction:column; min-height:0; }
  .drawer-heading { display:flex; justify-content:space-between; align-items:center; }
  .hint { font-size:11px; opacity:.6; margin:5px 0 12px; }
  .project-item { padding:9px 0; border-top:1px solid rgb(var(--color-border) / .65); }
  .project-name { display:flex; align-items:center; gap:6px; width:100%; text-align:left; font-weight:600; }
  .project-name span,.file-row span { overflow:hidden; text-overflow:ellipsis; white-space:nowrap; }
  .project-path { white-space:nowrap; overflow:hidden; text-overflow:ellipsis; font-size:10px; opacity:.65; margin:4px 0 7px; }
  .project-actions { display:flex; gap:10px; font-size:10px; }
  .primary { color:rgb(var(--color-primary)); font-weight:600; }
  .root-controls { padding:12px; display:flex; flex-direction:column; gap:8px; }
  label { font-size:10px; text-transform:uppercase; letter-spacing:.05em; opacity:.65; }
  select { width:100%; min-width:0; border:1px solid rgb(var(--color-border)); padding:6px; border-radius:5px; background:transparent; color:inherit; }
  option { background:rgb(var(--color-card-bg)); color:rgb(var(--color-text-primary)); }
  .choose { display:flex; align-items:center; justify-content:center; gap:5px; border:1px solid rgb(var(--color-border)); border-radius:5px; padding:6px; }
  .directory-bar { display:flex; align-items:center; gap:7px; padding:7px 12px; border-block:1px solid rgb(var(--color-border) / .65); }
  .breadcrumb { flex:1; text-align:left; overflow:hidden; text-overflow:ellipsis; white-space:nowrap; font-size:11px; }
  .file-list { overflow:auto; flex:1; padding:5px; }
  .file-row { display:flex; width:100%; align-items:center; gap:7px; text-align:left; padding:7px 9px; border-radius:4px; }
  .file-row :global(svg) { flex-shrink:0; opacity:.65; }
  .file-row:hover,.file-row.selected { background:rgb(var(--color-primary) / .10); }
  .notice { padding:12px; font-size:12px; opacity:.65; overflow-wrap:anywhere; }
  .error { color:rgb(var(--color-warning)); opacity:1; }
</style>
