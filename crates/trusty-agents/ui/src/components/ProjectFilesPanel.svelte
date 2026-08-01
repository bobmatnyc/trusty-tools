<script lang="ts">
  /**
   * Why (#4359): `MarkdownEditor` is deliberately ignorant of projects, routes
   * and persistence, so something has to own browsing a project root, deciding
   * what may be opened, and getting edits back to disk. Keeping that here — not
   * in the editor and not in `ProjectsView` (already 800 lines) — is what lets
   * #4360 change the gating rules and #4401 mount the same editor on a canvas
   * without either touching the other's file.
   *
   * What: the documents drawer of an expanded project card. Lists a directory
   * through #4357's `files/list`, opens markdown through `files/read` into the
   * editor, and saves through `files/write`. Non-markdown entries are listed
   * but not openable in this slice — the seam #4360 replaces with a
   * document-type table and a view-only preview.
   *
   * DEGRADE, NEVER FAKE: #4357's routes are not merged yet. When they answer
   * 404 this panel says so and shows the daemon's own message, matching
   * `AgentConfigKnowledge`'s three-state posture (loading / empty / error).
   * It never substitutes invented entries for an unreachable backend.
   *
   * Test: `ProjectFilesPanel.test.ts`.
   */
  import { onMount } from 'svelte';
  import { AlertCircle, ChevronLeft, FileText, Folder, Loader2, Save } from 'lucide-svelte';
  import type MarkdownEditor from './MarkdownEditor.svelte';
  import {
    isMarkdownPath,
    listProjectFiles,
    readProjectFile,
    writeProjectFile,
    type ProjectFileEntry,
  } from '../lib/projectFiles';

  /** Registry id of the project whose root is being browsed. */
  export let projectId: string;

  let dir = '';
  let entries: ProjectFileEntry[] | null = null;
  let listError = '';
  /** Open document: path + the last text known to be on disk + the draft. */
  let openPath = '';
  let savedText = '';
  let draft = '';
  let openError = '';
  let loadingDoc = false;
  let saving = false;
  let savedAt = '';

  /**
   * The editor is loaded on demand, not with the app.
   *
   * CodeMirror is ~180 kB gzipped — measured against this bundle it is larger
   * than everything else the app ships combined. Statically importing it would
   * put that on the startup path of every user, including the ones who never
   * open a document. Splitting it here costs one `await` on first open and
   * keeps the main chunk exactly the size it was.
   */
  let Editor: typeof MarkdownEditor | null = null;
  let editorError = '';

  async function ensureEditor() {
    if (Editor) return;
    try {
      Editor = (await import('./MarkdownEditor.svelte')).default;
    } catch (e) {
      editorError = `Could not load the markdown editor: ${(e as Error).message}`;
    }
  }

  $: dirty = openPath !== '' && draft !== savedText;

  /** Parent directory of `path`, or `''` at the project root. */
  function parentOf(path: string): string {
    const cut = path.lastIndexOf('/');
    return cut === -1 ? '' : path.slice(0, cut);
  }

  async function loadDir(next: string) {
    dir = next;
    entries = null;
    listError = '';
    try {
      const listed = await listProjectFiles(projectId, next);
      // Directories first, then names — a flat mtime order buries the tree
      // structure the browser exists to expose.
      entries = [...listed].sort((a, b) => {
        if (a.is_dir !== b.is_dir) return a.is_dir ? -1 : 1;
        return a.name.localeCompare(b.name);
      });
    } catch (e) {
      entries = [];
      listError = (e as Error).message;
    }
  }

  async function open(entry: ProjectFileEntry) {
    if (entry.is_dir) {
      closeDoc();
      await loadDir(entry.path);
      return;
    }
    if (!isMarkdownPath(entry.path)) return;
    loadingDoc = true;
    openError = '';
    savedAt = '';
    try {
      const [doc] = await Promise.all([readProjectFile(projectId, entry.path), ensureEditor()]);
      openPath = entry.path;
      savedText = doc.content;
      draft = doc.content;
    } catch (e) {
      openError = (e as Error).message;
      openPath = '';
    } finally {
      loadingDoc = false;
    }
  }

  function closeDoc() {
    openPath = '';
    savedText = '';
    draft = '';
    openError = '';
    savedAt = '';
  }

  async function save() {
    if (!openPath || saving) return;
    saving = true;
    openError = '';
    try {
      const written = draft;
      await writeProjectFile(projectId, openPath, written);
      savedText = written;
      savedAt = new Date().toLocaleTimeString();
    } catch (e) {
      openError = (e as Error).message;
    } finally {
      saving = false;
    }
  }

  // Browsing starts as soon as the drawer mounts; the parent only mounts this
  // component once the user has asked for documents, so no eager fetch happens
  // for projects nobody opened.
  onMount(() => {
    loadDir('');
  });
</script>

