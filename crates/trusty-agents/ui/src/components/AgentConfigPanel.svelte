<script lang="ts">
  /**
   * Why (#3819, epic #3052 — Bob's concept-demo centerpiece: "this agent -
   * OKG - tool - listener is the most revolutionary feature of trusty
   * agents"): the old top-nav Personality tab (a separate overlay-creation
   * flow) is replaced by an IN-PANE config form scoped to whichever agent is
   * currently active in the chat pane, opened via the gear icon in
   * `ChatHeader`. Five sections, in the order Bob specified: Personality
   * (main persona.md prose — real read/write), OKG Stores (the agent's bound
   * knowledge trees/search indexes — no `agent.toml` field exists yet,
   * confirmed by grep; rendered as honest structured scaffolding, not
   * fabricated live numbers), Tools (the `[tools].allow` glob allowlist —
   * real read/write), Permissions (`[agent].scopes` — real, READ-ONLY: no
   * write contract established yet), Listeners (gmail/google-calendar —
   * no backend yet, spec being filed; rendered from `DEFINED_LISTENERS` as
   * structured concept scaffolding per Bob's explicit ask).
   * What: Fetches `AgentDetail` + `AgentPersona` for `agentName` on mount
   * and whenever `agentName` changes. Personality and Tools are editable
   * with their own Save button (independent PATCH calls, so saving one
   * doesn't require the other to be valid). Permissions/OKG
   * Stores/Listeners render read-only. Concierge (`ctrl`) gets a static
   * notice per Bob: editable by name, but not an add-agent template.
   * Test: `agentConfig.test.ts` covers the scaffolding data; this component
   * is exercised manually (`pnpm dev`, open the gear, edit + save
   * Personality/Tools, confirm a re-open shows the persisted value) per the
   * project's existing convention for fetch-driven Svelte views (see
   * `PersonalityPanel.svelte`'s own "Test: Manual" precedent).
   */
  import { AlertCircle, Save, Loader2, Settings2 } from 'lucide-svelte';
  import { onMount } from 'svelte';
  import {
    fetchAgentDetail,
    fetchAgentPersona,
    patchAgent,
    scaffoldOkgStores,
    DEFINED_LISTENERS,
    type AgentDetail,
  } from '../lib/agentConfig';

  export let agentName: string;
  /** True when the active pane is Concierge (`ctrl`) — fixed coordination
   * layer, no template derivations (Bob). Edits are still allowed. */
  export let isConcierge = false;

  type Tab = 'personality' | 'okg' | 'tools' | 'permissions' | 'listeners';
  let tab: Tab = 'personality';

  let loading = true;
  let loadError = '';
  let detail: AgentDetail | null = null;
  let personaContent = '';
  let personaEditable = false;
  let savedPersonaContent = '';

  let toolsText = ''; // one glob per line — simplest editable form of a string[]
  let savedToolsText = '';

  let saving = false;
  let saveError = '';
  let justSaved: Tab | null = null;
  let justSavedTimer: ReturnType<typeof setTimeout> | null = null;

  $: personalityDirty = personaContent !== savedPersonaContent;
  $: toolsDirty = toolsText !== savedToolsText;

  function toolsArrayFromText(text: string): string[] {
    return text
      .split('\n')
      .map((s) => s.trim())
      .filter((s) => s.length > 0);
  }

  async function load(name: string) {
    loading = true;
    loadError = '';
    detail = null;
    try {
      const [d, p] = await Promise.all([fetchAgentDetail(name), fetchAgentPersona(name)]);
      detail = d;
      if (d) {
        savedToolsText = d.tools_allow.join('\n');
        toolsText = savedToolsText;
      }
      if (p) {
        personaEditable = p.editable;
        savedPersonaContent = p.content ?? '';
        personaContent = savedPersonaContent;
      } else {
        personaEditable = false;
        savedPersonaContent = '';
        personaContent = '';
      }
      if (!d) {
        loadError = `Agent "${name}" not found.`;
      }
    } catch (e) {
      loadError = `Failed to load agent config: ${e}`;
    } finally {
      loading = false;
    }
  }

  $: load(agentName);

  function flashSaved(which: Tab) {
    justSaved = which;
    if (justSavedTimer) clearTimeout(justSavedTimer);
    justSavedTimer = setTimeout(() => {
      justSaved = null;
    }, 2000);
  }

  async function savePersonality() {
    saving = true;
    saveError = '';
    try {
      detail = await patchAgent(agentName, { personality: personaContent });
      savedPersonaContent = personaContent;
      flashSaved('personality');
    } catch (e) {
      saveError = `${e}`;
    } finally {
      saving = false;
    }
  }

  async function saveTools() {
    saving = true;
    saveError = '';
    try {
      detail = await patchAgent(agentName, { tools_allow: toolsArrayFromText(toolsText) });
      savedToolsText = toolsText;
      flashSaved('tools');
    } catch (e) {
      saveError = `${e}`;
    } finally {
      saving = false;
    }
  }

  $: okgStores = scaffoldOkgStores(agentName);

  onMount(() => () => {
    if (justSavedTimer) clearTimeout(justSavedTimer);
  });
