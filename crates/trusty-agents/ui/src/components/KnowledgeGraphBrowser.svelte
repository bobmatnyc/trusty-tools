<script lang="ts">
  /** Read-only browser for the selected assistant's OKG knowledge graph —
   * triples and definitions out of its `okg/` tree, never a memory palace
   * (#7430). ChatPane positions this over its mounted chat, matching
   * preferences. Every request is scoped to the current assistant generation;
   * triple selection requests additionally use a sequence so older results
   * cannot replace a newer selection. Disconnected envelopes remain distinct
   * from an empty connected graph. Tests cover takeover and asynchronous races.
   */
  import { AlertCircle, ArrowLeft, Loader2, Network, X } from 'lucide-svelte';
  import { onMount, onDestroy } from 'svelte';
  import {
    fetchKgAll,
    fetchKgCount,
    fetchKgSubject,
    fetchKgSubjects,
    type KgDefinition,
    type KgSubjectCount,
    type KgTriple,
  } from '../lib/kg';

  /** The agent whose OKG tree this browses. Never Concierge — `ChatHeader`
   * hides the opening control when `activeAgentId === null` (owner decision:
   * Concierge has no `agent.toml`/`[[stores]]` binding). */
  export let agentName: string;
  export let onClose: () => void;

  const PAGE_SIZE = 50;

  type Mode = 'all' | 'subject';

  /** `null` while the first request (which resolves the tree) is in flight —
   * distinct from `false`, which means "resolved: not connected". */
  let connected: boolean | null = null;
  /** The binding's opaque label for the tree (`okg://izzie`, `izzie/okg`) —
   * never a filesystem path, so it is shown as a name and never as a location
   * (#7430). */
  let tree: string | null = null;
  let reason = '';
  let configError = '';
  /** 404 on the first request — an unknown agent, e.g. a stale roster
   * selection under an already-open panel. Distinct from `loadError` (any
   * other throw: 400, network failure, unreadable body). */
  let notFound = false;
  let loadError = '';

  let subjects: KgSubjectCount[] = [];
  let subjectFilter = '';
  let subjectSort: 'name' | 'count' = 'name';

  let mode: Mode = 'all';
  let selectedSubject = '';
  let triples: KgTriple[] = [];
  /** The definitions that came with the current page of triples — the "what is
   * this subject" half of the graph (#7430). */
  let definitions: KgDefinition[] = [];
  let triplesLoading = false;
  let triplesError = '';

  let activeCount: number | null = null;
  let offset = 0;

  let container: HTMLElement | null = null;
  let generation = 0;
  let triplesRequest = 0;

  $: visibleSubjects = (() => {
    const f = subjectFilter.trim().toLowerCase();
    let out = f ? subjects.filter((s) => s.subject.toLowerCase().includes(f)) : subjects.slice();
    if (subjectSort === 'count') {
      out = out
        .slice()
        .sort((a, b) => b.count - a.count || a.subject.localeCompare(b.subject));
    } else {
      out = out.slice().sort((a, b) => a.subject.localeCompare(b.subject));
    }
    return out;
  })();

  /**
   * Why (#4290 code-review finding, HIGH): every KG route can independently
   * flip to `connected: false` (still HTTP 200, `data: []`/`{active: 0}`) if
   * the OKG tree goes away AFTER bootstrap already resolved
   * `connected: true` — the directory is moved or the binding is rewritten, or
   * the user pages through "all"/clicks a subject meanwhile. Assigning
   * `env.data` unconditionally in that case renders "0 active triples" / "No
   * triples.", which is indistinguishable from a connected-but-empty tree —
   * exactly the failure the owner's contract forbids. So `loadAll`,
   * `loadCount`, and `loadSubject` all check `env.connected` first and, when
   * false, route into the SAME disconnected state `bootstrap()` shows
   * (`reason`/`config_error`), rather than assigning empty data.
   * What: Shared by the three post-bootstrap fetches so they can't drift on
   * how they handle a degraded envelope.
   * Test: `KnowledgeGraphBrowser.test.ts`.
   */
  function applyDisconnected(env: { reason?: string; config_error?: string }) {
    connected = false;
    reason = env.reason ?? '';
    configError = env.config_error ?? '';
  }

  /**
   * Why: `/kg/subjects` is the cheapest route that also carries the
   * tree/`connected`/`reason`/`config_error` state every other route would
   * report identically (all four resolve the SAME agent → OKG tree).
   * Resolving it once here, rather than duplicating the check in `loadAll`
   * and `loadCount`, is what lets those two stay simple "fetch the data"
   * calls.
   * What: Sets the top-level connection state; on `connected`, kicks off the
   * triple table + count badge in parallel — each with its OWN try/catch so
   * one degraded route darkens only its own section (AgentConfigPanel's
   * per-surface isolation precedent).
   */
  async function bootstrap(name: string) {
    const token = ++generation;
    triplesRequest++;
    subjectFilter = '';
    triplesError = '';
    triplesLoading = false;
    notFound = false;
    loadError = '';
    connected = null;
    tree = null;
    reason = '';
    configError = '';
    subjects = [];
    mode = 'all';
    selectedSubject = '';
    offset = 0;
    triples = [];
    definitions = [];
    activeCount = null;
    try {
      const env = await fetchKgSubjects(name, 200);
      if (token !== generation) return;
      if (env === null) {
        notFound = true;
        return;
      }
      tree = env.tree;
      connected = env.connected;
      reason = env.reason ?? '';
      configError = env.config_error ?? '';
      subjects = env.data ?? [];
      if (!connected) return;
      await Promise.all([loadAll(name), loadCount(name)]);
    } catch (e) {
      if (token === generation) loadError = `${e}`;
    }
  }

  async function loadAll(name: string) {
    const token = generation, request = ++triplesRequest;
    triplesLoading = true;
    triplesError = '';
    try {
      const env = await fetchKgAll(name, PAGE_SIZE, offset);
      if (token !== generation || request !== triplesRequest) return;
      if (env === null) {
        notFound = true;
        return;
      }
      // #4290: a mid-session degradation must render the same disconnected
      // state as a degraded bootstrap, never an empty triples list.
      if (!env.connected) {
        applyDisconnected(env);
        return;
      }
      triples = env.data ?? [];
      definitions = env.definitions ?? [];
    } catch (e) {
      if (token === generation && request === triplesRequest) { triplesError = `${e}`; triples = []; definitions = []; }
    } finally {
      if (token === generation && request === triplesRequest) triplesLoading = false;
    }
  }

  async function loadCount(name: string) {
    const token = generation;
    try {
      const env = await fetchKgCount(name);
      if (token !== generation) return;
      if (env === null) {
        activeCount = null;
        return;
      }
      // #4290: same disconnected-state routing as loadAll — a degraded count
      // must not render as "0 active triples".
      if (!env.connected) {
        applyDisconnected(env);
        activeCount = null;
        return;
      }
      activeCount = env.data?.active ?? null;
    } catch {
      if (token === generation) activeCount = null;
    }
  }

  async function loadSubject(subject: string) {
    const token = generation, request = ++triplesRequest;
    selectedSubject = subject;
    mode = 'subject';
    triplesLoading = true;
    triplesError = '';
    try {
      const env = await fetchKgSubject(agentName, subject);
      if (token !== generation || request !== triplesRequest) return;
      if (env === null) {
        notFound = true;
        return;
      }
      // #4290: same disconnected-state routing as loadAll.
      if (!env.connected) {
        applyDisconnected(env);
        return;
      }
      triples = env.data ?? [];
      definitions = env.definitions ?? [];
    } catch (e) {
      if (token === generation && request === triplesRequest) { triplesError = `${e}`; triples = []; definitions = []; }
    } finally {
      if (token === generation && request === triplesRequest) triplesLoading = false;
    }
  }

  function showAll() {
    mode = 'all';
    selectedSubject = '';
    offset = 0;
    loadAll(agentName);
  }

  function prevPage() {
    if (offset === 0) return;
    offset = Math.max(0, offset - PAGE_SIZE);
    loadAll(agentName);
  }

  function nextPage() {
    if (triples.length < PAGE_SIZE) return;
    offset += PAGE_SIZE;
    loadAll(agentName);
  }

  /** The definitions to show above the triple table: the selected subject's
   * own definition in subject mode, every definition on the page otherwise. */
  $: visibleDefinitions =
    mode === 'subject' ? definitions.filter((d) => d.subject === selectedSubject) : definitions;

  // Reactive rather than onMount: if the roster switch happens while the
  // panel is open (Concierge aside, which unmounts this component entirely —
  // see ChatHeader), the browser must re-resolve for the newly active agent
  // rather than keep showing the previous one's graph.
  $: bootstrap(agentName);

  function onKeydown(event: KeyboardEvent) {
    if (event.key !== 'Escape') return;
    event.preventDefault();
    onClose();
  }

  // Move focus into the main-pane browser, mirroring AgentConfigOverlay —
  // otherwise a keyboard user is left focused on the (now covered) button
  // that opened it.
  onMount(() => { container?.focus(); });
  onDestroy(() => { generation++; triplesRequest++; });
