<script>
  /**
   * Per-row compact control for a palace (#6371).
   *
   * Why: the Memory tab could delete a palace outright and do nothing short of
   * that. Compaction is the reclamation an operator reaches for first — it
   * drops vector entries with no drawer behind them and leaves every memory in
   * place — and the daemon has always been able to do it. Like the delete
   * beside it, it takes two clicks: the first reveals a confirm step naming the
   * exact palace, so a misclicked row is visible before anything runs.
   *
   * What: a four-state control (idle → confirm → busy → done/error), driven by
   * the console's compact route, which calls trusty-memory's own
   * `palace_compact`. Nothing here compacts anything. A success reports the
   * counts the daemon returned; any non-success leaves the row alone and shows
   * the daemon's own message.
   *
   * Test: the pure decisions live in `cleanupFlow.js` and are covered by
   * `cleanupFlow.test.js`; the route behaviour is covered by `routes::cleanup`
   * in the Rust crate.
   */
  import { compactConfirmMessage, compactUrl, readCompactResult } from './cleanupFlow.js';

  /** @type {{ id: string, onCompacted: () => void }} */
  let { id, onCompacted } = $props();

  /** 'idle' | 'confirm' | 'busy' | 'done' | 'error' */
  let stage = $state('idle');
  /** The message from the last attempt — the daemon's own words on a failure. */
  let message = $state('');

  function openConfirm() {
    stage = 'confirm';
    message = '';
  }

  function cancel() {
    stage = 'idle';
    message = '';
  }

  async function confirmCompact() {
    stage = 'busy';
    message = '';
    let status = 0;
    let body = null;
    try {
      const resp = await fetch(compactUrl(id), { method: 'POST' });
      status = resp.status;
      body = await resp.json().catch(() => null);
    } catch (e) {
      stage = 'error';
      message = `The console could not reach its own compact route: ${e.message}`;
      return;
    }

    const outcome = readCompactResult(status, body);
    message = outcome.message;
    if (!outcome.ok) {
      stage = 'error';
      return;
    }
    stage = 'done';
    onCompacted();
  }
</script>

<div class="compact-action">
  {#if stage === 'idle'}
    <button class="link" onclick={openConfirm} aria-label={`Compact palace ${id}`}>Compact</button>
  {:else if stage === 'busy'}
    <span class="busy">Compacting…</span>
  {:else if stage === 'done'}
    <span class="done" role="status">{message}</span>
  {:else}
    <div class="confirm" role="group" aria-label={compactConfirmMessage(id)}>
      <p class="prompt">{compactConfirmMessage(id)}</p>
      {#if stage === 'error'}
        <p class="failure" role="alert">{message}</p>
      {/if}
      <div class="buttons">
        <button class="cancel" onclick={cancel}>Cancel</button>
        <button class="go" onclick={confirmCompact}>
          {stage === 'error' ? 'Retry compact' : 'Compact'}
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .compact-action { display: inline-block; }

  button { font: inherit; cursor: pointer; border-radius: 0.35rem; border: 1px solid transparent; }

  .link {
    background: transparent; border-color: var(--trusty-border);
    color: var(--trusty-text-secondary); font-size: 0.78rem; padding: 0.15rem 0.5rem;
  }
  .link:hover { color: var(--trusty-text-primary); }

  .busy { font-size: 0.78rem; color: var(--trusty-text-secondary); }
  .done { font-size: 0.75rem; color: var(--trusty-success); overflow-wrap: anywhere; }

  .confirm {
    display: flex; flex-direction: column; gap: 0.4rem;
    min-width: 17rem; padding: 0.6rem; text-align: left;
    background: var(--trusty-surface-raised);
    border: 1px solid var(--trusty-border); border-radius: 0.4rem;
  }
  .prompt { margin: 0; font-size: 0.8rem; font-weight: 600; color: var(--trusty-text-primary); }
  .failure { margin: 0; font-size: 0.75rem; color: var(--trusty-danger); overflow-wrap: anywhere; }
  .buttons { display: flex; gap: 0.4rem; }
  .cancel {
    background: transparent; border-color: var(--trusty-border);
    color: var(--trusty-text-secondary); font-size: 0.78rem; padding: 0.2rem 0.6rem;
  }
  .go {
    background: var(--trusty-text-primary); color: var(--trusty-card-bg);
    font-size: 0.78rem; font-weight: 600; padding: 0.2rem 0.6rem;
  }
</style>
