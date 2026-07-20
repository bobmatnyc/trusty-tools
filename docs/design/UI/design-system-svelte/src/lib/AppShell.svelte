<script>
  // Full-screen shell: dark sidebar + topbar + content pane. 1440x900 frame.
  import Sidebar from './Sidebar.svelte';
  import Topbar from './Topbar.svelte';
  let { sidebar, crumb, status, topbarActions, children } = $props();
</script>

<div class="shell">
  <Sidebar {...sidebar} />
  <div class="main">
    <Topbar {crumb} {status} actions={topbarActions} />
    <div class="content">
      {@render children?.()}
    </div>
  </div>
</div>

<style>
  .shell {
    width: 1440px; height: 900px; display: flex;
    background: var(--trusty-content-bg); overflow: hidden;
    font-size: 14px; color: var(--trusty-text-primary); position: relative;
  }
  .main { flex: 1; display: flex; flex-direction: column; min-width: 0; }
  .content { padding: 28px 32px; flex: 1; overflow: hidden; display: flex; flex-direction: column; min-height: 0; }
</style>
