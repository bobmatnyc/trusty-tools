<script>
  /**
   * Why (#6423): the census's uncheckable rows were read-only, and one class of
   * them can never become valid again — registrations under a retired
   * `.base/.worktrees/` tree, which the daemon reports as "may become valid
   * again" forever because the heuristic cannot know the topology was retired.
   * The operator could see six of them and do nothing about them.
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
   * Test: the pure decisions live in `cleanupFlow.js` and are covered by
   * `cleanupFlow.test.js`; the route's own refusals are covered by
   * `routes::unjudged` in the Rust crate.
   */
  import { unjudgedConfirmMessage, unjudgedReviewNote } from './cleanupFlow.js';

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
      <span class="badge">kept — left registered</span>
    {:else if disposition === 'gone'}
      <span class="badge">deregistered</span>
    {:else}
      <button class="review" onclick={stage === 'closed' ? open : close} disabled={busy}>
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
            : 'stored in the daemon’s data directory'}
        </dd>
        <dt>Why it could not be checked</dt>
        <dd class="reason">{row.reason}</dd>
      </dl>

      {#if stage === 'review'}
        <!-- #6423 review round 2: the note and the confirmation are one fact,
             rendered twice. `cleanupFlow.js` owns it so they cannot disagree
             about whether a colocated row's data still exists. -->
        <p class="note">{unjudgedReviewNote(row)}</p>
        <div class="buttons">
          <button class="keep" onclick={keep} disabled={busy}>Keep registered</button>
          <button class="danger" onclick={askToConfirm} disabled={busy}>Deregister…</button>
        </div>
      {:else}
        <div class="confirm" role="group" aria-label={unjudgedConfirmMessage(row)}>
          <p class="prompt">{unjudgedConfirmMessage(row)}</p>
          <div class="buttons">
            <button class="cancel" onclick={open} disabled={busy}>Cancel</button>
            <button class="danger" onclick={confirmDeregister} disabled={busy}>
              {busy ? 'Deregistering…' : 'Deregister this registration'}
            </button>
          </div>
        </div>
      {/if}
    </div>
  {/if}
</li>

<style>
  li { display: block; padding: 0.3rem 0; font-size: 0.8rem; }
  li.settled { opacity: 0.65; }
  .row { display: flex; align-items: baseline; gap: 0.5rem; flex-wrap: wrap; }
  .path { color: var(--trusty-text-secondary); overflow-wrap: anywhere; }
  .reason { color: var(--trusty-text-muted); font-style: italic; overflow-wrap: anywhere; margin: 0.1rem 0 0; }
  .badge {
    font-size: 0.7rem; font-weight: 600; color: var(--trusty-text-secondary);
    border: 1px solid var(--trusty-border); border-radius: 0.25rem; padding: 0.05rem 0.35rem;
  }
  code {
    font-family: 'JetBrains Mono', monospace; font-size: 0.75rem;
    background: var(--trusty-surface-raised); padding: 0.1rem 0.35rem; border-radius: 0.25rem;
  }

  button { font: inherit; cursor: pointer; border-radius: 0.35rem; border: 1px solid transparent; }
  button:disabled { cursor: default; opacity: 0.6; }
  .review, .cancel, .keep {
    background: transparent; border-color: var(--trusty-border);
    color: var(--trusty-text-primary); font-size: 0.72rem; padding: 0.1rem 0.5rem;
  }
  .danger {
    background: var(--trusty-danger); color: #fff;
    font-size: 0.72rem; font-weight: 600; padding: 0.15rem 0.6rem;
  }

  .panel {
    margin: 0.4rem 0 0.6rem; padding: 0.6rem;
    background: var(--trusty-surface-raised);
    border: 1px solid var(--trusty-border); border-radius: 0.4rem;
  }
  dl { display: grid; grid-template-columns: max-content 1fr; gap: 0.15rem 0.6rem; margin: 0; }
  dt { font-size: 0.72rem; font-weight: 600; color: var(--trusty-text-secondary); }
  dd { margin: 0; font-size: 0.76rem; color: var(--trusty-text-primary); overflow-wrap: anywhere; }
  .note { margin: 0.5rem 0 0.4rem; font-size: 0.75rem; color: var(--trusty-text-secondary); }
  .buttons { display: flex; gap: 0.4rem; }

  .confirm {
    display: flex; flex-direction: column; gap: 0.4rem; margin-top: 0.5rem; padding: 0.6rem;
    border: 1px solid var(--trusty-danger); border-radius: 0.4rem;
  }
  .prompt { margin: 0; font-size: 0.8rem; font-weight: 600; color: var(--trusty-text-primary); }

  .outcome { margin: 0.25rem 0; font-size: 0.78rem; color: var(--trusty-success); }
  .outcome.bad { color: var(--trusty-danger); }
</style>
