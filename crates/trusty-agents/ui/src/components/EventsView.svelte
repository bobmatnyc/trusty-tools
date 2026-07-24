<script lang="ts">
  /**
   * Why (#3819, epic #3052): Bob's precise EVENTS spec — a single
   * deterministic events page, events tagged by LISTENER, and an
   * include/exclude toggle per listener/type where EXCLUDED events stay
   * visible (muted), not hidden, so a user can see what they're turning off
   * and turn it back on. Philosophy: inference configures listeners to
   * minimize events to only agent-wake-worthy ones, because each included
   * event type can wake an agent = inference cost — the UI surfaces that
   * cost explicitly. Chat is its own channel, NOT part of the eventstream —
   * this page is upstream-provider events only. There is no real listener
   * backend yet (#3798 family), so today's only "listener" bucket with live
   * traffic is Slack (`source: 'slack'`, #3754); the `task` bucket (chat/
   * agent lifecycle events) is EXCLUDED by default to reflect "chat is not
   * part of the eventstream" without ripping out the plumbing that still
   * feeds it into `eventLog` — see `lib/events.ts`'s doc comment for the
   * full seam description.
   *
   * #3820 update: `gmail` is now a REAL listener bucket, not a placeholder —
   * `crate::listeners::poll`'s Gmail history-poll engine publishes
   * `listener_event_received` events onto the same bus this view already
   * reads, so Gmail rows here are live data, not a concept mock. The
   * "→ wakes Izzie" affordance for `gmail` is likewise real: a wake only
   * actually fires when izzie's `agent.toml` `[[listeners]]` binding
   * matches (stage two) — this badge is a simplified always-shown hint, not
   * a guarantee every gmail row woke her.
   * What: `excludedSources` (not a single selected bucket) drives the
   * include/exclude toggle row; excluded rows render at reduced opacity
   * instead of disappearing. A free-text filter narrows further (applies
   * regardless of exclude state).
   * Test: `lib/events.test.ts` covers the reducer/store this renders.
   *
   * Concept-demo addition (Bob): an included event is meant to trigger an
   * agent reaction in that agent's chat pane (one continuous chat, the
   * reaction classified under an inferred task, ask-first by default). No
   * real "wake" plumbing exists yet (#3798/#3820) — this view only shows the
   * "→ wakes <agent>" affordance on included, listener-sourced rows so the
   * concept reads end to end, per Bob's explicit "don't build real wake
   * plumbing, this is the concept surface" instruction.
   */
  import { Filter, AlertTriangle, Zap } from 'lucide-svelte';
  import { eventLog, type EventRow } from '../lib/events';

  const SOURCE_LABELS: Record<EventRow['source'], string> = {
    task: 'Chat / Task',
    slack: 'Slack',
    system: 'System',
    gmail: 'Gmail',
  };

  // #3819 concept affordance: which agent a listener source would wake, if
  // wake plumbing existed. Slack already routes to the CTO Bot persona
  // elsewhere in this codebase (`SlackMirror.svelte`'s "CTO Bot" identity),
  // so that's the honest existing association to demo rather than an
  // invented one; `task`/`system` never wake anything (chat is its own
  // channel, and system rows are diagnostic).
  const WAKES_AGENT: Partial<Record<EventRow['source'], string>> = {
    slack: 'CTO Assistant',
    gmail: 'Izzie',
  };

  // #3819: chat/task lifecycle events are excluded by default — "chat is a
  // separate channel, not part of the eventstream." Slack (a real upstream
  // listener) and system diagnostics start included.
  let excludedSources = new Set<EventRow['source']>(['task']);
  let textFilter = '';

  function toggleSource(source: EventRow['source']) {
    const next = new Set(excludedSources);
    if (next.has(source)) next.delete(source);
    else next.add(source);
    excludedSources = next;
  }

  $: rows = $eventLog
    .slice()
    .reverse()
    .filter((r) => textFilter.trim() === '' || r.summary.toLowerCase().includes(textFilter.trim().toLowerCase()));

  $: includedCount = rows.filter((r) => !excludedSources.has(r.source)).length;

  function formatTime(ms: number): string {
    return new Date(ms).toLocaleTimeString();
  }

  function sourceBadgeClasses(source: EventRow['source']): string {
    switch (source) {
      case 'slack':
        return 'bg-purple-500/15 text-purple-600 dark:text-purple-400';
      case 'gmail':
        return 'bg-red-500/15 text-red-600 dark:text-red-400';
      case 'system':
        return 'bg-foundry-light-border/50 dark:bg-black/30 text-foundry-light-muted dark:text-foundry-text/50';
      default:
        return 'bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary';
    }
  }
