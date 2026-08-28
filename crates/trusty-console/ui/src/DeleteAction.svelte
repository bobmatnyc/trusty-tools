<script>
  /**
   * Per-row delete control for a palace or a search index (#6360).
   *
   * Why: deleting a corpus is the most destructive thing the dashboard can do,
   * so a single click must never be enough. This control starts as a plain
   * button and only reveals the confirm step — which names the exact id — after
   * that first click. The delete itself is issued by the console's DELETE
   * route, which calls the owning daemon; nothing here deletes anything.
   *
   * What: a four-state control (idle → confirm → busy → done/error). On a
   * confirmed success it calls `onDeleted`, which the parent tab uses to
   * re-fetch its roster from the daemon rather than removing the row locally.
   * On any non-success it stays put and shows the daemon's own message.
   *
   * Test: the pure decisions live in `deleteFlow.js` and are covered by
   * `deleteFlow.test.js`; the end-to-end route behaviour is covered by
   * `routes::deletes` in the Rust crate.
   */
  import { KINDS, confirmMessage, deleteUrl, readDeleteResult } from './deleteFlow.js';

  /**
   * @type {{ kind: 'palace' | 'index', id: string, onDeleted: () => void }}
   */
  let { kind, id, onDeleted } = $props();

  // `$derived`, not a plain `const`: a bare read of `kind` here would capture
  // only its initial value, which Svelte 5 flags — and would silently render
  // the wrong noun if a row were ever re-used for the other kind.
  let spec = $derived(KINDS[kind]);

  /** 'idle' | 'confirm' | 'busy' | 'error' — 'idle' is the only one-click state. */
  let stage = $state('idle');
  /** Whether the kind's extra option (force / delete_data) is enabled. */
  let option = $state(false);
  /** The daemon's own message from the last failed attempt. */
  let failure = $state('');

  function openConfirm() {
    stage = 'confirm';
    option = false;
    failure = '';
  }

  function cancel() {
    stage = 'idle';
    failure = '';
  }

  /**
   * Issue the delete and believe only what the response says.
   *
   * A refusal, a no-op, or an unreachable daemon all leave the row in place
   * with the reason shown — the roster is never re-fetched on a failure, so a
   * failed delete can never be mistaken for a successful one by a row that
   * happened to disappear for another reason.
   */
  async function confirmDelete() {
    stage = 'busy';
    failure = '';
    let status = 0;
    let body = null;
    try {
      const resp = await fetch(deleteUrl(kind, id, option), { method: 'DELETE' });
      status = resp.status;
      body = await resp.json().catch(() => null);
    } catch (e) {
      stage = 'error';
      failure = `The console could not reach its own delete route: ${e.message}`;
      return;
    }

    const outcome = readDeleteResult(status, body);
    if (!outcome.ok) {
      stage = 'error';
      failure = outcome.message;
      return;
    }
    stage = 'idle';
    onDeleted();
  }
</script>

<div class="delete-action">
  {#if stage === 'idle'}
    <button class="danger-link" onclick={openConfirm} aria-label={`Delete ${spec.noun} ${id}`}>
      Delete
    </button>
  {:else if stage === 'busy'}
    <span class="busy">Deleting…</span>
  {:else}
    <div class="confirm" role="group" aria-label={confirmMessage(kind, id)}>
      <p class="prompt">{confirmMessage(kind, id)}</p>
      <label class="opt">
        <input type="checkbox" bind:checked={option} />
        {spec.optionLabel}
      </label>
      {#if stage === 'error'}
        <p class="failure" role="alert">{failure}</p>
      {/if}
      <div class="buttons">
        <button class="cancel" onclick={cancel}>Cancel</button>
        <button class="danger" onclick={confirmDelete}>
          {stage === 'error' ? 'Retry delete' : `Delete "${id}"`}
        </button>
      </div>
    </div>
  {/if}
</div>

<style>
  .delete-action { display: inline-block; }

  button {
    font: inherit;
    cursor: pointer;
    border-radius: 0.35rem;
    border: 1px solid transparent;
  }

  .danger-link {
    background: transparent;
    border-color: var(--trusty-border);
    color: var(--trusty-danger);
    font-size: 0.78rem;
    padding: 0.15rem 0.5rem;
  }
  .danger-link:hover {
    background: color-mix(in srgb, var(--trusty-danger) 12%, transparent);
  }

  .busy { font-size: 0.78rem; color: var(--trusty-text-secondary); }

  .confirm {
    display: flex;
    flex-direction: column;
    gap: 0.4rem;
    min-width: 17rem;
    padding: 0.6rem;
    text-align: left;
    background: var(--trusty-surface-raised);
    border: 1px solid var(--trusty-danger);
    border-radius: 0.4rem;
  }
  .prompt {
    margin: 0;
    font-size: 0.8rem;
    font-weight: 600;
    color: var(--trusty-text-primary);
  }
  .opt {
    display: flex;
    align-items: center;
    gap: 0.35rem;
    font-size: 0.75rem;
    color: var(--trusty-text-secondary);
  }
  .failure {
    margin: 0;
    font-size: 0.75rem;
    color: var(--trusty-danger);
    overflow-wrap: anywhere;
  }
  .buttons { display: flex; gap: 0.4rem; }
  .cancel {
    background: transparent;
    border-color: var(--trusty-border);
    color: var(--trusty-text-secondary);
    font-size: 0.78rem;
    padding: 0.2rem 0.6rem;
  }
  .danger {
    background: var(--trusty-danger);
    color: #fff;
    font-size: 0.78rem;
    font-weight: 600;
    padding: 0.2rem 0.6rem;
  }
</style>
