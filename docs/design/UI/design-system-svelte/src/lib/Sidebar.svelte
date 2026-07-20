<script>
  import RobotMark from './RobotMark.svelte';
  let {
    title,
    unit,
    items = [],            // [{ icon, label, active }]
    accent = 'var(--trusty-accent)',
    mark = {}              // extra RobotMark props
  } = $props();
</script>

<aside style="border-right-color:{accent};">
  <div class="brand">
    <RobotMark size={40} state="idle" {...mark} />
    <div>
      <div class="title">{title}</div>
      <div class="unit">{unit}</div>
    </div>
  </div>
  <nav>
    {#each items as it}
      <div class="item" class:active={it.active}>
        <span class="icon">{it.icon}</span><span>{it.label}</span>
      </div>
    {/each}
  </nav>
  <div class="foot">TRUSTY WORKS ⚙ EST. 2026</div>
</aside>

<style>
  aside {
    width: var(--trusty-sidebar-width); flex: none;
    background: var(--trusty-sidebar-bg); color: var(--trusty-sidebar-text);
    display: flex; flex-direction: column; border-right: 3px solid;
  }
  .brand { padding: 20px 18px; display: flex; align-items: center; gap: 12px; border-bottom: 1px solid var(--trusty-sidebar-border); }
  .title { font: 700 15px var(--trusty-display); color: #f5efe7; letter-spacing: 0.04em; }
  .unit { font: 500 10px var(--trusty-mono); color: var(--trusty-sidebar-muted); letter-spacing: 0.12em; }
  nav { padding: 14px 10px; flex: 1; display: flex; flex-direction: column; gap: 3px; }
  .item { display: flex; align-items: center; gap: 11px; padding: 10px 13px; border-radius: 4px; border-left: 3px solid transparent; font-weight: 500; cursor: pointer; }
  .item:hover:not(.active) { background: var(--trusty-surface-hover); }
  .item.active { background: var(--trusty-sidebar-active); color: var(--trusty-sidebar-accent); font-weight: 600; border-left-color: #d97742; }
  .icon { width: 15px; text-align: center; }
  .foot { padding: 14px 18px; border-top: 1px solid var(--trusty-sidebar-border); font: 500 10px var(--trusty-mono); color: var(--trusty-sidebar-muted); letter-spacing: 0.12em; }
</style>