</script>

<div class="flex flex-1 flex-col overflow-hidden">
  <div class="flex flex-col gap-2 border-b border-foundry-light-border dark:border-foundry-border px-4 py-2.5">
    <div class="flex items-center gap-2">
      <Filter class="h-4 w-4 text-foundry-light-muted dark:text-foundry-text/50" />
      <div class="flex items-center gap-1">
        {#each Object.entries(SOURCE_LABELS) as [id, label] (id)}
          <button
            type="button"
            aria-pressed={!excludedSources.has(id as EventRow['source'])}
            class="rounded-md border px-2 py-1 font-mono text-[11px] font-semibold uppercase tracking-wide transition-colors {excludedSources.has(id as EventRow['source'])
              ? 'border-foundry-light-border dark:border-foundry-border text-foundry-light-muted/60 dark:text-foundry-text/30'
              : 'border-foundry-light-primary dark:border-foundry-primary bg-foundry-light-primary/15 dark:bg-foundry-primary/15 text-foundry-light-primary dark:text-foundry-primary'}"
            on:click={() => toggleSource(id as EventRow['source'])}
          >
            {label} {excludedSources.has(id as EventRow['source']) ? '(excluded)' : '(included)'}
          </button>
        {/each}
      </div>
      <input
        type="text"
        bind:value={textFilter}
        placeholder="Filter…"
        class="ml-2 w-56 rounded-md border border-foundry-light-border dark:border-foundry-primary/30 bg-foundry-light-bg dark:bg-foundry-bg px-2 py-1 text-xs text-foundry-light-text dark:text-foundry-text focus:border-foundry-light-primary dark:focus:border-foundry-primary focus:outline-none"
      />
      <span class="ml-auto text-[11px] text-foundry-light-muted dark:text-foundry-text/40">{includedCount} included / {rows.length} total</span>
    </div>
    <p class="flex items-start gap-1.5 text-[11px] text-foundry-amber">
      <AlertTriangle class="mt-0.5 h-3 w-3 shrink-0" />
      Each INCLUDED event type can wake an agent — more inclusions mean more inference cost.
      Configure listeners to include only agent-wake-worthy events; excluded events stay listed
      here (muted) so you can review and re-include them.
    </p>
  </div>

  <div class="flex-1 overflow-y-auto px-2 py-2">
    {#if rows.length === 0}
      <p class="px-2 py-6 text-center text-sm text-foundry-light-muted dark:text-foundry-text/40">
        No events yet — this fills in as connected listeners (Slack, Gmail today; Calendar once
        the eventstream connector framework, #3798, extends to it) send activity. Chat/task turns
        are a separate channel and are excluded by default.
      </p>
    {:else}
      <table class="w-full border-collapse text-left text-xs">
        <tbody>
          {#each rows as row, i (row.received_at + '-' + i)}
            <tr class="border-b border-foundry-light-border/50 dark:border-foundry-border/50 {excludedSources.has(row.source) ? 'opacity-40' : ''}">
              <td class="whitespace-nowrap py-1.5 pr-3 font-mono text-[11px] text-foundry-light-muted dark:text-foundry-text/40">
                {formatTime(row.received_at)}
              </td>
              <td class="py-1.5 pr-3">
                <span class="rounded px-1.5 py-0.5 font-mono text-[10px] font-semibold uppercase tracking-wide {sourceBadgeClasses(row.source)}">
                  {row.source}
                </span>
              </td>
              <td class="py-1.5 pr-3 font-mono text-[10px] text-foundry-light-muted dark:text-foundry-text/40">{row.type}</td>
              <td class="py-1.5 text-foundry-light-text dark:text-foundry-text">
                {row.summary}
                {#if !excludedSources.has(row.source) && WAKES_AGENT[row.source]}
                  <span class="ml-1.5 inline-flex items-center gap-1 rounded bg-foundry-amber/15 px-1.5 py-0.5 font-mono text-[10px] font-semibold text-foundry-amber">
                    <Zap class="h-2.5 w-2.5" /> wakes {WAKES_AGENT[row.source]}
                  </span>
                {/if}
              </td>
            </tr>
          {/each}
        </tbody>
      </table>
    {/if}
  </div>
</div>
