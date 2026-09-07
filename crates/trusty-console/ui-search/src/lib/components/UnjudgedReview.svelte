<script>
  /**
   * Why (#6423, ported to the dashboard by #6941): the census's uncheckable
   * rows were read-only, and one class of them can never become valid again —
   * registrations under a retired `.base/.worktrees/` tree, which the daemon
   * reports as "may become valid again" forever because the heuristic cannot
   * know the topology was retired. The operator could see six of them and do
   * nothing about them.
   *
   * What: one row's review. Collapsed it shows what the panel always showed.
   * Expanded it shows the full path, the daemon's reason, and the registration
   * metadata, then offers two dispositions — keep, which is a no-op, and
   * deregister, which is guarded by its own confirmation naming the path.
   *
   * Every guard here is per-row on purpose. This component holds no list and no
   * selection: the parent renders one of these per unjudged row, so nothing a
   * bulk action does can reach them.
   *
   * Test: the pure decisions live in `../cleanup.js` and are covered by
   * `cleanup.test.js`; the daemon's own refusals are covered by
   * `service::server::delete_guard` in trusty-search.
   */
  import { unjudgedConfirmMessage, unjudgedReviewNote } from '../cleanup.js';

  /**
   * @type {{
   *   row: {id: string, root_path: string, reason: string, colocated?: boolean,
   *         repo_identity?: string|null},
   *   disposition: 'none' | 'kept' | 'gone',
   *   busy: boolean,
   *   outcome: {ok: boolean, message: string} | null,
   *   onKeep: () => void,
   *   onDeregister: () => void,
   * }}
   */
  let { row, disposition, busy, outcome, onKeep, onDeregister } = $props();

  /** 'closed' | 'review' | 'confirm' */
  let stage = $state('closed');

  function open() {
    stage = 'review';
  }

  function close() {
    stage = 'closed';
  }

  function keep() {
    stage = 'closed';
    onKeep();
  }

  /** Confirming is a separate click from choosing to deregister. */
  function askToConfirm() {
    stage = 'confirm';
  }

  function confirmDeregister() {
    stage = 'closed';
    onDeregister();
  }
</script>

<li class:settled={disposition !== 'none'}>
  <div class="row">
    <code>{row.id}</code>
    <span class="path">{row.root_path}</span>
    {#if disposition === 'kept'}
      <span class="badge badge-muted">kept — left registered</span>
    {:else if disposition === 'gone'}
      <span class="badge badge-success">deregistered</span>
    {:else}
      <button class="btn btn-sm" onclick={stage === 'closed' ? open : close} disabled={busy}>
        {stage === 'closed' ? 'Review' : 'Close'}
      </button>
    {/if}
  </div>
  <p class="reason">{row.reason}</p>

  {#if outcome}
    <p class="outcome" class:bad={!outcome.ok} role="status">{outcome.message}</p>
  {/if}

  {#if stage !== 'closed' && disposition === 'none'}
    <div class="panel">
      <dl>
        <dt>Registration</dt>
        <dd><code>{row.id}</code></dd>
        <dt>Root path</dt>
        <dd class="path">{row.root_path}</dd>
        <dt>Repository</dt>
        <dd>{row.repo_identity ?? 'not recorded'}</dd>
        <dt>Index data</dt>
        <dd>
          {row.colocated
            ? 'stored beside the root, which is not reachable'
            : "stored in trusty-search's data directory"}
        </dd>
        <dt>Why it could not be checked</dt>
        <dd class="reason">{row.reason}</dd>
      </dl>

      {#if stage === 'review'}
        <!-- The note and the confirmation are one fact rendered twice;
             `cleanup.js` owns it so they cannot disagree about whether a
             colocated row's data still exists (#6423 review round 2). -->
        <p class="note">{unjudgedReviewNote(row)}</p>
        <div class="buttons">
          <button class="btn btn-sm" onclick={keep} disabled={busy}>Keep registered</button>
          <button class="btn btn-sm btn-danger" onclick={askToConfirm} disabled={busy}>
            Deregister…
          </button>
        </div>
      {:else}
        <div class="confirm" role="group" aria-label={unjudgedConfirmMessage(row)}>
          <p class="prompt">{unjudgedConfirmMessage(row)}</p>
          <div class="buttons">
            <button class="btn btn-sm" onclick={open} disabled={busy}>Cancel</button>
            <button class="btn btn-sm btn-danger" onclick={confirmDeregister} disabled={busy}>
              {busy ? 'Deregistering…' : 'Deregister this registration'}
            </button>
          </div>
        </div>
      {/if}
    </div>
  {/if}
</li>

<style>
  li {
    display: block;
    padding: var(--trusty-space-1) 0;
    font-size: var(--trusty-fs-sm);
  }
  li.settled {
    opacity: 0.65;
  }
  .row {
    display: flex;
    align-items: baseline;
    gap: var(--trusty-space-2);
    flex-wrap: wrap;
  }
  .path {
    color: var(--trusty-text-secondary);
    overflow-wrap: anywhere;
  }
  .reason {
    color: var(--trusty-text-muted);
    font-style: italic;
    overflow-wrap: anywhere;
    margin: 2px 0 0;
    font-size: var(--trusty-fs-xs);
  }
  code {
    font-family: var(--trusty-mono);
    font-size: var(--trusty-fs-xs);
    background: var(--trusty-surface-raised);
    padding: 1px var(--trusty-space-1);
    border-radius: var(--trusty-radius-sm);
  }
  .panel {
    margin: var(--trusty-space-2) 0 var(--trusty-space-3);
    padding: var(--trusty-space-3);
    background: var(--trusty-surface-raised);
    border: 1px solid var(--trusty-border);
    border-radius: var(--trusty-radius);
  }
  dl {
    display: grid;
    grid-template-columns: max-content 1fr;
    gap: 2px var(--trusty-space-3);
    margin: 0;
  }
  dt {
    font-size: var(--trusty-fs-xs);
    font-weight: 600;
    color: var(--trusty-text-secondary);
  }
  dd {
    margin: 0;
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-primary);
    overflow-wrap: anywhere;
  }
  .note {
    margin: var(--trusty-space-3) 0 var(--trusty-space-2);
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-secondary);
  }
  .buttons {
    display: flex;
    gap: var(--trusty-space-2);
  }
  .confirm {
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-2);
    margin-top: var(--trusty-space-3);
    padding: var(--trusty-space-3);
    border: 1px solid var(--trusty-danger);
    border-radius: var(--trusty-radius);
  }
  .prompt {
    margin: 0;
    font-size: var(--trusty-fs-sm);
    font-weight: 600;
    color: var(--trusty-text-primary);
  }
  .outcome {
    margin: var(--trusty-space-1) 0;
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-success);
  }
  .outcome.bad {
    color: var(--trusty-danger);
  }
</style>
