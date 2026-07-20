<script>
  let { label, value, meta = '', accent = false, children } = $props();
</script>

<div class="stat">
  <div class="stat-label">{label}</div>
  {#if value}<div class="stat-value" class:accent>{value}</div>{/if}
  {@render children?.()}
  {#if meta}<div class="stat-meta">{meta}</div>{/if}
</div>

<style>
  .accent { color: var(--trusty-accent); }
</style>
