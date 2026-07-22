<script lang="ts">
  /**
   * Why: #3219 — the mockup's SESSION RECAP rail (docs/design/gui/Foundry
   * Ecosystem.dc.html:220-238) is a persistent 300px right `<aside>` with
   * three always-visible sections (AGENTS ACTIVE, FILES TOUCHED, TOKENS),
   * not a horizontal band that only appears after a `recap_generated`
   * event. The old horizontal-band behavior (#371) is preserved as a
   * fourth, conditional section fed by the same `recaps` store, so the
   * prose summary + step/result table users already rely on don't
   * disappear — they just move into the rail.
   * What: A right-hand `<aside>` (own vertical scroll) reading the new
   * `workflowState` store (#3217) for agents/files/tokens, and the
   * existing `recaps` store for the summary/table fold. Collapses below
   * the `xl` breakpoint (see the responsive note in the issue) since at
   * narrower widths there isn't room for sidebar + chat + a 300px rail.
   * Test: Dispatch SSE `agent_spawned`/`agent_message`/`phase_*` events and
   * a `recap_generated` event for the active project; observe AGENTS
   * ACTIVE/FILES TOUCHED/TOKENS populate live and the recap fold appear
   * with its summary; click the recap header to collapse/expand the table.
   */
  import { recaps } from '../stores/recap';
  import { activeProjectId } from '../stores/app';
  import { workflowState, type AgentActivityStatus } from '../stores/workflow';
  import { isDesktop } from '../lib/transport';

  let collapsed = false;
  const desktop = isDesktop();

  $: currentRecap = $recaps.get($activeProjectId) ?? null;
  $: agents = $workflowState.agents;
  $: filesTouched = $workflowState.filesTouched;
  $: tokens = $workflowState.tokens;

  function toggleCollapse() {
    collapsed = !collapsed;
  }

  function dotClass(status: AgentActivityStatus): string {
    if (status === 'working') return 'bg-foundry-light-primary dark:bg-foundry-primary';
    if (status === 'failed') return 'bg-red-500 dark:bg-red-400';
    return 'bg-foundry-light-muted dark:bg-foundry-text/40';
  }

  function fmtTokenCount(n: number): string {
    return n >= 1000 ? `${(n / 1000).toFixed(1)}k` : `${n}`;
  }

  function fmtCost(c: number): string {
    return c < 0.01 ? `$${c.toFixed(4)}` : `$${c.toFixed(2)}`;
  }
</script>

<aside
  class="hidden xl:flex w-[300px] flex-shrink-0 flex-col border-l border-foundry-light-border dark:border-foundry-border bg-foundry-light-surface dark:bg-foundry-surface overflow-y-auto"
