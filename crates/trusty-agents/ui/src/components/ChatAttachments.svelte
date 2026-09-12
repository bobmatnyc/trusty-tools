<script lang="ts">
  import StoredChatImage from './StoredChatImage.svelte';
  import { attachmentSummary, type DisplayChatAttachment } from '../lib/chatAttachments';
  let { attachments, remove, assistant }: { attachments: DisplayChatAttachment[]; assistant?: string; remove?: (index: number) => void } = $props();
</script>
{#if attachments.length}
  <ul class="flex flex-wrap gap-2 px-3 py-2" aria-label="Message attachments">
    {#each attachments as attachment, index}
      <li class="min-w-0 max-w-full rounded border border-foundry-light-border dark:border-foundry-border p-2 text-xs">
        {#if attachment.kind === 'image' && 'asset_id' in attachment}
          <StoredChatImage {assistant} attachment={attachment} />
        {:else if attachment.kind === 'image'}
          <img class="mb-1 max-h-32 max-w-48 object-contain" src={`data:${attachment.mime_type};base64,${attachment.data_base64}`} alt={attachment.name} />
        {/if}
        <span class="break-all">{attachmentSummary(attachment)}</span>
        {#if remove}<button class="ml-2 underline" type="button" aria-label={`Remove ${attachment.name}`} onclick={() => remove?.(index)}>Remove</button>{/if}
        {#if attachment.kind === 'table'}
          <details class="mt-1"><summary class="cursor-pointer">Preview table</summary>
            {#each attachment.sheets as sheet}
              <div class="max-h-48 overflow-auto"><table class="border-collapse text-left"><caption class="text-left font-semibold">{sheet.name}</caption><tbody>
                {#each sheet.rows as row}<tr>{#each row as cell}<td class="whitespace-pre-wrap border border-foundry-light-border dark:border-foundry-border px-2 py-1">{cell}</td>{/each}</tr>{/each}
              </tbody></table></div>
            {/each}
          </details>
        {/if}
      </li>
    {/each}
  </ul>
{/if}