</script>

<svelte:window on:keydown={onKeydown} />

<section
    bind:this={container}
    aria-label="Knowledge Graph browser"
    tabindex="-1"
    data-knowledge-takeover
    class="kg-takeover absolute inset-0 z-20 flex min-h-0 min-w-0 flex-col bg-foundry-light-surface dark:bg-foundry-surface focus:outline-none"
  >
    <header class="flex shrink-0 items-center gap-2 border-b border-foundry-light-border dark:border-foundry-border px-4 py-3">
      <button type="button" class="flex items-center gap-1 rounded-md px-2 py-1 text-xs text-foundry-light-muted dark:text-foundry-text/60" on:click={onClose}><ArrowLeft class="h-4 w-4" />Back to chat</button>
      <Network class="h-4 w-4 text-foundry-light-primary dark:text-foundry-primary" />
      <h2 class="font-mono text-xs font-semibold uppercase tracking-wide text-foundry-light-text dark:text-foundry-text">
        Knowledge Graph
      </h2>
      {#if activeCount !== null}
        <span class="rounded-md bg-foundry-light-border/50 dark:bg-black/30 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">
          {activeCount.toLocaleString()} active triples
        </span>
      {/if}

      <button type="button" aria-label="Close Knowledge Graph" title="Close Knowledge Graph" class="ml-auto rounded-md p-1.5 text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10" on:click={onClose}><X class="h-4 w-4" /></button>
    </header>

    <div class="flex min-h-0 flex-1 flex-col overflow-y-auto px-4 py-3">
      <!-- State: fetch-helper error path (404 — unknown agent). -->
      {#if notFound}
        <div class="flex items-center gap-2 text-sm text-red-500 dark:text-red-400">
          <AlertCircle class="h-4 w-4 shrink-0" /> Agent "{agentName}" not found.
        </div>

      <!-- State: fetch-helper error path (any other throw — 400/network/unreadable body). -->
      {:else if loadError}
        <div class="flex items-center gap-2 text-sm text-red-500 dark:text-red-400">
          <AlertCircle class="h-4 w-4 shrink-0" /> Could not load the knowledge graph: {loadError}
        </div>

      <!-- State: loading (first request in flight). -->
      {:else if connected === null}
        <div class="flex items-center gap-2 text-sm text-foundry-light-muted dark:text-foundry-text/60">
          <Loader2 class="h-4 w-4 animate-spin" /> Loading knowledge graph…
        </div>

      <!-- State: connected:false — reason rendered directly (never a generic
           failure or an empty list), plus config_error alongside it when the
           agent's own agent.toml failed to parse. -->
      {:else if !connected}
        <div class="flex flex-col gap-2">
          <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-xs text-foundry-amber">
            {reason || "This agent's Knowledge Graph is not reachable right now."}
          </p>
          {#if configError}
            <p class="flex items-center gap-1.5 rounded-md border border-red-500/40 bg-red-500/10 px-3 py-2 text-[11px] text-red-500 dark:text-red-400">
              <AlertCircle class="h-3.5 w-3.5 shrink-0" /> Agent configuration could not be parsed: {configError}
            </p>
          {/if}
        </div>

      <!-- State: connected:true, genuinely empty graph (readable OKG tree, no
           entities) — distinct copy from the not-connected case above. -->
      {:else if subjects.length === 0}
        <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-xs text-foundry-light-muted dark:text-foundry-text/40">
          {tree ? `The OKG tree "${tree}"` : 'This agent'} is readable, but holds nothing yet.
        </p>

      <!-- State: connected:true with data — the explorer. -->
      {:else}
        <div class="kg-columns grid min-h-0 flex-1 grid-cols-[minmax(180px,30%)_minmax(0,1fr)] gap-3">
          <aside class="flex min-h-0 flex-col rounded-md border border-foundry-light-border dark:border-foundry-border">
            <div class="flex items-center justify-between gap-2 border-b border-foundry-light-border dark:border-foundry-border px-3 py-2 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">
              <span>Subjects ({visibleSubjects.length}/{subjects.length})</span>
            </div>
            <div class="flex flex-col gap-1.5 border-b border-foundry-light-border dark:border-foundry-border px-2 py-2">
              <input
                type="search"
                placeholder="Filter subjects…"
                bind:value={subjectFilter}
                class="w-full rounded border border-foundry-light-border dark:border-foundry-border bg-transparent px-2 py-1 text-xs text-foundry-light-text dark:text-foundry-text placeholder:text-foundry-light-muted dark:placeholder:text-foundry-text/40"
              />
              <select
                bind:value={subjectSort}
                class="w-full rounded border border-foundry-light-border dark:border-foundry-border bg-transparent px-2 py-1 text-[11px] text-foundry-light-text dark:text-foundry-text"
              >
                <option value="name">Sort: A→Z</option>
                <option value="count">Sort: count</option>
              </select>
            </div>
            <div class="min-h-0 flex-1 overflow-y-auto py-1">
              {#if visibleSubjects.length === 0}
                <p class="px-3 py-3 text-center text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
                  No matches.
                </p>
              {:else}
                <ul>
                  {#each visibleSubjects as s (s.subject)}
                    <li>
                      <button
                        type="button"
                        class="flex w-full items-center justify-between gap-2 px-3 py-1.5 text-left font-mono text-xs transition-colors {selectedSubject ===
                        s.subject
                          ? 'bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'
                          : 'text-foundry-light-text/80 dark:text-foundry-text/80 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10'}"
                        on:click={() => loadSubject(s.subject)}
                      >
                        <span class="truncate">{s.subject}</span>
                        <span class="shrink-0 rounded-full bg-foundry-light-border/50 dark:bg-black/30 px-1.5 py-0.5 text-[10px] text-foundry-light-muted dark:text-foundry-text/50">
                          {s.count}
                        </span>
                      </button>
                    </li>
                  {/each}
                </ul>
              {/if}
            </div>
          </aside>

          <section class="flex min-h-0 flex-col rounded-md border border-foundry-light-border dark:border-foundry-border">
            <div class="flex items-center gap-2 border-b border-foundry-light-border dark:border-foundry-border px-3 py-2">
              {#if mode === 'subject'}
                <button
                  type="button"
                  class="inline-flex items-center gap-1 font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-primary dark:text-foundry-primary hover:underline"
                  on:click={showAll}
                >
                  <ArrowLeft class="h-3 w-3" /> All triples
                </button>
                <span class="font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
                  · subject: <strong class="text-foundry-light-text dark:text-foundry-text">{selectedSubject}</strong>
                </span>
              {:else}
                <span class="font-mono text-[10px] font-semibold uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">
                  All triples
                </span>
              {/if}
              <span class="ml-auto text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
                {#if triplesLoading}loading…{:else}{triples.length} rows{/if}
              </span>
            </div>

            {#if triplesError}
              <p class="flex items-center gap-1.5 px-3 py-2 text-[11px] text-red-500 dark:text-red-400">
                <AlertCircle class="h-3.5 w-3.5 shrink-0" /> {triplesError}
              </p>
            {/if}

            <!-- The definitions half of the graph (#7430): what each subject on
                 this page IS, beside the edges it has. -->
            {#if visibleDefinitions.length > 0}
              <ul data-kg-definitions class="shrink-0 border-b border-foundry-light-border dark:border-foundry-border px-3 py-2 text-[11px]">
                {#each visibleDefinitions as d (d.collection + '/' + d.slug)}
                  <li class="py-0.5 text-foundry-light-muted dark:text-foundry-text/60">
                    <strong class="font-mono text-foundry-light-text dark:text-foundry-text">{d.subject}</strong>
                    {#if d.type}<span class="ml-1 rounded bg-foundry-light-border/50 dark:bg-black/30 px-1 py-0.5 font-mono text-[10px]">{d.type}</span>{/if}
                    {#if d.summary}<span class="ml-1">— {d.summary}</span>{/if}
                  </li>
                {/each}
              </ul>
            {/if}

            <div class="min-h-0 flex-1 overflow-y-auto">
              {#if triples.length === 0 && !triplesLoading}
                <p class="px-3 py-3 text-center text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
                  No triples.
                </p>
              {:else}
                <table class="w-full border-collapse text-xs">
                  <thead>
                    <tr class="border-b border-foundry-light-border dark:border-foundry-border">
                      <th class="px-2 py-1.5 text-left font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">Subject</th>
                      <th class="px-2 py-1.5 text-left font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">Predicate</th>
                      <th class="px-2 py-1.5 text-left font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">Object</th>
                      <th class="px-2 py-1.5 text-left font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">Source file</th>
                    </tr>
                  </thead>
                  <tbody>
                    {#each triples as t, i (t.subject + '|' + t.predicate + '|' + t.object + '|' + i)}
                      <tr class="border-b border-foundry-light-border/60 dark:border-foundry-border/60 last:border-0">
                        <td class="px-2 py-1.5 font-mono text-[11px] text-foundry-light-text dark:text-foundry-text">{t.subject}</td>
                        <td class="px-2 py-1.5 font-mono text-[11px] text-foundry-light-text dark:text-foundry-text">{t.predicate}</td>
                        <td class="px-2 py-1.5 text-foundry-light-text/90 dark:text-foundry-text/90">{t.object}</td>
                        <td class="px-2 py-1.5 font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/50">{t.provenance ?? '—'}</td>
                      </tr>
                    {/each}
                  </tbody>
                </table>
              {/if}
            </div>

            {#if mode === 'all'}
              <div class="flex shrink-0 items-center justify-between border-t border-foundry-light-border dark:border-foundry-border px-3 py-2">
                <button
                  type="button"
                  class="rounded-md border border-foundry-light-border dark:border-foundry-border px-2 py-1 text-[11px] disabled:cursor-not-allowed disabled:opacity-40"
                  disabled={offset === 0}
                  on:click={prevPage}
                >
                  ← Prev
                </button>
                <span class="text-[11px] text-foundry-light-muted dark:text-foundry-text/50">
                  offset {offset.toLocaleString()} – {(offset + triples.length).toLocaleString()}
                </span>
                <button
                  type="button"
                  class="rounded-md border border-foundry-light-border dark:border-foundry-border px-2 py-1 text-[11px] disabled:cursor-not-allowed disabled:opacity-40"
                  disabled={triples.length < PAGE_SIZE}
                  on:click={nextPage}
                >
                  Next →
                </button>
              </div>
            {/if}
          </section>
        </div>
      {/if}
    </div>
  </section>

<style>
  .kg-takeover { container-type:inline-size; }
  @container (max-width:600px) {
    .kg-columns { grid-template-columns:minmax(0,1fr); }
    .kg-columns > aside { max-height:220px; }
  }
</style>
