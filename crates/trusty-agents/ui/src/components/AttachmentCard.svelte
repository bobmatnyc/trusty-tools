<script lang="ts">
  /**
   * Why (#7370): an attachment on a chat turn is an OBJECT, not a filename in
   * the text — the epic's wording is "shown as objects (thumbnail,
   * click-expandable)". A card carries the thumbnail or icon, the name and the
   * size, and expands to a full view on click, so a user can confirm what they
   * attached without leaving the conversation.
   * What: one card per `AttachmentRef`. Images render from the session's
   * authenticated byte route; text and CSV fetch their own bytes lazily — only
   * when the card is first expanded, so a thread full of files costs one
   * request per file the reader actually opens, not one per bubble painted.
   * Everything else is an icon, a name and a size, which is exactly what the
   * model was told about it too.
   * Test: `attachments.test.ts` covers the pure helpers this renders
   * (`csvPreview`, `formatSize`, `visibleText`); the component itself is
   * exercised by the chat view's own tests.
   */
  import { FileText, File as FileIcon, Table2 } from 'lucide-svelte';
  import {
    attachmentUrl,
    csvPreview,
    formatSize,
    isImage,
    isPreviewableText,
    type AttachmentRef,
  } from '../lib/attachments';

  export let attachment: AttachmentRef;

  let expanded = false;
  let text: string | null = null;
  let loadError: string | null = null;
  let loading = false;

  $: isCsv = attachment.media_type === 'text/csv';
  $: preview = isCsv && text ? csvPreview(text) : null;

  /**
   * Why: fetching every text attachment's bytes when the thread paints would
   * cost one request per file whether or not anyone looks. Deferring to the
   * first expand makes the cost follow the reader's attention.
   */
  async function loadText(): Promise<void> {
    if (text !== null || loading || !isPreviewableText(attachment)) return;
    loading = true;
    try {
      const r = await fetch(attachmentUrl(attachment));
      if (!r.ok) throw new Error(`HTTP ${r.status}`);
      text = await r.text();
      loadError = null;
    } catch (e) {
      loadError = `Could not load ${attachment.file_name}: ${e}`;
    } finally {
      loading = false;
    }
  }

  function toggle(): void {
    expanded = !expanded;
    if (expanded) void loadText();
  }
</script>

<div
  class="my-2 w-full max-w-md overflow-hidden rounded-lg border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface"
  data-attachment-card
  data-attachment-id={attachment.id}
>
  <button
    type="button"
    class="flex w-full items-center gap-3 px-3 py-2 text-left hover:bg-foundry-light-bg dark:hover:bg-foundry-bg"
    on:click={toggle}
    aria-expanded={expanded}
    title={expanded ? 'Collapse attachment' : 'Expand attachment'}
  >
    {#if isImage(attachment)}
      <img
        src={attachmentUrl(attachment)}
        alt={attachment.file_name}
        class="h-10 w-10 shrink-0 rounded object-cover"
      />
    {:else if isCsv}
      <Table2 class="h-5 w-5 shrink-0 text-foundry-teal" aria-hidden="true" />
    {:else if isPreviewableText(attachment)}
      <FileText class="h-5 w-5 shrink-0 text-foundry-teal" aria-hidden="true" />
    {:else}
      <FileIcon class="h-5 w-5 shrink-0 text-foundry-light-muted dark:text-foundry-text/60" aria-hidden="true" />
    {/if}
    <span class="min-w-0 flex-1">
      <span class="block truncate text-sm text-foundry-light-text dark:text-foundry-text">{attachment.file_name}</span>
      <span class="block text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
        {attachment.media_type} · {formatSize(attachment.size)}
      </span>
    </span>
  </button>

  {#if expanded}
    <div class="border-t border-foundry-light-border dark:border-foundry-border px-3 py-2">
      {#if loading}
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/50">Loading…</p>
      {:else if loadError}
        <p role="alert" class="text-xs text-red-600 dark:text-red-400">{loadError}</p>
      {:else if isImage(attachment)}
        <img src={attachmentUrl(attachment)} alt={attachment.file_name} class="max-h-96 w-auto rounded" />
      {:else if preview}
        <div class="overflow-x-auto">
          <table class="w-full border-collapse text-xs">
            <thead>
              <tr>
                {#each preview.header as cell, i (i)}
                  <th class="border border-foundry-light-border dark:border-foundry-border px-2 py-1 text-left font-medium">{cell}</th>
                {/each}
              </tr>
            </thead>
            <tbody>
              {#each preview.rows as cells, r (r)}
                <tr>
                  {#each cells as cell, c (c)}
                    <td class="border border-foundry-light-border dark:border-foundry-border px-2 py-1">{cell}</td>
                  {/each}
                </tr>
              {/each}
            </tbody>
          </table>
        </div>
        {#if preview.omitted > 0}
          <p class="mt-1 text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
            {preview.omitted} more row{preview.omitted === 1 ? '' : 's'} not shown
          </p>
        {/if}
      {:else if text !== null}
        <pre class="max-h-64 overflow-auto whitespace-pre-wrap break-words text-xs">{text.slice(0, 4000)}</pre>
      {:else}
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/50">
          Binary attachment — <a class="underline" href={attachmentUrl(attachment)} download={attachment.file_name}>download</a> to open it.
        </p>
      {/if}
    </div>
  {/if}
</div>

<style>
  [data-attachment-card] :global(svg) { flex: none; }
</style>
