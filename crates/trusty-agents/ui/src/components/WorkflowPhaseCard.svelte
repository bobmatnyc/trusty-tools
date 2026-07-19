<script lang="ts">
  /**
   * Why: #3218 — the Foundry mockup's signature interaction is an inline
   * RESEARCH/PLAN/IMPLEMENT/VERIFY checklist card rendered in the chat
   * stream, not just a "Running…" spinner. `workflowState.phases` (#3217)
   * has carried this data since the SSE `phase_started`/`phase_done`
   * events started feeding it — this component just renders it.
   * What: A rectangular, mono-labelled checklist card: one row per phase
   * with a ✓ (done) / spinner (running) / ○ (pending or failed shows ✕)
   * marker, the phase name, and its note (elapsed/cost once the terminal
   * `PmResponse` backfills them via `applyTaskResult`, blank until then).
   * Test: Manual — drive a `--workflow prescriptive` task against a running
   * `tagent --api` server in browser/SSE mode; observe rows tick from ○ to
   * spinner to ✓ as `phase_started`/`phase_done` events arrive.
   */
  import { Loader2 } from 'lucide-svelte';
  import { workflowState } from '../stores/workflow';

  $: phases = $workflowState.phases;
  $: doneCount = phases.filter((p) => p.status === 'done').length;

  function fmtElapsed(secs?: number): string {
    if (typeof secs !== 'number') return '';
    return secs < 60 ? `${Math.round(secs)}s` : `${Math.floor(secs / 60)}m ${Math.round(secs % 60)}s`;
  }

  function phaseNote(note: string | undefined, elapsedSecs: number | undefined, costUsd: number | undefined): string {
    const parts: string[] = [];
    const e = fmtElapsed(elapsedSecs);
    if (e) parts.push(e);
    if (typeof costUsd === 'number') parts.push(`$${costUsd.toFixed(costUsd < 0.01 ? 4 : 2)}`);
    if (note) parts.push(note);
    return parts.join(' · ');
  }
</script>

{#if phases.length > 0}
  <div class="rounded-md border border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface overflow-hidden font-mono text-xs">
    <div class="flex items-center justify-between px-3.5 py-2 bg-foundry-light-border/40 dark:bg-foundry-border/40 text-foundry-light-text dark:text-[#e9b98a] font-semibold uppercase tracking-wide">
      <span>Workflow</span>
      <span class="text-foundry-amber">Phase {Math.min(doneCount + 1, phases.length)}/{phases.length}</span>
    </div>
    <div class="flex flex-col gap-2 px-3.5 py-3">
      {#each phases as phase (phase.name)}
        <div class="flex items-center gap-2.5">
          <span class="w-4 flex-shrink-0 text-center">
            {#if phase.status === 'done'}
              <span class="text-foundry-teal font-semibold" aria-hidden="true">&#10003;</span>
            {:else if phase.status === 'running'}
              <Loader2 class="h-3 w-3 animate-spin text-foundry-amber inline-block" aria-hidden="true" />
            {:else if phase.status === 'failed'}
              <span class="text-red-500 dark:text-red-400 font-semibold" aria-hidden="true">&#10007;</span>
            {:else}
              <span class="text-foundry-light-muted dark:text-foundry-text/40" aria-hidden="true">&#9675;</span>
            {/if}
            <span class="sr-only">{phase.status}</span>
          </span>
          <span
            class="font-medium uppercase tracking-wide {phase.status === 'running'
              ? 'text-foundry-light-text dark:text-foundry-text'
              : 'text-foundry-light-muted dark:text-foundry-text/60'}"
          >{phase.name}</span>
          <span class="text-foundry-light-muted dark:text-foundry-text/50 truncate">
            {phaseNote(phase.note, phase.elapsedSecs, phase.costUsd) || (phase.status === 'pending' ? 'queued' : '')}
          </span>
        </div>
      {/each}
    </div>
  </div>
{/if}
