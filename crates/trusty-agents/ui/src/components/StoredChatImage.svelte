<script lang="ts">
  import { acquireStoredImage } from '../lib/storedImages';
  let { assistant, attachment }: { assistant?: string; attachment: { name: string; mime_type: string; asset_id: string } } = $props();
  let container: HTMLDivElement;
  let nearby = $state(false);
  let url = $state('');
  let error = $state('');
  let reload = $state(0);
  $effect(() => {
    if (typeof IntersectionObserver === 'undefined') return;
    const observer = new IntersectionObserver(entries => { nearby = entries.some(entry => entry.isIntersecting); }, { rootMargin: '200px' });
    observer.observe(container);
    return () => observer.disconnect();
  });
  $effect(() => {
    const owner = assistant, asset = attachment.asset_id;
    void reload;
    url = ''; error = '';
    if (!nearby) return;
    if (!owner) { error = 'Image owner is unavailable.'; return; }
    return acquireStoredImage(owner, asset, value => { url = value; }, value => { url = ''; error = value; });
  });
</script>
<div bind:this={container} class="min-h-16">
  {#if url}<img class="mb-1 max-h-32 max-w-48 object-contain" src={url} alt={attachment.name} />
  {:else if error}<p class="text-xs text-red-600 dark:text-red-400" role="alert">{error}</p><button type="button" class="text-xs underline" onclick={() => { reload++; }}>Load {attachment.name}</button>
  {:else if nearby}<p class="text-xs" role="status">Loading image…</p>
  {:else}<button type="button" class="text-xs underline" onclick={() => { nearby = true; }}>Load {attachment.name}</button>{/if}
</div>
