<script>
  import Badge from './Badge.svelte';
  let { crumb, status = 'ONLINE v0.4.2', actions } = $props();
</script>

<header>
  <span class="crumb">// {crumb}</span>
  <div class="right">
    {@render actions?.()}
    <Badge tone="success" dot>{status}</Badge>
  </div>
</header>

<style>
  header {
    height: var(--trusty-topbar-height); flex: none;
    background: var(--trusty-surface-raised);
    border-bottom: 2px solid var(--trusty-border);
    display: flex; align-items: center; justify-content: space-between;
    padding: 0 32px;
  }
  .crumb { font: 600 13px var(--trusty-mono); letter-spacing: 0.1em; color: var(--trusty-text-secondary); }
  .right { display: flex; align-items: center; gap: 10px; }
</style>
