<script lang="ts">
  /**
   * Why (#3819, epic #3052 — Bob's concept-demo centerpiece: "this agent -
   * OKG - tool - listener is the most revolutionary feature of trusty
   * agents"): the old top-nav Personality tab (a separate overlay-creation
   * flow) is replaced by an IN-PANE config form scoped to whichever agent is
   * currently active in the chat pane, opened via the gear icon in
   * `ChatHeader`. Five sections, in the order Bob specified: Personality
   * (main persona.md prose — real read/write), OKG Stores (the agent's bound
   * knowledge trees/search indexes — REAL as of #3816/#3864: read from
   * `agent.toml`'s `[[stores]]` and resolved live against trusty-search /
   * trusty-memory by `GET /api/agents/:name/stores`), Tools (the
   * `[tools].allow` glob allowlist —
   * real read/write), Permissions (`[agent].scopes` — real, READ-ONLY: no
   * write contract established yet), Listeners (gmail/google-calendar —
   * no backend yet, spec being filed; rendered from `DEFINED_LISTENERS` as
   * structured concept scaffolding per Bob's explicit ask).
   * What: Fetches `AgentDetail` + `AgentPersona` for `agentName` on mount
   * and whenever `agentName` changes. Personality and Tools are editable
   * with their own Save button (independent PATCH calls, so saving one
   * doesn't require the other to be valid). Permissions/OKG
   * Stores/Listeners render read-only — store bindings are edited in
   * `agent.toml`, not here. Concierge (`ctrl`) gets a static
   * notice per Bob: editable by name, but not an add-agent template.
   *
   * #3894 (Bob: "when we're in agent configuration, let's take over that
   * pane"): this panel no longer shares the chat column as a 320px strip —
   * `AgentConfigOverlay` mounts it as a full-pane takeover, so the layout here
   * is now a height-filling flex column: header + tabs are `shrink-0` and the
   * active tab takes the rest, with the instructions editor growing into every
   * remaining pixel instead of #3862's `55vh/70vh` clamp. The header carries
   * the takeover's exit affordances (Back / Close, plus an Esc hint —
   * `AgentConfigOverlay` owns the key handler) via the `onExit` prop, and no
   * exit path reaches `onExit` without passing the unsaved-changes confirm
   * (see `requestExit` / `configPaneDirty`).
   * Test: `AgentConfigPanel.test.ts` (exit affordances, instructions-editor
   * sizing invariants, tab preservation) + `agentConfig.test.ts` (scaffolding
   * data); the save round-trip stays manual (`pnpm dev`, open the gear, edit +
   * save Personality/Tools, confirm a re-open shows the persisted value) per
   * the project's convention for fetch-driven Svelte views.
   */
  import { AlertCircle, ArrowLeft, Save, Loader2, Settings2, X } from 'lucide-svelte';
  import { onDestroy, onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { configExitIntent, configPaneDirty } from '../stores/configPane';
  import {
    fetchAgentDetail,
    fetchAgentPersona,
    patchAgent,
    fetchAgentStores,
    DEFINED_LISTENERS,
    type AgentDetail,
    type OkgStoreBinding,
  } from '../lib/agentConfig';

  export let agentName: string;
  /** True when the active pane is Concierge (`ctrl`) — fixed coordination
   * layer, no template derivations (Bob). Edits are still allowed. */
  export let isConcierge = false;
  /**
   * Why (#3894): the takeover covers `ChatHeader`, which used to be the thing
   * naming the selected agent ("Concierge"), so this header is now the ONLY
   * label on screen and must agree with the picker the user just used —
   * including before `detail` has loaded and for Concierge, whose picker label
   * is not derived from `agent.toml` at all.
   * What: Optional display label; falls back to the fetched `display_name`,
   * then the raw name, exactly as before.
   */
  export let displayName = '';
  /** Leaves the config surface and returns to chat (#3894). Wired to the
   * Back/Close affordances; Esc is handled by `AgentConfigOverlay`. */
  export let onExit: () => void;

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

  /** Live OKG store bindings (#3816/#3864) — `null` until the first load
   * resolves, so the pane can distinguish "loading" from "binds nothing". */
  let okgStores: OkgStoreBinding[] | null = null;
  let okgIssues: string[] = [];
  let okgError = '';

  let saving = false;
  let saveError = '';
  let justSaved: Tab | null = null;
  let justSavedTimer: ReturnType<typeof setTimeout> | null = null;

  $: personalityDirty = personaContent !== savedPersonaContent;
  $: toolsDirty = toolsText !== savedToolsText;

  /**
   * Why (PR #3895 code-critic HIGH-1): before this, Esc/Back/Close unmounted
   * the panel and took any unsaved persona edit with them — silently, with no
   * confirmation, on a surface whose whole point is hand-writing long system
   * prompts. Confirm-before-discard (with a save option) rather than
   * auto-save-on-exit: auto-saving would persist a half-finished prompt to
   * `persona.md` and change how the agent actually behaves, which is the worse
   * failure of the two.
   * What: `configPaneDirty` mirrors the panel's dirty state for the exit paths
   * that live outside it (Esc, and App's Chat→Events switch); those raise
   * `configExitIntent`, which this component turns into the confirm prompt.
   * Test: `ChatPane.test.ts` — Esc / Back / Close / tab-switch with a dirty
   * editor.
   */
  let confirmingExit = false;
  let seenExitIntent = get(configExitIntent);

  $: configPaneDirty.set(personalityDirty || toolsDirty);
  $: if ($configExitIntent !== seenExitIntent) {
    seenExitIntent = $configExitIntent;
    confirmingExit = true;
  }
  $: dirtySections = [personalityDirty && 'Personality', toolsDirty && 'Tools']
    .filter(Boolean)
    .join(' and ');

  onDestroy(() => configPaneDirty.set(false));

  /** Exit affordances go through here, never straight to `onExit`. */
  function requestExit() {
    if (personalityDirty || toolsDirty) {
      confirmingExit = true;
      return;
    }
    onExit();
  }

  function discardAndExit() {
    confirmingExit = false;
    configPaneDirty.set(false);
    onExit();
  }

  async function saveAndExit() {
    saveError = '';
    if (personalityDirty) await savePersonality();
    if (!saveError && toolsDirty) await saveTools();
    if (saveError) return; // stay put and show the error rather than lose the edit
    confirmingExit = false;
    onExit();
  }

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
    okgStores = null;
    okgIssues = [];
    okgError = '';
    try {
      // Store resolution talks to two daemons and can be the slowest leg, but
      // it must never block (or fail) the rest of the panel — hence
      // `allSettled`-style isolation via its own catch rather than a shared
      // `Promise.all` rejection path.
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
    try {
      const s = await fetchAgentStores(name);
      okgStores = s?.stores ?? [];
      okgIssues = s?.issues ?? [];
      if (s?.config_error) okgIssues = [...okgIssues, s.config_error];
    } catch (e) {
      okgStores = [];
      okgError = `${e}`;
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

  onMount(() => () => {
    if (justSavedTimer) clearTimeout(justSavedTimer);
  });
</script>

<div class="relative flex h-full w-full flex-col overflow-hidden bg-foundry-light-surface dark:bg-foundry-surface">
  <header class="flex shrink-0 items-center gap-2 border-b border-foundry-light-border dark:border-foundry-border px-4 py-3">
    <button
      type="button"
      class="inline-flex items-center gap-1.5 rounded-md px-2 py-1 text-xs font-medium text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10 hover:text-foundry-light-primary dark:hover:text-foundry-primary"
      on:click={requestExit}
    >
      <ArrowLeft class="h-3.5 w-3.5" /> Back to chat
    </button>
    <Settings2 class="ml-2 h-4 w-4 text-foundry-light-primary dark:text-foundry-primary" />
    <h2 class="font-mono text-xs font-semibold uppercase tracking-wide text-foundry-light-text dark:text-foundry-text">
      Configure {displayName || detail?.display_name || agentName}
    </h2>
    <span class="ml-auto flex items-center gap-2">
      <kbd class="rounded border border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] uppercase text-foundry-light-muted dark:text-foundry-text/40">
        Esc
      </kbd>
      <button
        type="button"
        class="rounded-md p-1.5 text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10 hover:text-foundry-light-primary dark:hover:text-foundry-primary"
        aria-label="Close agent configuration"
        on:click={requestExit}
      >
        <X class="h-4 w-4" />
      </button>
    </span>
  </header>

  {#if isConcierge}
    <p class="mx-4 mt-3 shrink-0 rounded-md border border-foundry-amber/40 bg-foundry-amber/10 px-3 py-2 text-xs text-foundry-amber">
      Concierge is trusty-agents' fixed coordination layer — editable here, but not offered as an
      add-agent template.
    </p>
  {/if}

  <div class="flex shrink-0 flex-wrap gap-1 border-b border-foundry-light-border dark:border-foundry-border px-3 py-2" role="tablist">
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

  <!-- #3894: the body is a flex COLUMN with `min-h-0`, and each tab owns its
       own scrolling. The editor tabs (Personality/Tools) grow their textarea
       into the leftover space and scroll INSIDE it; the read-only tabs
       (OKG/Permissions/Listeners) scroll as a whole. A single shared
       `overflow-y-auto` here — what #3826 had — would put a second scrollbar
       around an already-scrolling textarea. -->
  <div class="flex min-h-0 flex-1 flex-col px-4 py-3">
    {#if loading}
      <div class="flex items-center gap-2 text-sm text-foundry-light-muted dark:text-foundry-text/60">
        <Loader2 class="h-4 w-4 animate-spin" /> Loading…
      </div>
    {:else if loadError}
      <div class="flex items-center gap-2 text-sm text-red-500 dark:text-red-400">
        <AlertCircle class="h-4 w-4" /> {loadError}
      </div>
    {:else if tab === 'personality'}
      <div class="flex min-h-0 flex-1 flex-col gap-2">
        <p class="shrink-0 text-xs text-foundry-light-muted dark:text-foundry-text/60">
          Main instructions — this agent's <code class="font-mono">persona.md</code>.
        </p>
        {#if !personaEditable}
          <p class="shrink-0 rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2 text-xs text-foundry-light-muted dark:text-foundry-text/50">
            This agent has no editable persona.md (flat-file agents don't carry a separate
            personality file).
          </p>
        {:else}
          <!-- #3894 (supersedes #3862's vh clamp): the instructions editor is
               the primary surface of this pane, so it takes EVERY remaining
               pixel of the takeover — `flex-1 min-h-0`, no `rows`, no
               `min-h-[55vh]/max-h-[70vh]` clamp (which capped it at 70% of the
               viewport and left dead space below on a tall window, and could
               overflow its container on a short one). It is the only scroll
               container in this tab; `resize-none` because a drag handle can
               only make a full-height field smaller. -->
          <textarea
            bind:value={personaContent}
            spellcheck="false"
            data-instructions-editor
            class="w-full min-h-0 flex-1 resize-none overflow-y-auto rounded-md border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-bg dark:bg-foundry-bg px-3 py-2 font-mono text-xs leading-relaxed text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
          ></textarea>
          <div class="flex shrink-0 items-center gap-2 pt-1">
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
        <p class="shrink-0 pt-2 text-xs text-foundry-light-muted dark:text-foundry-text/40">
          Event-specific instructions (per-connector context loaded on an incoming event — e.g. a
          Gmail event loading Gmail-connector instructions) are a separate runtime epic. This
          section is scaffolding until that backend exists.
        </p>
        <div class="shrink-0 rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-xs text-foundry-light-muted dark:text-foundry-text/40">
          {#each DEFINED_LISTENERS as listener (listener.id)}
            <div class="py-0.5">{listener.label}: no event instructions configured</div>
          {/each}
        </div>
      </div>
    {:else if tab === 'okg'}
      <div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
        <p class="text-xs text-foundry-light-muted dark:text-foundry-text/60">
          Knowledge trees + search indexes bound to this agent, resolved live against
          trusty-search and trusty-memory. Edit bindings in
          <code class="font-mono">agent.toml</code>'s <code class="font-mono">[[stores]]</code>.
        </p>
        {#if okgStores === null}
          <div class="flex items-center gap-2 text-xs text-foundry-light-muted dark:text-foundry-text/60">
            <Loader2 class="h-3.5 w-3.5 animate-spin" /> Resolving stores…
          </div>
        {:else if okgError}
          <p class="flex items-center gap-1.5 rounded-md border border-red-500/40 bg-red-500/10 px-3 py-2 text-[11px] text-red-500 dark:text-red-400">
            <AlertCircle class="h-3.5 w-3.5 shrink-0" /> Could not read store bindings: {okgError}
          </p>
        {:else if okgStores.length === 0}
          <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
            This agent binds no OKG store. Add a <code class="font-mono">[[stores]]</code> table to
            its <code class="font-mono">agent.toml</code> to give it a knowledge tree.
          </p>
        {/if}
        {#each okgStores ?? [] as store (store.name)}
          <div class="rounded-md border border-foundry-light-border dark:border-foundry-border px-3 py-2">
            <div class="flex items-center justify-between gap-2">
              <span class="font-mono text-xs font-semibold text-foundry-light-text dark:text-foundry-text">{store.name}</span>
              <span
                class="shrink-0 rounded-md px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide {store.connected
                  ? 'bg-foundry-teal/15 text-foundry-teal'
                  : 'bg-foundry-light-border/50 dark:bg-black/30 text-foundry-light-muted dark:text-foundry-text/50'}"
              >
                {store.connected ? 'connected' : 'not connected'}
              </span>
            </div>
            <div class="mt-1 flex flex-col gap-0.5 font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
              <span>tree: {store.tree}</span>
              <span>
                index: {store.index}{#if store.connected && store.chunk_count !== undefined}
                  &nbsp;· {store.chunk_count.toLocaleString()} chunks{/if}{#if store.connected && store.index_status}
                  &nbsp;· {store.index_status}{/if}
              </span>
              {#if store.root_path}<span>root: {store.root_path}</span>{/if}
              {#if store.palace}
                <span>
                  palace: {store.palace}
                  {#if store.palace_connected === false}
                    <span class="text-foundry-amber">— {store.palace_reason ?? 'not connected'}</span>
                  {/if}
                </span>
              {/if}
            </div>
            {#if !store.connected && store.reason}
              <p class="mt-1.5 text-[11px] text-foundry-amber">{store.reason}</p>
            {/if}
          </div>
        {/each}
        {#each okgIssues as issue (issue)}
          <p class="text-[11px] text-foundry-amber">{issue}</p>
        {/each}
      </div>
    {:else if tab === 'tools'}
      <div class="flex min-h-0 flex-1 flex-col gap-2">
        <p class="shrink-0 text-xs text-foundry-light-muted dark:text-foundry-text/60">
          MCP tool allow-list — one glob pattern per line (e.g. <code class="font-mono">gworkspace_*</code>). Empty = no restriction.
        </p>
        <!-- Same full-height treatment as the instructions editor above. -->
        <textarea
          bind:value={toolsText}
          spellcheck="false"
          placeholder="gworkspace_*&#10;memory_*"
          class="w-full min-h-0 flex-1 resize-none overflow-y-auto rounded-md border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-bg dark:bg-foundry-bg px-3 py-2 font-mono text-xs leading-relaxed text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
        ></textarea>
        <div class="flex shrink-0 items-center gap-2 pt-1">
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
      <div class="flex min-h-0 flex-1 flex-col gap-2 overflow-y-auto">
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
      <div class="flex min-h-0 flex-1 flex-col gap-3 overflow-y-auto">
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
      <p class="mt-2 shrink-0 text-xs text-red-500 dark:text-red-400">{saveError}</p>
    {/if}
  </div>

  <!-- Confirm-before-discard (code-critic HIGH-1). Rendered inside the panel
       because only this component can save; every exit path — the header's
       Back/Close, Esc, and the Chat→Events tab switch — arrives here. -->
  {#if confirmingExit}
    <div
      role="alertdialog"
      aria-modal="true"
      aria-label="Unsaved configuration changes"
      class="absolute inset-0 z-30 flex items-center justify-center bg-black/40 px-4"
    >
      <div class="w-full max-w-md rounded-lg border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface p-5 shadow-xl">
        <h3 class="mb-1 text-sm font-semibold text-foundry-light-text dark:text-foundry-text">
          Unsaved changes
        </h3>
        <p class="mb-4 text-xs leading-relaxed text-foundry-light-muted dark:text-foundry-text/60">
          Your {dirtySections} {dirtySections.includes(' and ') ? 'edits have' : 'edit has'} not been
          saved. Leaving configuration now discards {dirtySections.includes(' and ') ? 'them' : 'it'}.
        </p>
        <div class="flex flex-wrap items-center justify-end gap-2">
          <button
            type="button"
            class="rounded-md px-3 py-1.5 text-xs font-medium text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10"
            on:click={() => (confirmingExit = false)}
          >
            Keep editing
          </button>
          <button
            type="button"
            class="rounded-md border border-red-500/40 px-3 py-1.5 text-xs font-medium text-red-500 dark:text-red-400 hover:bg-red-500/10"
            on:click={discardAndExit}
          >
            Discard changes
          </button>
          <button
            type="button"
            class="inline-flex items-center gap-1.5 rounded-md bg-foundry-light-primary dark:bg-foundry-primary px-3 py-1.5 text-xs font-semibold text-white shadow-sm hover:bg-foundry-light-primary/80 dark:hover:bg-foundry-primary/80 disabled:cursor-not-allowed disabled:opacity-50"
            disabled={saving}
            on:click={saveAndExit}
          >
            <Save class="h-3.5 w-3.5" /> {saving ? 'Saving…' : 'Save and close'}
          </button>
        </div>
        {#if saveError}
          <p class="mt-3 text-xs text-red-500 dark:text-red-400">{saveError}</p>
        {/if}
      </div>
    </div>
  {/if}
</div>
