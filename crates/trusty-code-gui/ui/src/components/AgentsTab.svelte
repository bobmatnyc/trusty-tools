<script lang="ts">
  // Why: issue #3449 — Bob: "add a 'Skills' tab along with 'Agents', and
  // the user should have the ability to add/remove both." This REPLACES the
  // #3153 shell-rebuild stub (which deferred everything to "a follow-up
  // slice, per-agent todo checklists + model badge, #3255/DOC-39 §5.4" —
  // that per-SESSION live-roster feature is UNCHANGED and still not built;
  // this tab is a different feature entirely: the daemon's disk/embedded
  // AGENT CATALOG, `GET`/`POST /agents`, `DELETE /agents/{name}`
  // (`crate::serve::rest::agent_catalog`), independent of any session.
  //
  // Polls `GET /agents` every `POLL_MS`, the same `$effect`/
  // `AbortController`/`setInterval` shape every sibling tab establishes
  // (`SearchTab.svelte`, `WorkstreamActivity.svelte`). Embedded entries
  // (`tier: "embedded"`) are read-only — badged, no remove button, matching
  // `crate::agents::protocol`'s 403-on-embedded-delete contract. Disk-tier
  // entries (`tier: "project"`/`"user"`) get a two-step inline confirm on
  // remove — the SAME pattern `WorkstreamSwitcher.svelte`'s close-confirm
  // establishes (no `window.confirm()`), including its post-#3392
  // `resetRowState()` discipline: every armed row control is cleared on
  // every list refresh and on add-panel toggle, so an armed confirm can
  // never survive a stale-row reorder.
  //
  // Add flow: a name field + a Markdown+frontmatter content editor
  // (`<textarea>`), with an optional file picker that reads a chosen
  // `.md` file's text into the same textarea (`FileReader`, progressive
  // convenience — nothing here requires it). `createAgent` surfaces the
  // daemon's exact error message (403 embedded collision, 409 already
  // exists, 400 invalid name) inline rather than a generic failure.
  //
  // Test: `AgentsTab.test.ts`.
  import { apiBase } from '../lib/api-config';
  import {
    createAgent,
    deleteAgent,
    fetchAgentRoster,
    type AgentCatalogEntry,
  } from '../lib/agent-roster';

  const POLL_MS = 5000;

  type Phase = 'connecting' | 'daemon-unreachable' | 'ready';

  let phase = $state<Phase>('connecting');
  let agents = $state<AgentCatalogEntry[]>([]);
  let error = $state<string | null>(null);

  let addOpen = $state(false);
  let addName = $state('');
  let addContent = $state('');
  let addBusy = $state(false);
  let addError = $state<string | null>(null);

  let removeConfirmName = $state<string | null>(null);
  let removeBusy = $state(false);
  let removeError = $state<string | null>(null);

  let pollController: AbortController | null = null;

  function msg(e: unknown): string {
    return e instanceof Error ? e.message : String(e);
  }

  /** Clears every armed row control — mirrors
   * `WorkstreamSwitcher.svelte::resetRowState`'s post-#3392 discipline. */
  function resetRowState() {
    removeConfirmName = null;
    removeError = null;
  }

  async function refresh(signal: AbortSignal) {
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        error = msg(e);
      }
      return;
    }
    if (signal.aborted) return;

    try {
      const roster = await fetchAgentRoster(base, signal);
      if (signal.aborted) return;
      agents = roster;
      phase = 'ready';
      error = null;
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        error = msg(e);
      }
    }
  }

  $effect(() => {
    const controller = new AbortController();
    pollController = controller;
    void refresh(controller.signal);
    const timer = setInterval(() => void refresh(controller.signal), POLL_MS);
    return () => {
      controller.abort();
      if (pollController === controller) pollController = null;
      clearInterval(timer);
    };
  });

  function toggleAdd() {
    addOpen = !addOpen;
    addError = null;
    if (!addOpen) {
      addName = '';
      addContent = '';
    }
    resetRowState();
  }

  async function onFilePicked(e: Event) {
    const input = e.target as HTMLInputElement;
    const file = input.files?.[0];
    if (!file) return;
    addContent = await file.text();
    if (!addName) {
      addName = file.name.replace(/\.md$/i, '');
    }
  }

  async function submitAdd() {
    if (addBusy || !addName.trim() || !addContent.trim()) return;
    addBusy = true;
    addError = null;
    try {
      const base = await apiBase();
      await createAgent(base, addName.trim(), addContent);
      addOpen = false;
      addName = '';
      addContent = '';
      if (pollController) await refresh(pollController.signal);
    } catch (e) {
      addError = msg(e);
    } finally {
      addBusy = false;
    }
  }

  async function doRemove(name: string) {
    if (removeBusy) return;
    removeBusy = true;
    try {
      const base = await apiBase();
      await deleteAgent(base, name);
      removeConfirmName = null;
      if (pollController) await refresh(pollController.signal);
    } catch (e) {
      removeError = msg(e);
    } finally {
      removeBusy = false;
    }
  }

  function tierLabel(tier: AgentCatalogEntry['tier']): string {
    if (tier === 'embedded') return 'embedded · read-only';
    if (tier === 'project') return 'project';
    if (tier === 'broken') return 'broken · dispatch will fail';
    return 'user';
  }
