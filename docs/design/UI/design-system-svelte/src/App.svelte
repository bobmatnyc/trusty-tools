<script>
  import Dashboard from './screens/search/Dashboard.svelte';
  import Search from './screens/search/Search.svelte';
  import Indexes from './screens/search/Indexes.svelte';
  import Health from './screens/search/Health.svelte';
  import Dialogs from './screens/search/Dialogs.svelte';
  import Console from './screens/console/Console.svelte';
  import Palaces from './screens/memory/Palaces.svelte';
  import AgentsChat from './screens/agents/AgentsChat.svelte';
  import AgentsProjects from './screens/agents/AgentsProjects.svelte';
  import AgentsAuth from './screens/agents/AgentsAuth.svelte';
  import AgentsFailure from './screens/agents/AgentsFailure.svelte';
  import AgentsRecap from './screens/agents/AgentsRecap.svelte';
  import CodeGui from './screens/code/CodeGui.svelte';
  import CodeTui from './screens/code/CodeTui.svelte';

  const groups = [
    { label: 'SEARCH', screens: [
      { id: 'dashboard', label: 'Dashboard', comp: Dashboard },
      { id: 'search', label: 'Search', comp: Search },
      { id: 'indexes', label: 'Indexes', comp: Indexes },
      { id: 'health', label: 'Health', comp: Health },
      { id: 'dialogs', label: 'Dialogs', comp: Dialogs }
    ]},
    { label: 'CONSOLE', screens: [{ id: 'console', label: 'Command deck', comp: Console }] },
    { label: 'MEMORY', screens: [{ id: 'palaces', label: 'Palaces', comp: Palaces }] },
    { label: 'AGENTS', screens: [
      { id: 'agents-chat', label: 'Chat', comp: AgentsChat },
      { id: 'agents-projects', label: 'Projects', comp: AgentsProjects },
      { id: 'agents-auth', label: 'Auth gate', comp: AgentsAuth },
      { id: 'agents-failure', label: 'Task failure', comp: AgentsFailure },
      { id: 'agents-recap', label: 'Recap', comp: AgentsRecap }
    ]},
    { label: 'CODE', screens: [
      { id: 'code-gui', label: 'GUI', comp: CodeGui },
      { id: 'code-tui', label: 'TUI', comp: CodeTui }
    ]}
  ];
  let current = $state(groups[0].screens[0]);
</script>

<div class="app">
  <nav>
    <div class="brand">FOUNDRY <span>· TRUSTY TOOLS</span></div>
    {#each groups as g}
      <span class="g-label">{g.label}</span>
      {#each g.screens as s}
        <button class:on={current.id === s.id} onclick={() => (current = s)}>{s.label}</button>
      {/each}
    {/each}
  </nav>
  <main>
    {#key current.id}
      {@const Screen = current.comp}
      <Screen />
    {/key}
  </main>
</div>

<style>
  .app { display: flex; min-height: 100vh; background: #211b17; }
  nav { width: 190px; flex: none; padding: 20px 14px; display: flex; flex-direction: column; gap: 3px; position: sticky; top: 0; align-self: flex-start; height: 100vh; box-sizing: border-box; overflow: auto; }
  .brand { font: 700 14px var(--trusty-display); color: #f5efe7; letter-spacing: 0.04em; margin-bottom: 14px; }
  .brand span { font: 500 9px var(--trusty-mono); color: #a58a6b; display: block; letter-spacing: 0.12em; margin-top: 2px; }
  .g-label { font: 600 9px var(--trusty-mono); letter-spacing: 0.16em; color: #a58a6b; margin: 12px 0 4px; }
  nav button { text-align: left; padding: 7px 10px; border: none; border-left: 3px solid transparent; border-radius: 4px; background: none; color: #cbb69c; font: 500 12.5px var(--trusty-font); cursor: pointer; }
  nav button:hover { background: rgba(217, 119, 66, 0.09); }
  nav button.on { background: #46311f; color: #e9b98a; font-weight: 600; border-left-color: #d97742; }
  main { flex: 1; padding: 32px; overflow: auto; }
</style>