>
  <div
    class="px-4 py-3 border-b border-foundry-light-border dark:border-foundry-border font-mono text-[10px] font-semibold uppercase tracking-widest text-foundry-light-muted dark:text-foundry-text/60"
  >
    Session Recap
  </div>

  <div class="flex flex-col gap-5 px-4 py-4 text-xs font-mono">
    <!-- AGENTS ACTIVE -->
    <section>
      <h3 class="mb-2 text-[10px] font-semibold uppercase tracking-widest text-foundry-light-primary dark:text-foundry-sidebar-accent">
        Agents Active
      </h3>
      {#if agents.length === 0}
        {#if desktop}
          <!-- Why (#3257 code-critic HIGH / #3258): desktop mode never
               receives agent_spawned/agent_message/agent_done/agent_failed
               SSE events (App.svelte's SSE bridge is browser-only), and
               today's terminal PmResponse carries no per-agent summary
               either — see PmResponseLike.agents_active doc comment in
               stores/workflow.ts. `agents.length === 0` in desktop mode is
               therefore NOT "no agents ran"; it's "we have no way to know."
               Saying "No agents active." would be actively misleading. -->
          <p class="text-foundry-light-muted dark:text-foundry-text/40">
            Live agent status unavailable in desktop mode (see #3258).
          </p>
        {:else}
          <p class="text-foundry-light-muted dark:text-foundry-text/40">No agents active.</p>
        {/if}
      {:else}
        <ul class="flex flex-col gap-2">
          {#each agents as a (a.agent)}
            <li class="flex items-center gap-2 min-w-0">
              <span class="h-1.5 w-1.5 flex-shrink-0 rounded-full {dotClass(a.status)}" aria-hidden="true"></span>
              <span class="font-medium text-foundry-light-text dark:text-foundry-text">{a.agent}</span>
              <span class="truncate text-foundry-light-muted dark:text-foundry-text/50">{a.lastActivity ?? a.status}</span>
            </li>
          {/each}
        </ul>
      {/if}
    </section>

    <!-- FILES TOUCHED -->
    <section>
      <h3 class="mb-2 text-[10px] font-semibold uppercase tracking-widest text-foundry-light-primary dark:text-foundry-sidebar-accent">
        Files Touched
      </h3>
      {#if filesTouched.length === 0}
        <p class="text-foundry-light-muted dark:text-foundry-text/40">No files touched yet.</p>
      {:else}
        <ul class="flex flex-col gap-1 text-foundry-light-text/80 dark:text-foundry-text/80">
          {#each filesTouched as f (f)}
            <li class="truncate" title={f}>{f}</li>
          {/each}
        </ul>
      {/if}
    </section>

    <!-- TOKENS -->
    <section>
      <h3 class="mb-2 text-[10px] font-semibold uppercase tracking-widest text-foundry-light-primary dark:text-foundry-sidebar-accent">
        Tokens
      </h3>
      {#if tokens}
        <div class="flex items-center justify-between text-foundry-light-text dark:text-foundry-text">
          <span>{fmtTokenCount(tokens.tokensIn)} in / {fmtTokenCount(tokens.tokensOut)} out</span>
          <span class="text-foundry-light-muted dark:text-foundry-text/60">{fmtCost(tokens.costUsd)}</span>
        </div>
        {#if tokens.model}
          <div class="mt-1 truncate text-foundry-light-muted dark:text-foundry-text/50">{tokens.model}</div>
        {/if}
        <!-- Why (#3219 scope note): the mockup shows a "42.1k / 200k · 21%"
             progress bar, but no context-window max is available from the
             server today and per-model constants aren't reliably knowable
             client-side — inventing one would show a fabricated limit.
             Deliberately showing absolute numbers + cost only; see PR body. -->
      {:else}
        <p class="text-foundry-light-muted dark:text-foundry-text/40">No token data yet.</p>
      {/if}
    </section>

    <!-- Folded recap summary/table (#371) -->
    {#if currentRecap}
      <section class="border-t border-foundry-light-border dark:border-foundry-border pt-4">
        <button
          type="button"
          class="w-full flex items-center gap-2 text-left text-foundry-teal"
          on:click={toggleCollapse}
          aria-expanded={!collapsed}
        >
          <span aria-hidden="true">※</span>
          <span class="text-[10px] font-semibold uppercase tracking-widest">Recap</span>
          <span class="ml-auto text-foundry-light-muted dark:text-foundry-text/40" aria-hidden="true">
            {collapsed ? '▲' : '▼'}
          </span>
        </button>
        <p class="mt-1.5 text-foundry-light-text/70 dark:text-foundry-text/70">{currentRecap.summary}</p>

        {#if !collapsed && currentRecap.table_rows.length > 0}
          <table class="mt-2 w-full border-collapse">
            <tbody>
              {#each currentRecap.table_rows as [step, result], i (i)}
                <tr class="border-b border-foundry-teal/10 last:border-0 recap-row align-top">
                  <td class="py-1 pr-3 text-foundry-teal/80 whitespace-nowrap">{step}</td>
                  <td class="py-1 text-foundry-light-text/80 dark:text-foundry-text/70 break-words">{result}</td>
                </tr>
              {/each}
            </tbody>
          </table>
        {/if}
      </section>
    {/if}
  </div>
</aside>

<style>
  /* Why (#3387): was an unsanctioned raw `rgb(20 184 166 / 0.04)` teal-500
     literal with no token relationship (audit finding B2). Now reads
     --trusty-surface-hover (app.css), the DS's own sanctioned hover/tint
     token — same rust-derived hue family as the rest of the app, and
     correctly light/dark-reactive. Alternate-row striping itself (nth-child)
     is kept as-is; the DS's "no zebra-striped tables" guardrail is a
     structural change out of scope for this token-layer pass — flagged as a
     follow-up, not fixed here.
     Test: Inspect the recap fold's table — even rows have a faint tint. */
  tbody tr.recap-row:nth-child(even) {
    background-color: var(--trusty-surface-hover);
  }
</style>
