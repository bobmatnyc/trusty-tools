<script>
  /*
   * Why: Fixed-position dark sidebar matching the trusty-search layout so
   * operators jumping between tools get a consistent shell.
   * What: Five top-level routes — Dashboard, Indexes, Smells, Facts, Config —
   * backed by the hash router. Brand text reads "Trusty Analyzer".
   * #6439: each route's nav glyph used to be a bespoke unicode character,
   * one divergent set per dashboard (owner directive: "use the same
   * iconography"). Now it names a glyph from the shared Foundry ActionIcon
   * vocabulary (docs/design/UI/design-system/icons/README.md), vendored
   * alongside this component.
   * Test: Click each nav item, confirm hash updates and `.active` moves.
   */
  import { getRoute, navigate } from '../router.svelte.js';
  import { getSelectedIndex } from '../state.svelte.js';
  import ActionIcon from './ActionIcon.svelte';

  let route = $derived(getRoute());
  let selected = $derived(getSelectedIndex());

  const links = [
    { path: '/', label: 'Dashboard', icon: 'home' },
    { path: '/complexity', label: 'Complexity', icon: 'workflow' },
    { path: '/smells', label: 'Smells', icon: 'smells' },
    { path: '/refactors', label: 'Refactors', icon: 'write_file' },
    { path: '/clusters', label: 'Clusters', icon: 'clusters' },
    { path: '/facts', label: 'Facts', icon: 'review' }
  ];

  function isActive(path) {
    if (path === '/') return route.path === '/' || route.path === '';
    return route.path.startsWith(path);
  }
</script>

<aside class="sidebar">
  <div class="brand">
    <div class="logo">T</div>
    <div>
      <div class="brand-title">Trusty Analyzer</div>
      <div class="brand-sub">Code Analysis</div>
    </div>
  </div>
  {#if selected}
    <div class="selected-index">
      <div class="text-xs sel-label">Active Index</div>
      <div class="sel-value truncate" title={selected}>{selected}</div>
    </div>
  {/if}
  <nav>
    {#each links as link}
      <a
        href={'#' + link.path}
        class="nav-link"
        class:active={isActive(link.path)}
        onclick={(e) => {
          e.preventDefault();
          navigate(link.path);
        }}
      >
        <span class="icon"><ActionIcon name={link.icon} size={16} /></span>
        <span>{link.label}</span>
      </a>
    {/each}
  </nav>
  <div class="footer">
    <div class="text-xs text-muted">Trusty Suite</div>
  </div>
</aside>

<style>
  .sidebar {
    position: fixed;
    top: 0;
    left: 0;
    bottom: 0;
    width: var(--trusty-sidebar-width);
    background: var(--trusty-sidebar-bg);
    color: var(--trusty-sidebar-text);
    display: flex;
    flex-direction: column;
    border-right: 1px solid var(--trusty-sidebar-border);
  }
  .brand {
    padding: 20px 20px;
    display: flex;
    align-items: center;
    gap: 12px;
    border-bottom: 1px solid var(--trusty-sidebar-border);
  }
  .logo {
    width: 36px;
    height: 36px;
    border-radius: 8px;
    background: var(--trusty-accent);
    color: white;
    display: flex;
    align-items: center;
    justify-content: center;
    font-weight: 700;
    font-size: 18px;
  }
  .brand-title {
    font-weight: 600;
    font-size: 15px;
    color: var(--text);
  }
  .brand-sub {
    font-size: 11px;
    color: var(--trusty-sidebar-muted);
  }
  .selected-index {
    padding: 10px 20px;
    border-bottom: 1px solid var(--trusty-sidebar-border);
  }
  .sel-label {
    color: var(--trusty-sidebar-muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    margin-bottom: 2px;
  }
  .sel-value {
    color: var(--trusty-sidebar-accent);
    font-family: var(--trusty-mono);
    font-size: 12px;
  }
  nav {
    padding: 16px 12px;
    flex: 1;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .nav-link {
    display: flex;
    align-items: center;
    gap: 12px;
    padding: 10px 14px;
    border-radius: 6px;
    color: var(--trusty-sidebar-text);
    text-decoration: none;
    font-size: 14px;
    font-weight: 500;
    transition: background 0.15s ease;
  }
  .nav-link:hover {
    background: var(--trusty-sidebar-active);
    text-decoration: none;
  }
  .nav-link.active {
    background: var(--trusty-sidebar-active);
    color: var(--trusty-sidebar-accent);
  }
  .icon {
    width: 16px;
    flex-shrink: 0;
    display: flex;
    align-items: center;
    justify-content: center;
  }
  .footer {
    padding: 16px 20px;
    border-top: 1px solid var(--trusty-sidebar-border);
  }
  .footer .text-muted {
    color: var(--trusty-sidebar-muted);
  }
</style>
