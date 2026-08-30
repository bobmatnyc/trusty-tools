<script>
  /*
   * Why: Fixed-position dark sidebar matching the trusty-memory layout so
   * operators jumping between the two tools get a consistent shell.
   * What: Six top-level routes — Dashboard, Search, Indexes, Health, Logs,
   * Config — backed by the hash router. Brand text reads "Trusty Search".
   * Test: Click each nav item, confirm hash updates and `.active` moves.
   */
  import { getRoute, navigate } from '../router.svelte.js';

  let route = $derived(getRoute());

  const links = [
    { path: '/', label: 'Dashboard', icon: '◇' },
    { path: '/search', label: 'Search', icon: '⌕' },
    { path: '/indexes', label: 'Indexes', icon: '▣' },
    { path: '/health', label: 'Health', icon: '♥' },
    { path: '/logs', label: 'Logs', icon: '☰' },
    { path: '/config', label: 'Config', icon: '⚙' }
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
      <div class="brand-title">Trusty Search</div>
      <div class="brand-sub">Code Search</div>
    </div>
  </div>
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
        <span class="icon">{link.icon}</span>
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
    color: var(--trusty-text-inverse);
  }
  .brand-sub {
    font-size: 11px;
    color: var(--trusty-sidebar-muted);
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
    text-align: center;
  }
  .footer {
    padding: 16px 20px;
    border-top: 1px solid var(--trusty-sidebar-border);
  }
  .footer .text-muted {
    color: var(--trusty-sidebar-muted);
  }
</style>