<div class="flex flex-col gap-2">
  <div class="flex items-center gap-2">
    {#if dir}
      <button
        type="button"
        on:click={() => { closeDoc(); loadDir(parentOf(dir)); }}
        class="inline-flex items-center gap-1 rounded-md border border-foundry-light-border dark:border-foundry-border px-2 py-0.5 text-xs text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10"
      >
        <ChevronLeft class="h-3 w-3" />
        Up
      </button>
    {/if}
    <code class="truncate font-mono text-xs text-foundry-light-muted dark:text-foundry-text/60">
      {dir || '/'}
    </code>
  </div>

  {#if entries === null}
    <p class="flex items-center gap-2 px-2 py-1 text-xs text-foundry-light-muted dark:text-foundry-text/60">
      <Loader2 class="h-3.5 w-3.5 animate-spin" /> Listing documents…
    </p>
  {:else if listError}
    <p
      class="flex items-start gap-1.5 rounded-md border border-red-500/40 bg-red-500/5 px-3 py-2 text-[11px] text-red-600 dark:text-red-400"
    >
      <AlertCircle class="mt-0.5 h-3.5 w-3.5 shrink-0" />
      <span>Could not list project files: {listError}</span>
    </p>
  {:else if entries.length === 0}
    <p class="px-2 py-1 text-xs text-foundry-light-muted dark:text-foundry-text/60">
      No files in this directory.
    </p>
  {:else}
    <ul class="flex max-h-56 flex-col gap-0.5 overflow-y-auto">
      {#each entries as entry (entry.path)}
        {@const editable = entry.is_dir || isMarkdownPath(entry.path)}
        <li>
          <button
            type="button"
            on:click={() => open(entry)}
            disabled={!editable}
            title={editable ? entry.path : 'Only markdown files can be opened yet'}
            class="flex w-full items-center gap-2 rounded-md px-2 py-1 text-left text-xs transition-colors {entry.path ===
            openPath
              ? 'bg-foundry-light-primary/10 dark:bg-foundry-primary/10 text-foundry-light-primary dark:text-foundry-primary'
              : 'text-foundry-light-text dark:text-foundry-text hover:bg-foundry-light-primary/5 dark:hover:bg-foundry-primary/5'} disabled:cursor-default disabled:text-foundry-light-muted/60 disabled:hover:bg-transparent dark:disabled:text-foundry-text/40"
          >
            {#if entry.is_dir}
              <Folder class="h-3.5 w-3.5 shrink-0 text-foundry-light-primary dark:text-foundry-primary" />
            {:else}
              <FileText class="h-3.5 w-3.5 shrink-0" />
            {/if}
            <span class="flex-1 truncate font-mono">{entry.name}</span>
            {#if !entry.is_dir && entry.size != null}
              <span class="shrink-0 text-[10px] text-foundry-light-muted dark:text-foundry-text/50">
                {entry.size} B
              </span>
            {/if}
          </button>
        </li>
      {/each}
    </ul>
  {/if}

  {#if loadingDoc}
    <p class="flex items-center gap-2 px-2 py-1 text-xs text-foundry-light-muted dark:text-foundry-text/60">
      <Loader2 class="h-3.5 w-3.5 animate-spin" /> Opening document…
    </p>
  {/if}

  {#if openError}
    <p
      class="flex items-start gap-1.5 rounded-md border border-red-500/40 bg-red-500/5 px-3 py-2 text-[11px] text-red-600 dark:text-red-400"
    >
      <AlertCircle class="mt-0.5 h-3.5 w-3.5 shrink-0" />
      <span>{openError}</span>
    </p>
  {/if}

  {#if openPath}
    <div class="flex flex-col gap-1.5">
      <div class="flex flex-wrap items-center gap-2">
        <code class="flex-1 truncate font-mono text-xs text-foundry-light-text dark:text-foundry-text">
          {openPath}
        </code>
        {#if dirty}
          <span class="text-[10px] text-foundry-amber">unsaved</span>
        {:else if savedAt}
          <span class="text-[10px] text-foundry-light-muted dark:text-foundry-text/50">
            saved {savedAt}
          </span>
        {/if}
        <button
          type="button"
          on:click={save}
          disabled={saving || !dirty}
          class="inline-flex items-center gap-1 rounded-md bg-foundry-light-primary dark:bg-foundry-primary px-2.5 py-1 text-xs font-medium text-white hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:opacity-50"
        >
          <Save class="h-3 w-3" />
          {saving ? 'Saving…' : 'Save'}
        </button>
        <button
          type="button"
          on:click={closeDoc}
          class="rounded-md border border-foundry-light-border dark:border-foundry-border px-2.5 py-1 text-xs text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10"
        >
          Close
        </button>
      </div>
      {#if Editor}
        <div class="h-72">
          <svelte:component
            this={Editor}
            value={draft}
            ariaLabel="Markdown editor for {openPath}"
            placeholder="This document is empty."
            on:change={(e) => (draft = e.detail.value)}
            on:save={save}
          />
        </div>
      {:else if editorError}
        <p
          class="flex items-start gap-1.5 rounded-md border border-red-500/40 bg-red-500/5 px-3 py-2 text-[11px] text-red-600 dark:text-red-400"
        >
          <AlertCircle class="mt-0.5 h-3.5 w-3.5 shrink-0" />
          <span>{editorError}</span>
        </p>
      {/if}
    </div>
  {/if}
</div>