</script>

<div class="flex h-full w-full flex-col overflow-hidden bg-foundry-light-surface dark:bg-foundry-surface">
  <header class="flex items-center gap-2 border-b border-foundry-light-border dark:border-foundry-border px-4 py-3">
    <Settings2 class="h-4 w-4 text-foundry-light-primary dark:text-foundry-primary" />
    <h2 class="font-mono text-xs font-semibold uppercase tracking-wide text-foundry-light-text dark:text-foundry-text">
      Configure {detail?.display_name ?? agentName}
    </h2>
  </header>

  {#if isConcierge}
    <p class="mx-4 mt-3 rounded-md border border-foundry-amber/40 bg-foundry-amber/10 px-3 py-2 text-xs text-foundry-amber">
      Concierge is trusty-agents' fixed coordination layer — editable here, but not offered as an
      add-agent template.
    </p>
  {/if}

  <div class="flex flex-wrap gap-1 border-b border-foundry-light-border dark:border-foundry-border px-3 py-2" role="tablist">
    {#each [['personality', 'Personality'], ['okg', 'OKG Stores'], ['tools', 'Tools'], ['permissions', 'Permissions'], ['listeners', 'Listeners']] as [id, label] (id)}
      <button
        type="button"
        role="tab"
        aria-selected={tab === id}
        class="rounded px-2.5 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide transition-colors {tab === id
          ? 'bg-foundry-light-primary/20 dark:bg-foundry-primary/20 text-foundry-light-primary dark:text-foundry-primary'
          : 'text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
        on:click={() => (tab = id as Tab)}
      >
        {label}
      </button>
    {/each}
  </div>

  <div class="flex-1 overflow-y-auto px-4 py-3">
    {#if loading}
      <div class="flex items-center gap-2 text-sm text-foundry-light-muted dark:text-foundry-text/60">
        <Loader2 class="h-4 w-4 animate-spin" /> Loading…
      </div>
    {:else if loadError}
      <div class="flex items-center gap-2 text-sm text-red-500 dark:text-red-400">
        <AlertCircle class="h-4 w-4" /> {loadError}
      </div>
    {:else if tab === 'personality'}
      <div class="flex h-full flex-col gap-2">
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
          Main instructions — this agent's <code class="font-mono">persona.md</code>.
        </p>
        {#if !personaEditable}
          <p class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2 text-xs text-foundry-light-muted dark:text-foundry-text/50">
            This agent has no editable persona.md (flat-file agents don't carry a separate
            personality file).
          </p>
        {:else}
          <textarea
            bind:value={personaContent}
            class="w-full flex-1 min-h-[55vh] max-h-[70vh] resize-y overflow-y-auto rounded-md border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-bg dark:bg-foundry-bg px-3 py-2 font-mono text-xs text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
          ></textarea>
          <div class="sticky bottom-0 flex items-center gap-2 bg-foundry-light-surface dark:bg-foundry-surface pt-1 pb-1">
            <button
              type="button"
              class="inline-flex items-center gap-1.5 rounded-md bg-foundry-light-primary dark:bg-foundry-primary px-3 py-1.5 text-xs font-semibold text-white shadow-sm hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:opacity-50"
              disabled={saving || !personalityDirty}
              on:click={savePersonality}
            >
              <Save class="h-3.5 w-3.5" /> {saving ? 'Saving…' : 'Save'}
            </button>
            {#if justSaved === 'personality'}
              <span class="text-xs text-foundry-teal">Saved</span>
            {/if}
          </div>
        {/if}
        <p class="pt-2 text-xs text-foundry-light-muted dark:text-foundry-text/40">
          Event-specific instructions (per-connector context loaded on an incoming event — e.g. a
          Gmail event loading Gmail-connector instructions) are a separate runtime epic. This
          section is scaffolding until that backend exists.
        </p>
        <div class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-xs text-foundry-light-muted dark:text-foundry-text/40">
          {#each DEFINED_LISTENERS as listener (listener.id)}
            <div class="py-0.5">{listener.label}: no event instructions configured</div>
          {/each}
        </div>
      </div>
    {:else if tab === 'okg'}
      <div class="flex flex-col gap-2">
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
          Knowledge trees + search indexes bound to this agent.
        </p>
        <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
          Scaffolding — <code class="font-mono">agent.toml</code> has no OKG store binding field
          yet. Shown here so the concept reads end to end; not yet connected to a live index.
        </p>
        {#each okgStores as store (store.name)}
          <div class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
            <div class="flex items-center justify-between">
              <span class="font-mono text-xs font-semibold text-foundry-light-text dark:text-foundry-text">{store.name}</span>
              <span class="rounded-md bg-foundry-light-border/50 dark:bg-black/30 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">
                not connected
              </span>
            </div>
            <div class="mt-1 flex flex-col gap-0.5 font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
              <span>tree: {store.tree}</span>
              <span>index: {store.index}</span>
            </div>
          </div>
        {/each}
      </div>
    {:else if tab === 'tools'}
      <div class="flex h-full flex-col gap-2">
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
          MCP tool allow-list — one glob pattern per line (e.g. <code class="font-mono">gworkspace_*</code>). Empty = no restriction.
        </p>
        <textarea
          bind:value={toolsText}
          placeholder="gworkspace_*&#10;memory_*"
          class="w-full flex-1 min-h-[55vh] max-h-[70vh] resize-y overflow-y-auto rounded-md border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-bg dark:bg-foundry-bg px-3 py-2 font-mono text-xs text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
        ></textarea>
        <div class="flex items-center gap-2 pt-1">
          <button
            type="button"
            class="inline-flex items-center gap-1.5 rounded-md bg-foundry-light-primary dark:bg-foundry-primary px-3 py-1.5 text-xs font-semibold text-white shadow-sm hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:opacity-50"
            disabled={saving || !toolsDirty}
            on:click={saveTools}
          >
            <Save class="h-3.5 w-3.5" /> {saving ? 'Saving…' : 'Save'}
          </button>
          {#if justSaved === 'tools'}
            <span class="text-xs text-foundry-teal">Saved</span>
          {/if}
        </div>
      </div>
    {:else if tab === 'permissions'}
      <div class="flex flex-col gap-2">
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
          RBAC / OpenRPC scopes claimed by this agent — read-only (no write path yet).
        </p>
        {#if detail && detail.scopes.length > 0}
          <ul class="flex flex-wrap gap-1.5">
            {#each detail.scopes as scope (scope)}
              <li class="rounded-md border border-foundry-light-border dark:border-foundry-border px-2 py-0.5 font-mono text-[11px] text-foundry-light-text dark:text-foundry-text">
                {scope}
              </li>
            {/each}
          </ul>
        {:else}
          <p class="text-xs text-foundry-light-muted dark:text-foundry-text/40">No scopes declared.</p>
        {/if}
      </div>
    {:else if tab === 'listeners'}
      <div class="flex flex-col gap-3">
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
          Inbound event bindings — API connections to upstream event providers, not MCP tools. No
          backend yet (spec being filed); shown as the two defined listener bindings.
        </p>
        {#each DEFINED_LISTENERS as listener (listener.id)}
          <div class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
            <div class="flex items-center justify-between">
              <span class="text-sm font-semibold text-foundry-light-text dark:text-foundry-text">{listener.label}</span>
              <span class="rounded-md bg-foundry-light-border/50 dark:bg-black/30 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">
                not bound
              </span>
            </div>
            <p class="mt-0.5 text-[11px] text-foundry-light-muted dark:text-foundry-text/50">{listener.description}</p>
            <div class="mt-1.5 flex flex-wrap gap-1">
              {#each listener.eventTypes as eventType (eventType)}
                <span class="rounded border border-dashed border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] text-foundry-light-muted dark:text-foundry-text/40">
                  {eventType}
                </span>
              {/each}
            </div>
          </div>
        {/each}
      </div>
    {/if}

    {#if saveError}
      <p class="mt-2 text-xs text-red-500 dark:text-red-400">{saveError}</p>
    {/if}
  </div>
</div>
