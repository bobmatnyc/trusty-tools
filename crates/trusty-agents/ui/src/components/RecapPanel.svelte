<script lang="ts">
  /**
   * Why: #371 — When the backend emits a `recap_generated` SSE event, the
   * latest recap for the active session should surface as a slim, collapsible
   * banner between the chat scrollback and the input area. This keeps the
   * step-by-step results visible without forcing the user to scroll up.
   * What: Subscribes to the recap store and renders a teal-tinted panel for
   * the active session's most recent recap, with a clickable header that
   * collapses/expands the result table.
   * Test: Dispatch a `recap_generated` SSE event for the active project, see
   * the panel appear with the summary; click the header to collapse the
   * table; switch projects and verify the panel hides when no recap exists.
   */
  import { recaps } from '../stores/recap';
  import { activeProjectId } from '../stores/app';

  let collapsed = false;

  $: currentRecap = $recaps.get($activeProjectId) ?? null;

  function toggleCollapse() {
    collapsed = !collapsed;
  }
</script>

{#if currentRecap}
  <div class="border-t border-ompm-teal/20 bg-ompm-teal/5 dark:bg-ompm-teal/10 text-xs font-mono">
    <button
      type="button"
      class="w-full flex items-center gap-2 px-3 py-1.5 text-ompm-teal hover:bg-ompm-teal/10 transition-colors"
      on:click={toggleCollapse}
      aria-expanded={!collapsed}
    >
      <span class="text-ompm-teal" aria-hidden="true">※</span>
      <span class="font-semibold">recap</span>
      <span
        class="text-ompm-light-text/60 dark:text-ompm-text/60 truncate flex-1 text-left"
      >
        · {currentRecap.summary}
      </span>
      <span class="text-ompm-light-muted dark:text-ompm-text/40" aria-hidden="true">
        {collapsed ? '▲' : '▼'}
      </span>
    </button>

    {#if !collapsed && currentRecap.table_rows.length > 0}
      <div class="px-3 pb-2">
        <table class="w-full border-collapse">
          <thead>
            <tr class="text-ompm-teal/70 border-b border-ompm-teal/20">
              <th class="text-left py-1 pr-4 w-32 font-normal">Step</th>
              <th class="text-left py-1 font-normal">Result</th>
            </tr>
          </thead>
          <tbody>
            {#each currentRecap.table_rows as [step, result], i (i)}
              <tr
                class="border-b border-ompm-teal/10 last:border-0"
                class:bg-white={false}
                class:bg-ompm-teal={i % 2 === 0}
              >
                <td class="py-0.5 pr-4 text-ompm-teal/80 whitespace-nowrap align-top">
                  {step}
                </td>
                <td class="py-0.5 text-ompm-light-text/80 dark:text-ompm-text/70 break-words">
                  {result}
                </td>
              </tr>
            {/each}
          </tbody>
        </table>
      </div>
    {/if}
  </div>
{/if}

<style>
  /* Why: Tailwind class-toggle for striping was clumsy with arbitrary teal
     opacity; a tiny scoped rule using nth-child gives clean alternating rows
     without an extra class plumbing layer.
     Test: Inspect rendered table — even rows have a faint teal tint, odd rows
     are fully transparent over the panel background. */
  tbody tr:nth-child(even) {
    background-color: rgb(20 184 166 / 0.04);
  }
  tbody tr {
    background-color: transparent;
  }
</style>
