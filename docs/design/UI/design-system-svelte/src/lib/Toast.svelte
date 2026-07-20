<script>
  // tone: '' | 'success' | 'danger'
  let { tone = '', title, ondismiss, children } = $props();
</script>

<div class="toast {tone ? 'toast-' + tone : ''}">
  <div class="body-wrap">
    <div class="toast-title">{title}</div>
    <div class="toast-body">{@render children?.()}</div>
  </div>
  <button class="x" onclick={ondismiss}>✕</button>
</div>

<style>
  .body-wrap { flex: 1; }
  .x { background: none; border: none; color: var(--trusty-sidebar-muted); font-size: 14px; line-height: 1; padding: 0; }
</style>
