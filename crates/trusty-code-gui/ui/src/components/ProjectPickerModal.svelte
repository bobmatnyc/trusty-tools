<script lang="ts">
  // Why: Bob's product direction (issue #3365): "Let's call it a new
  // workstream, and users pick a project or just start chatting. Use a
  // modal for project selection." This is that modal — it replaces
  // `CreateSessionForm`'s prior inline browse-and-descend folder list (DOC-39
  // §4.2.1's 7a picker) with a flat, pre-resolved roster
  // (`lib/project-roster.ts`, `GET /projects`) the operator picks from
  // directly, plus an explicit "start chatting without a project" row for
  // the projectless path (DOC-39 §4.2's projectless state — this ticket
  // keeps the UI copy workstream-neutral rather than naming "session" or
  // "unbound", per the issue's own design note).
  //
  // What: A controlled modal — `open` is owned by the parent
  // (`StartWorkingForm.svelte`), not this component's own state, matching
  // Svelte's usual controlled-dialog shape. An `$effect` keyed on `open`
  // fetches the roster (via [`fetchProjectRoster`]) only while visible —
  // mirrors every other poller/fetcher's `AbortController`-per-lifetime
  // shape in this codebase, aborting on close/unmount so a slow fetch never
  // resolves into a closed (or re-opened, for a different reason) modal.
  // Picking a roster row calls `onSelect` with a fully-resolved
  // [`ProjectSelection`] (`isGitRepo: true` always — every roster entry is
  // already known to be a git repo, see `fs_browse::roster` module docs);
  // "start chatting without a project" calls `onSelect(null)`. Neither path
  // calls `onClose` itself — the parent's `onSelect` handler is expected to
  // close the modal, keeping "what was picked" and "is the modal open" as
  // two separate pieces of state the parent owns, not duplicated here.
  // Backdrop click and the `×` button call `onClose` directly.
  //
  // Update (issue #3435, code-critic PR #3439 review HIGH 2): `roster.source`
  // distinguishes "the shared mpm registry legitimately has nothing
  // registered" from "the registry was unreachable, this is the local-only
  // fallback view" — the #3363 lesson is that collapsing those into the same
  // all-`registered: false` shape hides a real outage from the operator. A
  // `source === 'fs_only'` roster renders a banner above the row list; row
  // selection/binding behavior is unchanged either way.
  // Test: `ProjectPickerModal.test.ts`.
  import { apiBase } from '../lib/api-config';
  import {
    fetchProjectRoster,
    projectCandidateLabel,
    type ProjectCandidate,
    type RosterSource,
  } from '../lib/project-roster';
  import type { ProjectSelection } from '../lib/new-workstream';

  let {
    open,
    onSelect,
    onClose,
  }: {
    open: boolean;
    onSelect: (project: ProjectSelection | null) => void;
    onClose: () => void;
  } = $props();

  type Phase = 'loading' | 'daemon-unreachable' | 'ready';

  let phase = $state<Phase>('loading');
  let entries = $state<ProjectCandidate[]>([]);
  let source = $state<RosterSource | undefined>(undefined);
  let error = $state<string | null>(null);

  async function load(signal: AbortSignal) {
    phase = 'loading';
    let base: string;
    try {
      base = await apiBase();
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        error = e instanceof Error ? e.message : String(e);
      }
      return;
    }
    if (signal.aborted) return;

    try {
      const roster = await fetchProjectRoster(base, signal);
      if (signal.aborted) return;
      entries = roster.entries;
      source = roster.source;
      phase = 'ready';
      error = null;
    } catch (e) {
      if (!signal.aborted) {
        phase = 'daemon-unreachable';
        error = e instanceof Error ? e.message : String(e);
      }
    }
  }

  $effect(() => {
    if (!open) return;
    const controller = new AbortController();
    void load(controller.signal);
    return () => controller.abort();
  });

  function pick(entry: ProjectCandidate) {
    onSelect({ path: entry.path, displayPath: projectCandidateLabel(entry), isGitRepo: true });
  }

  function pickProjectless() {
    onSelect(null);
  }
</script>

{#if open}
  <!-- No dimming fill — mirrors `WorkstreamSwitcher.svelte`'s identical
       invisible click-catcher backdrop (`theme.test.ts`'s theming-hygiene
       gate bars a raw, un-themed palette color utility and there is no
       dedicated dim-overlay token yet; a plain click-catcher matches
       established precedent here, not a regression). -->
  <button
    type="button"
    class="fixed inset-0 z-30 cursor-default"
    aria-label="close project picker"
    onclick={onClose}
  ></button>

  <div
    role="dialog"
    aria-modal="true"
    aria-label="choose a project"
    class="fixed left-1/2 top-1/2 z-40 w-96 max-w-[90vw] -translate-x-1/2 -translate-y-1/2 rounded-sm border-1.5 border-trusty-border bg-trusty-raised p-4 shadow-lg"
  >
    <div class="flex items-center justify-between">
      <h2 class="font-display text-xs font-bold uppercase tracking-wide text-trusty-text">
        choose a project
      </h2>
      <button
        type="button"
        aria-label="close"
        class="text-trusty-text-muted hover:text-trusty-primary"
        onclick={onClose}
      >
        ×
      </button>
    </div>

    {#if phase === 'ready' && source === 'fs_only'}
      <!-- Issue #3435, code-critic PR #3439 review HIGH 2: mirrors
           `WorkstreamSwitcher.svelte`'s existing inline warning-banner
           pattern (`bg-status-warn/10` + `text-status-warn`), amber rather
           than red — the picker is still usable, just degraded. -->
      <p
        class="mt-2 rounded-sm bg-status-warn/10 px-2 py-1.5 text-[11px] text-status-warn"
      >
        shared registry unavailable — showing local checkouts only
      </p>
    {/if}

    <div class="mt-3 max-h-72 space-y-1 overflow-y-auto">
      {#if phase === 'loading'}
        <p class="px-1 py-2 text-xs text-trusty-text-muted">loading…</p>
      {:else if phase === 'daemon-unreachable'}
        <p class="px-1 py-2 text-xs text-status-error">
          daemon unreachable{error ? ` — ${error}` : ''}
        </p>
      {:else if entries.length === 0}
        <p class="px-1 py-2 text-xs text-trusty-text-muted">no known projects found</p>
      {:else}
        {#each entries as entry (entry.path)}
          <button
            type="button"
            title={entry.path}
            class="flex w-full items-center justify-between gap-2 truncate rounded-sm px-2 py-1.5 text-left font-mono text-xs text-trusty-text hover:bg-trusty-card"
            onclick={() => pick(entry)}
          >
            <span class="truncate">{projectCandidateLabel(entry)}</span>
            {#if !entry.registered}
              <!-- Issue #3435: the registry is now primary, so a row without
                   it is a local-only, unregistered discovery — mirrors
                   `new-workstream.ts::bindingLabel`'s existing precedent for
                   a small state-driven label suffix (there: git repo vs.
                   plain directory). -->
              <span class="shrink-0 text-[10px] uppercase tracking-wide text-trusty-text-muted"
                >local only</span
              >
            {/if}
          </button>
        {/each}
      {/if}
    </div>

    <div class="mt-3 border-t border-trusty-border pt-3">
      <button
        type="button"
        class="w-full rounded-sm border-1.5 border-dashed border-trusty-border-strong px-2 py-1.5 text-center font-mono text-[11px] uppercase tracking-wide text-trusty-text-secondary hover:border-trusty-primary hover:text-trusty-primary"
        onclick={pickProjectless}
      >
        start chatting without a project
      </button>
    </div>
  </div>
{/if}
