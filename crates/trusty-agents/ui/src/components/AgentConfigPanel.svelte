<script lang="ts">
  /**
   * Why (#3819/#3932, epic #3052 — Bob's concept-demo centerpiece: "this agent
   * - OKG - tool - listener is the most revolutionary feature of trusty
   * agents"): agent configuration is an IN-PANE surface scoped to whichever
   * agent is active in the chat pane, opened via `ChatHeader`'s gear.
   *
   * #3932 reshapes it to DOC-57's five sections, in the owner's order:
   * **Personality / Knowledge / Skills / Listeners / Permissions** — identity →
   * knowledge → capability → reactivity → constraint. Bob: *"Instead of tools,
   * let's show skills. Should be Personality, Knowledge (list knowledge tools
   * including MCP connections to knowledge stores), Skills (each tool should be
   * wrapped in a skill), Listeners, the Permissions."* Three of the five
   * changed materially rather than in name: OKG Stores widened into Knowledge
   * (§4), the raw-glob Tools textarea became read-only skill cards (§5), and
   * Permissions moved last and gained per-mechanism `enforced` markers (§7).
   *
   * What: This component is now the SHELL — header, tab strip, the one data
   * load, the one PATCH path, and the exit guard. Each section's body lives in
   * its own `AgentConfig<Section>.svelte` (DOC-57 §8.4). Phase 1 adds no HTTP
   * route and no config-schema change (§8.5, G-7): every pane maps onto data
   * already served by `GET /api/agents/:name`, `.../persona` and `.../stores`,
   * and a pane whose spec'd data source does not exist yet says so instead of
   * fabricating it (G-4).
   *
   * The shell keeps the editable persona buffer (rather than letting
   * `AgentConfigPersonality` own it) for two reasons that are easy to lose in
   * a decomposition: section switching unmounts the inactive body, so a
   * child-owned buffer would discard a half-written system prompt on every tab
   * click; and `configPaneDirty` — which Esc, Back, Close and the Chat→Events
   * switch all consult through `requestExitConfigPane` — needs one owner that
   * outlives the tabs (§8.4, G-6a).
   *
   * #3894: the panel is a full-pane takeover mounted by `AgentConfigOverlay`,
   * so the layout is a height-filling flex column — header + tabs `shrink-0`,
   * the active section taking the rest, with the instructions editor growing
   * into every remaining pixel (§8.4, G-6b).
   * Test: `AgentConfigPanel.test.ts` (five-section tab strip, exit affordances,
   * editor sizing invariants, edit survival across a section switch),
   * `AgentConfig*.test.ts` per section, `agentConfig.test.ts` (the derivations),
   * and `tests/config-takeover.spec.ts` (measured layout). The save round-trip
   * stays manual (`pnpm dev`, open the gear, edit + save Personality, confirm a
   * re-open shows the persisted value) per the project's convention for
   * fetch-driven Svelte views.
   */
  import { AlertCircle, ArrowLeft, Loader2, Save, Settings2, X } from 'lucide-svelte';
  import { onDestroy, onMount } from 'svelte';
  import { get } from 'svelte/store';
  import { configExitIntent, configPaneDirty } from '../stores/configPane';
  import {
    fetchAgentDetail,
    fetchAgentPersona,
    patchAgent,
    fetchAgentStores,
    matchesToolGlob,
    synthesizeSkills,
    STORE_BOUND_KNOWLEDGE_TOOLS,
    type AgentDetail,
    type OkgStoreBinding,
  } from '../lib/agentConfig';
  import AgentConfigPersonality from './AgentConfigPersonality.svelte';
  import AgentConfigKnowledge from './AgentConfigKnowledge.svelte';
  import AgentConfigSkills from './AgentConfigSkills.svelte';
  import AgentConfigListeners from './AgentConfigListeners.svelte';
  import AgentConfigPermissions from './AgentConfigPermissions.svelte';

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

  /** DOC-57 §2.1: the order is NORMATIVE, and it is not the pre-#3932 order
   * (which put Permissions before Listeners). */
  const SECTIONS = [
    ['personality', 'Personality'],
    ['knowledge', 'Knowledge'],
    ['skills', 'Skills'],
    ['listeners', 'Listeners'],
    ['permissions', 'Permissions'],
  ] as const;

  type Tab = (typeof SECTIONS)[number][0];
  let tab: Tab = 'personality';

  let loading = true;
  let loadError = '';
  let detail: AgentDetail | null = null;
  let personaContent = '';
  let personaEditable = false;
  let savedPersonaContent = '';

  /** Live OKG store bindings (#3816/#3864) — `null` until the first load
   * resolves, so the pane can distinguish "loading" from "binds nothing". */
  let okgStores: OkgStoreBinding[] | null = null;
  let okgIssues: string[] = [];
  let okgError = '';

  let saving = false;
  let saveError = '';
  let justSavedPersonality = false;
  let justSavedTimer: ReturnType<typeof setTimeout> | null = null;

  $: personalityDirty = personaContent !== savedPersonaContent;

  /**
   * DOC-57 §5.5/§8.5: Skills and the Knowledge pane's tool linkage are DERIVED
   * from `AgentDetail.tools_allow` — the only capability data this phase has.
   * Note `parse_agent_toml` reads a single file with no `extends` merge, so
   * this is what the agent DECLARES; the panes say so rather than presenting it
   * as the resolved effective set.
   */
  $: skills = synthesizeSkills(detail?.tools_allow ?? []);
  $: storeBoundTools = STORE_BOUND_KNOWLEDGE_TOOLS.filter((tool) =>
    (detail?.tools_allow ?? []).some((pattern) => matchesToolGlob(pattern, tool)),
  );

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
   * Since #3932 the Skills pane is read-only (§5.7, S-12), so Personality is
   * the only editable section and the only dirty source — but the aggregation
   * deliberately stays here in the shell, because that is the seam a second
   * editable section (Phase 3's `skills_allow`) plugs into.
   * Test: `ChatPane.test.ts` — Esc / Back / Close / tab-switch with a dirty
   * editor.
   */
  let confirmingExit = false;
  let seenExitIntent = get(configExitIntent);

  $: configPaneDirty.set(personalityDirty);
  $: if ($configExitIntent !== seenExitIntent) {
    seenExitIntent = $configExitIntent;
    confirmingExit = true;
  }

  onDestroy(() => configPaneDirty.set(false));

  /** Exit affordances go through here, never straight to `onExit`. */
  function requestExit() {
    if (personalityDirty) {
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
    if (saveError) return; // stay put and show the error rather than lose the edit
    confirmingExit = false;
    onExit();
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
      // `Promise.all` rejection path. DOC-57 §8.3's G-5 generalises this:
      // one degraded backend darkens one pane, never the panel.
      const [d, p] = await Promise.all([fetchAgentDetail(name), fetchAgentPersona(name)]);
      detail = d;
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

  async function savePersonality() {
    saving = true;
    saveError = '';
    try {
      detail = await patchAgent(agentName, { personality: personaContent });
      savedPersonaContent = personaContent;
      justSavedPersonality = true;
      if (justSavedTimer) clearTimeout(justSavedTimer);
      justSavedTimer = setTimeout(() => {
        justSavedPersonality = false;
      }, 2000);
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

  <!-- Switching sections keeps every edit: the persona buffer lives in this
       shell, not in the unmounted body, so no exit guard is needed on the tab
       strip itself (§8.4, G-6a). -->
  <div class="flex shrink-0 flex-wrap gap-1 border-b border-foundry-light-border dark:border-foundry-border px-3 py-2" role="tablist">
    {#each SECTIONS as [id, label] (id)}
      <button
        type="button"
        role="tab"
        aria-selected={tab === id}
        class="rounded px-2.5 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide transition-colors {tab === id
          ? 'bg-foundry-light-primary/20 dark:bg-foundry-primary/20 text-foundry-light-primary dark:text-foundry-primary'
          : 'text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
        on:click={() => (tab = id)}
      >
        {label}
      </button>
    {/each}
  </div>

  <!-- #3894: the body is a flex COLUMN with `min-h-0`, and each section owns
       its own scrolling. Personality grows its textarea into the leftover space
       and scrolls INSIDE it; the read-only sections scroll as a whole. A single
       shared `overflow-y-auto` here — what #3826 had — would put a second
       scrollbar around an already-scrolling textarea. -->
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
      <AgentConfigPersonality
        {personaEditable}
        bind:content={personaContent}
        dirty={personalityDirty}
        {saving}
        justSaved={justSavedPersonality}
        onSave={savePersonality}
      />
    {:else if tab === 'knowledge'}
      <AgentConfigKnowledge
        stores={okgStores}
        issues={okgIssues}
        error={okgError}
        {storeBoundTools}
      />
    {:else if tab === 'skills'}
      <AgentConfigSkills {skills} />
    {:else if tab === 'listeners'}
      <AgentConfigListeners />
    {:else if tab === 'permissions'}
      <AgentConfigPermissions toolsAllow={detail?.tools_allow ?? []} scopes={detail?.scopes ?? []} />
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
          Your Personality edit has not been saved. Leaving configuration now discards it.
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
