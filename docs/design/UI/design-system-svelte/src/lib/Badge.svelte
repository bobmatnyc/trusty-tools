<script>
  // tone: '' | 'success' | 'warning' | 'danger' | 'info' | 'muted'
  let { tone = '', dot = false, spinner = false, children } = $props();
</script>

<span class="badge inline {tone ? 'badge-' + tone : ''}">
  {#if spinner}<span class="spinner"></span>{:else if dot}<span class="dot"></span>{/if}
  {@render children?.()}
</span>

<style>
  .inline { display: inline-flex; align-items: center; gap: 6px; }
  .dot { width: 7px; height: 7px; border-radius: 50%; background: currentColor; flex: none; }
</style>
