<script lang="ts">
  /**
   * Why (#4290): the owner asked for a dedicated Knowledge Graph browser
   * reachable from the chat pane — a slide-over opened by its own button in
   * `ChatHeader`, not a new config-pane tab and not folded into the existing
   * "Knowledge" tab (`AgentConfigKnowledge.svelte`, which only shows store
   * BINDINGS, never the graph's contents). Ports the interaction pattern of
   * `crates/trusty-memory/ui/src/lib/views/KG.svelte` (subject list with
   * count badges, drill-down to a subject's triples, paginated "all" mode,
   * substring filter, sort by name/count) into this app's Foundry/Tailwind
   * idiom — that component is NOT imported (the two Svelte apps share no
   * package) and its palace dropdown is dropped: this browser is scoped to
   * the active agent's one bound palace (owner decision — single palace, no
   * picker).
   *
   * Read-only v1 (owner decision): no assert/delete UI. The backend
   * (`agent_kg.rs`) deliberately proxies only the four GET routes.
   *
   * Label is always "Knowledge Graph" in user-facing text — never "OKG"
   * (verbatim owner requirement), even though the on-disk concept is the
   * same one `AgentConfigKnowledge.svelte` calls "OKG store" today.
   *
   * What: A fixed slide-over (backdrop + right-anchored panel) that resolves
   * palace-binding state ONCE via `/kg/subjects` (the cheapest of the four
   * routes to establish "is this agent's palace reachable at all"), then —
   * only once `connected` — loads `/kg/all` and `/kg/count` alongside it.
   * Mirrors `AgentConfigPanel`'s per-surface fetch isolation: `/kg/count`'s
   * own failure never blanks the triple table, and vice versa. Six explicit
   * states, matching the ticket's checklist: (1) loading, (2) connected with
   * data, (3) connected with a genuinely empty graph, (4) `connected: false`
   * with `reason` rendered directly, (5) `config_error` surfaced alongside
   * (4) — the backend only ever sets it together with a "no palace bound"
   * reason, never alone — and (6) the fetch-helper error path, split into a
   * 404 ("agent not found", a stale roster selection) and any other
   * network/HTTP throw.
   * Test: manual — open the panel for an agent with a bound palace with
   * data, one with a palace but no triples, one with no `[[stores]].palace`,
   * and with the trusty-memory daemon stopped; confirm each renders its own
   * copy rather than a shared spinner or empty list.
   */
  import { AlertCircle, ArrowLeft, Loader2, Network, X } from 'lucide-svelte';
  import { onMount } from 'svelte';
  import {
    fetchKgAll,
    fetchKgCount,
    fetchKgSubject,
    fetchKgSubjects,
    type KgSubjectCount,
    type KgTriple,
  } from '../lib/kg';

  /** The agent whose bound palace this browses. Never Concierge — `ChatHeader`
   * hides the opening control when `activeAgentId === null` (owner decision:
   * Concierge has no `agent.toml`/`[[stores]]` binding). */
  export let agentName: string;
  export let onClose: () => void;

  const PAGE_SIZE = 50;

  type Mode = 'all' | 'subject';

  /** `null` while the first request (which resolves palace-binding state) is
   * in flight — distinct from `false`, which means "resolved: not connected". */
  let connected: boolean | null = null;
  let palace: string | null = null;
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
  let triplesLoading = false;
  let triplesError = '';

  let activeCount: number | null = null;
  let offset = 0;

  let container: HTMLElement | null = null;

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
   * Why: `/kg/subjects` is the cheapest route that also carries the
   * palace/`connected`/`reason`/`config_error` state every other route would
   * report identically (all four resolve the SAME agent → palace binding).
   * Resolving it once here, rather than duplicating the check in `loadAll`
   * and `loadCount`, is what lets those two stay simple "fetch the data"
   * calls.
   * What: Sets the top-level connection state; on `connected`, kicks off the
   * triple table + count badge in parallel — each with its OWN try/catch so
   * one degraded route darkens only its own section (AgentConfigPanel's
   * per-surface isolation precedent).
   */
  async function bootstrap(name: string) {
    notFound = false;
    loadError = '';
    connected = null;
    palace = null;
    reason = '';
    configError = '';
    subjects = [];
    mode = 'all';
    selectedSubject = '';
    offset = 0;
    triples = [];
    activeCount = null;
    try {
      const env = await fetchKgSubjects(name, 200);
      if (env === null) {
        notFound = true;
        return;
      }
      palace = env.palace;
      connected = env.connected;
      reason = env.reason ?? '';
      configError = env.config_error ?? '';
      subjects = env.data ?? [];
      if (!connected) return;
      await Promise.all([loadAll(name), loadCount(name)]);
    } catch (e) {
      loadError = `${e}`;
    }
  }

  async function loadAll(name: string) {
    triplesLoading = true;
    triplesError = '';
    try {
      const env = await fetchKgAll(name, PAGE_SIZE, offset);
      if (env === null) {
        notFound = true;
        return;
      }
      triples = env.data ?? [];
    } catch (e) {
      triplesError = `${e}`;
      triples = [];
    } finally {
      triplesLoading = false;
    }
  }

  async function loadCount(name: string) {
    try {
      const env = await fetchKgCount(name);
      activeCount = env?.data?.active ?? null;
    } catch {
      activeCount = null;
    }
  }

  async function loadSubject(subject: string) {
    selectedSubject = subject;
    mode = 'subject';
    triplesLoading = true;
    triplesError = '';
    try {
      const env = await fetchKgSubject(agentName, subject);
      if (env === null) {
        notFound = true;
        return;
      }
      triples = env.data ?? [];
    } catch (e) {
      triplesError = `${e}`;
      triples = [];
    } finally {
      triplesLoading = false;
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

  function humanTime(iso?: string): string {
    if (!iso) return '—';
    const d = new Date(iso);
    if (Number.isNaN(d.getTime())) return iso;
    return d.toLocaleString();
  }

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

  // Move focus into the slide-over on open, mirroring `AgentConfigOverlay` —
  // otherwise a keyboard user is left focused on the (now covered) button
  // that opened it.
  onMount(() => {
    container?.focus();
  });
</script>

<svelte:window on:keydown={onKeydown} />

<div class="fixed inset-0 z-40 flex justify-end">
  <button
    type="button"
    class="absolute inset-0 bg-black/40"
    aria-label="Close Knowledge Graph"
    on:click={onClose}
  ></button>

  <section
    bind:this={container}
    role="dialog"
    aria-modal="true"
    aria-label="Knowledge Graph"
    tabindex="-1"
    class="relative z-10 flex h-full w-full max-w-3xl flex-col border-l border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface shadow-xl focus:outline-none"
  >
    <header class="flex shrink-0 items-center gap-2 border-b border-foundry-light-border dark:border-foundry-border px-4 py-3">
      <Network class="h-4 w-4 text-foundry-light-primary dark:text-foundry-primary" />
      <h2 class="font-mono text-xs font-semibold uppercase tracking-wide text-foundry-light-text dark:text-foundry-text">
        Knowledge Graph
      </h2>
      {#if activeCount !== null}
        <span class="rounded-md bg-foundry-light-border/50 dark:bg-black/30 px-1.5 py-0.5 font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">
          {activeCount.toLocaleString()} active triples
        </span>
      {/if}
      <span class="ml-auto flex items-center gap-2">
        <kbd class="rounded border border-foundry-light-border dark:border-foundry-border px-1.5 py-0.5 font-mono text-[10px] uppercase text-foundry-light-muted dark:text-foundry-text/40">
          Esc
        </kbd>
        <button
          type="button"
          class="rounded-md p-1.5 text-foundry-light-muted dark:text-foundry-text/60 hover:bg-foundry-light-primary/10 dark:hover:bg-foundry-primary/10 hover:text-foundry-light-primary dark:hover:text-foundry-primary"
          aria-label="Close Knowledge Graph"
          on:click={onClose}
        >
          <X class="h-4 w-4" />
        </button>
      </span>
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

      <!-- State: connected:true, genuinely empty graph (bound palace, zero
           triples) — distinct copy from the not-connected case above. -->
      {:else if subjects.length === 0}
        <p class="rounded-md border border-dashed border-foundry-light-border dark:border-foundry-border px-3 py-2 text-xs text-foundry-light-muted dark:text-foundry-text/40">
          {palace ? `Palace "${palace}"` : 'This agent'} is connected, but its Knowledge Graph has no
          triples yet.
        </p>

      <!-- State: connected:true with data — the explorer. -->
      {:else}
        <div class="grid min-h-0 flex-1 grid-cols-[minmax(180px,30%)_1fr] gap-3">
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
                      <th class="px-2 py-1.5 text-left font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">Conf.</th>
                      <th class="px-2 py-1.5 text-left font-mono text-[10px] uppercase tracking-wide text-foundry-light-muted dark:text-foundry-text/50">Valid from</th>
                    </tr>
                  </thead>
                  <tbody>
                    {#each triples as t, i (t.subject + '|' + t.predicate + '|' + t.object + '|' + i)}
                      <tr class="border-b border-foundry-light-border/60 dark:border-foundry-border/60 last:border-0">
                        <td class="px-2 py-1.5 font-mono text-[11px] text-foundry-light-text dark:text-foundry-text">{t.subject}</td>
                        <td class="px-2 py-1.5 font-mono text-[11px] text-foundry-light-text dark:text-foundry-text">{t.predicate}</td>
                        <td class="px-2 py-1.5 text-foundry-light-text/90 dark:text-foundry-text/90">{t.object}</td>
                        <td class="px-2 py-1.5 text-foundry-light-muted dark:text-foundry-text/50">{(t.confidence ?? 0).toFixed(2)}</td>
                        <td class="px-2 py-1.5 text-[11px] text-foundry-light-muted dark:text-foundry-text/50">{humanTime(t.valid_from)}</td>
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
</div>
