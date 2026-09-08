<script>
  /**
   * Per-palace compact and delete, moved off the console tab (#6928).
   *
   * Why: the owner's ruling makes the console's Memory tab display-only and
   * puts every action here. These two were the tab's `CompactAction.svelte`
   * and `DeleteAction.svelte`; they are one component now because they share a
   * row, a confirm discipline and an outcome line, and two components would
   * have meant two copies of all three.
   *
   * What: a two-click control per action — the first click reveals a confirm
   * step naming the exact palace, so a misclicked row is visible before
   * anything runs. Nothing here compacts or deletes: both go through the
   * console's routes, which call trusty-memory's own `palace_compact` and
   * `palace_delete`. A success reports what the daemon returned; any
   * non-success leaves the palace alone and shows the daemon's own message.
   *
   * Test: the pure decisions are in `palaceActions.js`, covered by
   * `palaceActions.test.js`; the routes are covered by `routes::cleanup` and
   * `routes::deletes` in the Rust crate.
   */
  import { compactPalace, deletePalace } from '../consoleApi.js';
  import {
    FORCE_LABEL,
    compactConfirmMessage,
    deleteConfirmMessage,
  } from '../palaceActions.js';

  /** @type {{ id: string, onChanged: () => void }} */
  let { id, onChanged } = $props();

  /** `null` when idle, otherwise `'compact'` or `'delete'`. */
  let pending = $state(null);
  /** 'confirm' | 'busy' | 'error' — only meaningful while `pending`. */
  let stage = $state('confirm');
  /** The last attempt's message: the daemon's own words on a failure. */
  let message = $state('');
  /** Whether a success line is showing, and what it said. */
  let done = $state('');
  /** #6422: force starts unticked — a refusal on a non-empty palace is safe. */
  let force = $state(false);

  function open(action) {
    pending = action;
    stage = 'confirm';
    message = '';
    done = '';
    force = false;
  }

  function cancel() {
    pending = null;
    message = '';
  }

  async function run() {
    const action = pending;
    stage = 'busy';
    message = '';
    const outcome =
      action === 'delete' ? await deletePalace(id, force) : await compactPalace(id);
    if (!outcome.ok) {
      stage = 'error';
      message = outcome.message;
      return;
    }
    pending = null;
    done = outcome.message;
    onChanged();
  }

  let prompt = $derived(
    pending === 'delete' ? deleteConfirmMessage(id) : compactConfirmMessage(id),
  );
</script>

<div class="palace-actions">
  {#if done}
    <span class="done" role="status">{done}</span>
  {:else if !pending}
    <button type="button" class="link" onclick={() => open('compact')}>Compact</button>
    <button type="button" class="link danger" onclick={() => open('delete')}>Delete</button>
  {:else if stage === 'busy'}
    <span class="busy">{pending === 'delete' ? 'Deleting…' : 'Compacting…'}</span>
  {:else}
    <div class="confirm" role="group" aria-label={prompt}>
      <p class="prompt">{prompt}</p>
      {#if pending === 'delete'}
        <label class="force">
          <input type="checkbox" bind:checked={force} />
          {FORCE_LABEL}
        </label>
      {/if}
      {#if stage === 'error'}
        <p class="failure" role="alert">{message}</p>
      {/if}
      <div class="buttons">
        <button type="button" class="cancel" onclick={cancel}>Cancel</button>
        <button type="button" class="go" onclick={run}>
          {#if stage === 'error'}Retry{:else if pending === 'delete'}Delete{:else}Compact{/if}
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .palace-actions {
    display: inline-flex;
    gap: var(--trusty-space-2, 0.4rem);
    align-items: flex-start;
  }
  button {
    font: inherit;
    cursor: pointer;
    border-radius: var(--trusty-radius-sm, 3px);
    border: 1px solid var(--trusty-border);
    background: transparent;
    color: var(--trusty-text-secondary);
    font-size: var(--trusty-fs-xs, 0.78rem);
    padding: 0.15rem 0.5rem;
  }
  .link:hover { color: var(--trusty-text-primary); }
  .danger:hover { color: var(--trusty-danger); border-color: var(--trusty-danger); }

  .busy { font-size: var(--trusty-fs-xs, 0.78rem); color: var(--trusty-text-secondary); }
  .done {
    font-size: var(--trusty-fs-xs, 0.75rem);
    color: var(--trusty-success);
    overflow-wrap: anywhere;
  }

  .confirm {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    min-width: 19rem;
    padding: 0.6rem;
    text-align: left;
    background: var(--trusty-surface-raised, var(--trusty-card-bg));
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius, 5px);
  }
  .prompt {
    margin: 0;
    font-size: var(--trusty-fs-sm, 0.8rem);
    font-weight: 600;
    color: var(--trusty-text-primary);
  }
  .force {
    display: flex;
    gap: 0.35rem;
    align-items: flex-start;
    font-size: var(--trusty-fs-xs, 0.75rem);
    color: var(--trusty-text-secondary);
  }
  .failure {
    margin: 0;
    font-size: var(--trusty-fs-xs, 0.75rem);
    color: var(--trusty-danger);
    overflow-wrap: anywhere;
  }
  .buttons { display: flex; gap: 0.4rem; }
  .go {
    background: var(--trusty-text-primary);
    color: var(--trusty-card-bg);
    border-color: var(--trusty-text-primary);
    font-weight: 600;
  }
</style>
