<script lang="ts">
  import { onDestroy } from 'svelte';
  import { X, RefreshCw } from 'lucide-svelte';
  import { openedFile } from '../stores/workspace';
  import { readWorkspaceFile, diffWorkspaceFile, type WorkspaceFile, type WorkspaceDiff } from '../lib/workspaceFiles';
  import { renderMarkdown, renderCode, relativeDocumentPath } from '../lib/fileRendering';

  let document: WorkspaceFile | null = null;
  let diff: WorkspaceDiff | null = null;
  let mode: 'preview' | 'source' | 'diff' = 'preview';
  let loading = false;
  let error = '';
  let notice = '';
  let sequence = 0;
  let key = '';

  $: nextKey = $openedFile ? `${$openedFile.root.id}\0${$openedFile.path}` : '';
  $: if (nextKey !== key) { key = nextKey; mode = 'preview'; load(); }
  $: markdownHtml = document?.kind === 'markdown' ? renderMarkdown(document.content) : '';
  $: codeHtml = document && (document.kind === 'code' || document.kind === 'markdown') ? renderCode(document.content, document.path) : '';

  async function load() {
    const selected = $openedFile;
    const request = ++sequence;
    document = null; diff = null; error = ''; notice = '';
    if (!selected) { loading = false; return; }
    loading = true;
    try {
      const result = await readWorkspaceFile(selected.root.id, selected.path);
      if (request === sequence) document = result;
    } catch (e) { if (request === sequence) error = String(e); }
    finally { if (request === sequence) loading = false; }
  }
  async function showDiff() {
    mode = 'diff';
    if (diff || loading) return;
    const selected = $openedFile;
    if (!selected) return;
    const request = sequence;
    loading = true; error = '';
    try {
      const result = await diffWorkspaceFile(selected.root.id, selected.path);
      if (request === sequence) diff = result;
    } catch (e) { if (request === sequence) error = String(e); }
    finally { if (request === sequence) loading = false; }
  }
  function followLink(event: MouseEvent | KeyboardEvent) {
    if (event instanceof KeyboardEvent && event.key !== 'Enter' && event.key !== ' ') return;
    const link = (event.target as Element).closest('a[data-local-href]');
    if (!link) return;
    event.preventDefault();
    const selected = $openedFile;
    const path = selected && relativeDocumentPath(selected.path, link.getAttribute('data-local-href') ?? '');
    if (selected && path) openedFile.set({ root: selected.root, path });
    else notice = 'This link is outside the selected folder. Choose its folder in the file browser to open it.';
  }
  function handleKey(event: KeyboardEvent) {
    if (event.key === 'Escape') openedFile.set(null);
  }
  onDestroy(() => { sequence++; });
</script>

