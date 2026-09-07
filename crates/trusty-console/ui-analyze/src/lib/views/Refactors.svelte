<script>
  /*
   * Why: Operators want a prioritized worklist of "fix these next" items — the
   * refactor-suggestions endpoint already ranks by severity + impact, so this
   * view just needs to render the queue clearly with severity-driven color.
   * What: Severity filter dropdown, then a vertical card list. Each card shows
   * a severity badge, file+lines, function, refactor type chip, rationale,
   * and the suggested_action body.
   * Test: Set min severity to "high"; expect only high/critical cards.
   */
  import { onMount } from 'svelte';
  import {
    getSelectedIndex,
    getRefactors,
    refreshRefactors
  } from '../state.svelte.js';

  let selected = $derived(getSelectedIndex());
  let refactors = $derived(getRefactors());
  let minSeverity = $state('low');
  let topK = $state(20);

  $effect(() => {
    if (!selected) return;
    refreshRefactors(selected, { minSeverity, topK }).catch(() => {});
  });

  onMount(() => {
    if (selected) refreshRefactors(selected, { minSeverity, topK }).catch(() => {});
  });
</script>

<h1 class="page-title">Refactor Suggestions</h1>

{#if !selected}
  <div class="card"><div class="empty">Select an index in the top bar.</div></div>
{:else}
  <div class="filter-bar">
    <label class="text-xs text-muted">
      Min severity
      <select class="select" bind:value={minSeverity} style="margin-left: 8px; width: 140px">
        <option value="low">low</option>
        <option value="medium">medium</option>
        <option value="high">high</option>
        <option value="critical">critical</option>
      </select>
    </label>
    <label class="text-xs text-muted">
      Top K
      <input
        class="input"
        type="number"
        min="1"
        max="200"
        bind:value={topK}
        style="margin-left: 8px; width: 100px"
      />
    </label>
  </div>

  {#if refactors.length === 0}
    <div class="card"><div class="empty">No suggestions at this severity.</div></div>
  {:else}
    <div class="refactor-grid">
      {#each refactors as r}
        {@const sev = (r.severity || 'low').toString().toLowerCase()}
        <div class="refactor-card">
          <div class="rc-head">
            <span class="badge sev-{sev}">{sev}</span>
            <span class="rc-type">{r.refactor_type || r.type || 'refactor'}</span>
          </div>
          <div class="rc-target text-mono text-xs">
            <strong>{r.function_name || r.symbol || '—'}</strong>
            <span class="text-muted">— {r.file || ''}{r.start_line ? `:${r.start_line}` : ''}{r.end_line ? `–${r.end_line}` : ''}</span>
          </div>
          {#if r.rationale}
            <div class="rc-rationale">{r.rationale}</div>
          {/if}
          {#if r.suggested_action}
            <div class="rc-action">
              <div class="rc-action-label">Suggested action</div>
              <pre>{r.suggested_action}</pre>
            </div>
          {/if}
          {#if r.metrics}
            <div class="rc-meta text-xs text-muted">
              {#if r.metrics.cyclomatic != null}cyclo {r.metrics.cyclomatic}{/if}
              {#if r.metrics.cognitive != null} • cognitive {r.metrics.cognitive}{/if}
              {#if r.metrics.loc != null} • loc {r.metrics.loc}{/if}
            </div>
          {/if}
        </div>
      {/each}
    </div>
  {/if}
{/if}

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    margin: 0 0 var(--trusty-space-5) 0;
    font-weight: 600;
  }
  .filter-bar {
    display: flex;
    gap: var(--trusty-space-4);
    align-items: center;
    margin-bottom: var(--trusty-space-4);
  }
  .refactor-grid {
    display: grid;
    grid-template-columns: repeat(auto-fill, minmax(420px, 1fr));
    gap: var(--trusty-space-4);
  }
  .refactor-card {
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
    padding: var(--trusty-space-4);
    box-shadow: var(--trusty-shadow-sm);
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-3);
  }
  .rc-head {
    display: flex;
    justify-content: space-between;
    align-items: center;
  }
  .rc-type {
    padding: 2px 10px;
    border-radius: 999px;
    background: var(--trusty-accent-soft);
    color: var(--trusty-accent);
    font-size: var(--trusty-fs-xs);
    font-weight: 600;
    text-transform: uppercase;
    letter-spacing: 0.04em;
  }
  .rc-target {
    word-break: break-all;
  }
  .rc-rationale {
    color: var(--trusty-text-secondary);
    font-size: var(--trusty-fs-sm);
    line-height: 1.5;
  }
  .rc-action {
    background: var(--bg);
    border: 1px solid var(--border);
    border-radius: var(--trusty-radius-sm);
    padding: var(--trusty-space-3);
  }
  .rc-action-label {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    margin-bottom: 6px;
  }
  .rc-action pre {
    margin: 0;
    white-space: pre-wrap;
    word-break: break-word;
    color: var(--trusty-text-primary);
    font-size: var(--trusty-fs-sm);
  }
  .rc-meta {
    margin-top: auto;
  }
</style>
