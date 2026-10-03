<script>
  /*
   * Why: a held index (#9059) looks like a stale one unless the console says
   * otherwise. One banner keeps the settings page and the roster's expanded row
   * saying the same thing.
   * What: renders an `IndexHold` from `indexHold.js` — the index id, the reason,
   * the invalid globs when known, and the fix. `settingsHref` adds a link to the
   * index settings page; the settings page itself omits it and points at the
   * form below instead.
   * Test: `IndexConfig.test.js`, `IndexPipeline.test.js`.
   */
  import { navigate } from '../router.svelte.js';

  /** @type {{ hold: import('../indexHold.js').IndexHold, settingsHref?: string|null }} */
  let { hold, settingsHref = null } = $props();
</script>

<div class="held" role="alert" data-testid="held-index-banner">
  <div class="held-title">
    Index <span class="text-mono">{hold.indexId}</span> is held
  </div>
  <p class="held-reason" data-testid="held-index-reason">{hold.reason}</p>
  {#if hold.patterns.length > 0}
    <ul class="held-globs" data-testid="held-index-globs">
      {#each hold.patterns as pattern (pattern)}
        <li><code>{pattern}</code></li>
      {/each}
    </ul>
  {/if}
  <p class="held-hint" data-testid="held-index-hint">
    {#if settingsHref}
      Fix or remove the invalid exclude glob in
      <a
        href={`#${settingsHref}`}
        onclick={(e) => {
          e.preventDefault();
          navigate(settingsHref);
        }}>index settings</a
      >
      and save.
    {:else}
      Fix or remove the invalid exclude glob in <strong>Exclude globs</strong> below and save.
    {/if}
    Saving valid globs releases the hold and starts a catch-up reindex.
  </p>
</div>

<style>
  .held {
    border: 1px solid var(--trusty-danger);
    background: var(--trusty-danger-soft);
    border-radius: var(--trusty-radius-sm);
    padding: var(--trusty-space-3) var(--trusty-space-4);
    margin-bottom: var(--trusty-space-4);
    font-size: var(--trusty-fs-sm);
    line-height: 1.5;
  }
  .held-title {
    font-weight: 600;
    color: var(--trusty-danger);
  }
  .held-reason,
  .held-hint {
    margin: var(--trusty-space-2) 0 0 0;
  }
  .held-reason {
    overflow-wrap: anywhere;
  }
  .held-globs {
    margin: var(--trusty-space-2) 0 0 0;
    padding-left: var(--trusty-space-5);
  }
</style>