<svelte:window on:keydown={handleKey} />
<div class="flex h-full min-h-0 min-w-0 flex-col bg-foundry-light-bg dark:bg-foundry-bg text-foundry-light-text dark:text-foundry-text" aria-label="File viewer">
  <header class="flex shrink-0 flex-wrap items-center gap-2 border-b border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface px-4 py-2">
    <div class="min-w-0 flex-1">
      <h1 class="truncate text-sm font-semibold" title={$openedFile?.path}>{$openedFile?.path}</h1>
      <p class="truncate text-[11px] text-foundry-light-muted dark:text-foundry-text/50">{$openedFile?.root.name} · Read only</p>
    </div>
    <div class="flex gap-1" role="group" aria-label="File display">
      <button type="button" aria-pressed={mode === 'preview'} on:click={() => mode = 'preview'} class="viewer-toggle">Preview</button>
      {#if document?.kind === 'markdown'}<button type="button" aria-pressed={mode === 'source'} on:click={() => mode = 'source'} class="viewer-toggle">Source</button>{/if}
      <button type="button" aria-pressed={mode === 'diff'} on:click={showDiff} disabled={loading} class="viewer-toggle">Diff</button>
    </div>
    <button type="button" aria-label="Refresh file" title="Refresh file" on:click={() => { mode = 'preview'; load(); }} class="p-2"><RefreshCw size={15}/></button>
    <button type="button" aria-label="Close file" title="Return to workspace" on:click={() => openedFile.set(null)} class="p-2"><X size={17}/></button>
  </header>
  <div class="min-h-0 flex-1 overflow-auto p-5">
    {#if loading}<p role="status" class="text-sm">Loading…</p>
    {:else if error}<p role="alert" class="text-sm text-red-600 dark:text-red-400">{error}</p>
    {:else if mode === 'diff'}
      <p class="mb-4 text-xs text-foundry-light-muted dark:text-foundry-text/60">Working file compared with Git HEAD · includes staged and unstaged changes</p>
      {#if diff?.available}
        {#if diff.diff}<pre class="text-xs leading-6 font-mono" aria-label="File diff">{#each diff.diff.split('\n') as line}<div class:diff-added={line.startsWith('+') && !line.startsWith('+++')} class:diff-removed={line.startsWith('-') && !line.startsWith('---')} class:diff-hunk={line.startsWith('@@')}>{line || ' '}</div>{/each}</pre>
        {:else}<p class="text-sm">No changes from HEAD.</p>{/if}
      {:else}<p class="text-sm">{diff?.reason ?? 'Diff is unavailable for this file.'}</p>{/if}
    {:else if document?.kind === 'image'}
      <div class="flex h-full items-center justify-center"><img src={document.content} alt={$openedFile?.path ?? 'Selected image'} class="max-h-full max-w-full object-contain" /></div>
    {:else if document?.kind === 'markdown' && mode === 'preview'}
      <!-- Sanitized HTML; local links are opened through the scoped native reader. -->
      <article class="document-prose" on:click={followLink} on:keydown={followLink} role="document">{@html markdownHtml}</article>
    {:else if document?.kind === 'code' || document?.kind === 'markdown'}
      <pre class="file-code text-sm leading-6" aria-label="File source"><code>{@html codeHtml}</code></pre>
    {:else if document}<p class="text-sm">This file type is not supported yet. Open Markdown, an image, or a text/code file.</p>{/if}
    {#if notice}<p role="status" class="mt-3 text-xs text-foundry-light-muted dark:text-foundry-text/60">{notice}</p>{/if}
  </div>
</div>

<style>
  .viewer-toggle { padding: .35rem .65rem; border-radius: .3rem; font-size: .75rem; }
  .viewer-toggle[aria-pressed='true'] { background: rgb(var(--color-primary) / .12); }
  .viewer-toggle:disabled { opacity: .4; }
  .diff-added { background: rgb(60 160 80 / .12); }
  .diff-removed { background: rgb(200 70 70 / .12); }
  .diff-hunk { color: rgb(var(--color-primary)); }
  .document-prose { overflow-wrap: anywhere; line-height: 1.75; font-size: .95rem; }
  .document-prose :global(h1) { font-size: 2rem; font-weight: 650; margin: .6em 0; }
  .document-prose :global(h2) { font-size: 1.5rem; font-weight: 600; margin: 1.2em 0 .5em; }
  .document-prose :global(h3) { font-size: 1.2rem; font-weight: 600; margin: 1em 0 .5em; }
  .document-prose :global(p), .document-prose :global(ul), .document-prose :global(ol) { margin: .75em 0; }
  .document-prose :global(ul) { list-style: disc; padding-left: 1.6em; }
  .document-prose :global(ol) { list-style: decimal; padding-left: 1.6em; }
  .document-prose :global(a) { text-decoration: underline; color: rgb(var(--color-primary)); }
  .document-prose :global(pre) { overflow: auto; padding: 1rem; border: 1px solid rgb(var(--color-border)); border-radius: .4rem; }
  .document-prose :global(code) { font-size: .85em; font-family: monospace; }
  .document-prose :global(blockquote) { border-left: 2px solid rgb(var(--color-border)); padding-left: 1em; }
  .document-prose :global(table) { border-collapse: collapse; margin: 1em 0; }
  .document-prose :global(th), .document-prose :global(td) { border: 1px solid rgb(var(--color-border)); padding: .4em .7em; }
  .file-code :global(.hljs-keyword), .file-code :global(.hljs-built_in) { color: rgb(var(--color-primary)); font-weight: 600; }
  .file-code :global(.hljs-string), .file-code :global(.hljs-title) { color: rgb(var(--color-success)); }
  .file-code :global(.hljs-comment) { opacity: .55; }
  .file-code :global(.hljs-number), .file-code :global(.hljs-literal) { color: rgb(var(--color-primary)); }
</style>