</script>

<section class="rounded border-1.5 border-trusty-border bg-trusty-card">
  <div
    class="flex items-center justify-between border-b border-trusty-border bg-trusty-raised px-4 py-2.5"
  >
    <h2 class="font-display text-xs font-bold uppercase tracking-wide text-trusty-text">agents</h2>
    <button
      type="button"
      onclick={toggleAdd}
      class="rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card px-2 py-0.5 font-mono text-[10px] uppercase tracking-wide text-trusty-text-secondary hover:border-trusty-primary hover:text-trusty-primary"
    >
      {addOpen ? 'cancel' : '+ add agent'}
    </button>
  </div>

  <div class="p-4">
    {#if addOpen}
      <div class="mb-4 rounded border-1.5 border-trusty-border bg-trusty-raised p-3">
        <label
          class="font-mono text-[10px] font-semibold uppercase tracking-wide text-trusty-text-muted"
          for="agent-add-name"
        >
          name (lowercase, digits, hyphens)
        </label>
        <input
          id="agent-add-name"
          bind:value={addName}
          placeholder="my-custom-agent"
          class="mt-1 w-full rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card p-1.5 font-mono text-xs text-trusty-text"
        />

        <label
          class="mt-2 block font-mono text-[10px] font-semibold uppercase tracking-wide text-trusty-text-muted"
          for="agent-add-content"
        >
          content (Markdown + frontmatter)
        </label>
        <textarea
          id="agent-add-content"
          bind:value={addContent}
          rows="8"
          placeholder={'---\nname: my-custom-agent\ndescription: ...\n---\n\nSystem prompt body.'}
          class="mt-1 w-full rounded-sm border-1.5 border-trusty-border-strong bg-trusty-card p-1.5 font-mono text-xs text-trusty-text"
        ></textarea>

        <div class="mt-2 flex items-center gap-2">
          <input
            type="file"
            accept=".md,text/markdown,text/plain"
            onchange={onFilePicked}
            class="font-mono text-[10px] text-trusty-text-muted"
          />
        </div>

        {#if addError}
          <p class="mt-2 text-xs text-status-error">{addError}</p>
        {/if}

        <button
          type="button"
          disabled={addBusy || !addName.trim() || !addContent.trim()}
          onclick={submitAdd}
          class="mt-2 rounded-sm border-1.5 border-trusty-primary bg-trusty-primary px-2.5 py-1 font-mono text-[11px] uppercase tracking-wide text-trusty-surface disabled:cursor-not-allowed disabled:opacity-50"
        >
          {addBusy ? 'creating…' : 'create'}
        </button>
      </div>
    {/if}

    {#if phase === 'connecting'}
      <p class="text-xs text-trusty-text-muted">connecting&hellip;</p>
    {:else if phase === 'daemon-unreachable'}
      <p class="flex items-center gap-1.5 text-xs text-status-error">
        <span class="h-1.5 w-1.5 rounded-full bg-status-error"></span>
        daemon unreachable{error ? ` — ${error}` : ''}
      </p>
    {:else if agents.length === 0}
      <p class="text-xs text-trusty-text-muted">no agents found</p>
    {:else}
      <ul class="flex flex-col gap-1.5">
        {#each agents as agent (agent.name)}
          <li
            class="flex items-center justify-between gap-2 rounded-sm border-1.5 border-trusty-border px-2.5 py-1.5"
          >
            <div class="min-w-0 flex-1">
              <div class="flex items-center gap-2">
                <span class="truncate font-mono text-xs text-trusty-text">{agent.name}</span>
                <span
                  class="shrink-0 rounded-sm border border-trusty-border-strong px-1 font-mono text-[9px] uppercase tracking-wide text-trusty-text-muted"
                >
                  {tierLabel(agent.tier)}
                </span>
              </div>
              {#if agent.description}
                <p class="truncate text-[11px] text-trusty-text-secondary">{agent.description}</p>
              {/if}
            </div>

            {#if agent.tier !== 'embedded'}
              {#if removeConfirmName === agent.name}
                <div class="flex shrink-0 items-center gap-1.5">
                  <button
                    type="button"
                    class="font-mono text-[10px] uppercase tracking-wide text-status-error"
                    disabled={removeBusy}
                    onclick={() => doRemove(agent.name)}
                  >
                    confirm
                  </button>
                  <button
                    type="button"
                    class="font-mono text-[10px] uppercase tracking-wide text-trusty-text-muted"
                    disabled={removeBusy}
                    onclick={() => (removeConfirmName = null)}
                  >
                    never mind
                  </button>
                </div>
              {:else}
                <button
                  type="button"
                  class="shrink-0 px-1 font-mono text-[11px] text-trusty-text-muted hover:text-status-error"
                  aria-label={`remove ${agent.name}`}
                  onclick={() => (removeConfirmName = agent.name)}
                >
                  ×
                </button>
              {/if}
            {/if}
          </li>
        {/each}
      </ul>
      {#if removeError}
        <p class="mt-2 text-xs text-status-error">{removeError}</p>
      {/if}
    {/if}
  </div>
</section>
