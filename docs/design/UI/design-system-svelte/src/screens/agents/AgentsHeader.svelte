<script>
  // Shared header for all trusty-agents (dark) screens.
  import RobotMark from '../../lib/RobotMark.svelte';
  import Badge from '../../lib/Badge.svelte';
  let { subtitle = 'UNIT-04 · MPM ORCHESTRATION', tab = 'CHAT', showTabs = true, right } = $props();
</script>

<header>
  <div class="left">
    <RobotMark size={30} body="#d97742" face="#201612" state="working" />
    <span class="brand">TRUSTY AGENTS</span>
    <span class="sub">{subtitle}</span>
  </div>
  <div class="right">
    {#if showTabs}
      <div class="tabs">
        <span class:on={tab === 'CHAT'}>CHAT</span>
        <span class:on={tab === 'PROJECTS'}>PROJECTS</span>
      </div>
    {/if}
    <span class="chip">DESKTOP</span>
    <Badge tone="success" dot>API READY</Badge>
    {@render right?.()}
  </div>
</header>

<style>
  header { height: 52px; flex: none; background: var(--trusty-sidebar-bg); border-bottom: 1px solid var(--trusty-sidebar-border); display: flex; align-items: center; justify-content: space-between; padding: 0 20px; }
  .left, .right { display: flex; align-items: center; gap: 12px; }
  .right { gap: 10px; }
  .brand { font: 700 15px var(--trusty-display); letter-spacing: 0.04em; }
  .sub { font: 500 10px var(--trusty-mono); letter-spacing: 0.12em; color: var(--trusty-text-muted); }
  .tabs { display: flex; gap: 2px; }
  .tabs span { padding: 5px 14px; font: 600 11px var(--trusty-mono); color: var(--trusty-text-muted); border-radius: 4px; }
  .tabs .on { color: var(--trusty-accent); background: var(--trusty-accent-soft); }
  .chip { padding: 2px 8px; border-radius: 4px; font: 600 10px var(--trusty-mono); background: var(--trusty-accent-soft); color: var(--trusty-sidebar-accent); border: 1px solid var(--trusty-border-strong); }
</style>
