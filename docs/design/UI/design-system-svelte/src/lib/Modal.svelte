<script>
  // Set backdrop={false} to render the sheet inline (spec/dialog galleries).
  let { title, titleMeta = '', danger = false, backdrop = true, onclose, children, footer } = $props();
</script>

{#snippet sheet()}
  <div class="modal">
    <div class="modal-header" class:danger>
      <div class="titles">
        <span class="modal-title" class:danger-text={danger}>{title}</span>
        {#if titleMeta}<span class="meta">{titleMeta}</span>{/if}
      </div>
      <button class="x" onclick={onclose}>✕</button>
    </div>
    <div class="modal-body">{@render children?.()}</div>
    {#if footer}<div class="modal-footer">{@render footer()}</div>{/if}
  </div>
{/snippet}

{#if backdrop}
  <div class="modal-backdrop">{@render sheet()}</div>
{:else}
  {@render sheet()}
{/if}

<style>
  .titles { display: flex; align-items: baseline; gap: 10px; }
  .meta { font: 600 11px var(--trusty-mono); color: var(--trusty-accent); }
  .modal-header.danger { background: var(--trusty-danger-soft); }
  .danger-text { color: #8a2013; }
  .x { background: none; border: none; color: var(--trusty-text-muted); font-size: 15px; padding: 0; line-height: 1; }
</style>
