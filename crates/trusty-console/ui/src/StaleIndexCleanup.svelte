<script>
  /**
   * Why: a host accumulates index registrations whose root was wiped (#4255),
   * and clearing them one row at a time — which is all #6360 offered — is why
   * 60 of them were still on the owner's machine keeping `warm_boot_degraded`
   * true. Worse, an allowlist-excluded registration never reaches the daemon's
   * in-memory registry, so it has no row in the Indexes table to delete (#6363).
   * This panel is the only place those registrations are visible at all.
   *
   * What: a four-stage flow (idle → reviewing → confirm → done/error). Nothing
   * is deleted before the operator has seen the exact list: the census is
   * fetched, every candidate is listed with its dead root path, and the confirm
   * step names the count and the fate of the on-disk data. Roots the daemon
   * declined to judge are listed too and cannot be selected — `cleanupFlow.js`
   * owns that rule.
   *
   * The candidate list is the DAEMON's census. This component decides nothing
   * about what is stale; it renders what trusty-search reports and sends back
   * the subset the operator confirmed.
   *
   * #6423 adds a second, separate flow for the rows the daemon declined to
   * judge, because one class of them can never become valid again and listing
   * them forever was the only thing on offer. That flow is per-row: review one,
   * then keep it or deregister it behind its own confirmation. It shares no
   * state with the batch above — `selected` is still built from the census's
   * `orphans` alone — so "shown, never selected" is unchanged.
   *
   * Test: the pure decisions live in `cleanupFlow.js` and are covered by
   * `cleanupFlow.test.js`; the batch route's per-item outcomes are covered by
   * `routes::cleanup` in the Rust crate.
   */
  import {
    CENSUS_URL,
    DEREGISTER_UNJUDGED_URL,
    PRUNE_DELETE_DATA_DEFAULT,
    PRUNE_URL,
    censusSummary,
    pruneConfirmMessage,
    readDeregisterResult,
    readPruneResult,
    selectableOrphans,
    unjudgedRows,
  } from './cleanupFlow.js';
  import UnjudgedReview from './UnjudgedReview.svelte';

  /** @type {{ onPruned: () => void }} */
  let { onPruned } = $props();

  /** 'idle' | 'scanning' | 'reviewing' | 'confirm' | 'busy' | 'done' */
  let stage = $state('idle');
  /** The daemon's census body, or null before one has been fetched. */
  let census = $state(null);
  /** Ids the operator has ticked, as a plain object so Svelte tracks writes. */
  let selected = $state({});
  /**
   * Whether the prune also destroys each index's on-disk corpus.
   *
   * #6422: starts ticked. Purging is the default and keeping the data is the
   * explicit opt-out.
   */
  let deleteData = $state(PRUNE_DELETE_DATA_DEFAULT);
  /** The message from the last completed attempt, or a fetch failure. */
  let outcome = $state(null);
  /** Per-id rows from the last prune. */
  let rows = $state([]);
  /**
   * What the operator decided about each reviewed uncheckable row (#6423).
   *
   * `'kept'` and `'gone'` are set only after the decision is made — `'gone'`
   * only after the daemon confirmed the deregistration, so a failed attempt
   * leaves the row exactly where it was and offers the action again.
   *
   * @type {Record<string, 'kept' | 'gone'>}
   */
  let disposed = $state({});
  /** Per-id outcome of the last deregister attempt. */
  let unjudgedOutcomes = $state({});
  /** The id whose deregistration is in flight, or null. */
  let deregistering = $state(null);

  let candidates = $derived(selectableOrphans(census));
  let unjudged = $derived(unjudgedRows(census));
  let chosen = $derived(candidates.filter((c) => selected[c.id]).map((c) => c.id));

  async function scan() {
    stage = 'scanning';
    outcome = null;
    rows = [];
    try {
      const resp = await fetch(CENSUS_URL);
      if (!resp.ok) throw new Error(`HTTP ${resp.status}`);
      census = await resp.json();
    } catch (e) {
      census = null;
      stage = 'idle';
      outcome = {
        ok: false,
        message: `Could not read trusty-search's registry census: ${e.message}`,
      };
      return;
    }
    // Every candidate starts ticked: the operator asked for a cleanup, and the
    // list they are about to confirm is the daemon's own. Untick, not tick, is
    // the exception.
    //
    // #6423: this reads `selectableOrphans` and nothing else, which is what
    // keeps an uncheckable row out of every bulk action. A fresh scan also
    // clears the per-row dispositions, because they were decisions about the
    // previous census.
    selected = Object.fromEntries(selectableOrphans(census).map((c) => [c.id, true]));
    disposed = {};
    unjudgedOutcomes = {};
    stage = 'reviewing';
  }

  /**
   * Record that the operator reviewed a row and chose to leave it registered.
   *
   * Nothing is sent: keeping is the default state, so this only stops the panel
   * offering the row as something still to decide (#6423 closure condition 3).
   *
   * @param {string} id The reviewed registration id.
   */
  function keepUnjudged(id) {
    disposed = { ...disposed, [id]: 'kept' };
    unjudgedOutcomes = { ...unjudgedOutcomes, [id]: null };
  }

  /**
   * Deregister one reviewed row, and believe only what the response says.
   *
   * Fail-closed (#6423): the row is marked `'gone'` only when the console route
   * confirmed the removal. Anything else — a refusal, an unreachable daemon, a
   * body that did not parse — leaves the row where it was, with the reason shown
   * beside it, so a failed deregistration is never counted as done.
   *
   * @param {{id: string, root_path: string}} row The reviewed registration.
   */
  async function deregisterUnjudged(row) {
    // #6423 review round 2: single-flight per row. The confirm button's
    // `disabled` reads a prop that has not re-rendered yet when the click lands,
    // so a fast double-click sends two requests for the same id. The loser gets
    // the daemon's "no registration was removed" and the row reads as failed
    // when it in fact succeeded.
    if (deregistering === row.id) return;
    deregistering = row.id;
    let status = 0;
    let body = null;
    try {
      const resp = await fetch(DEREGISTER_UNJUDGED_URL, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ id: row.id, root_path: row.root_path }),
      });
      status = resp.status;
      body = await resp.json().catch(() => null);
    } catch (e) {
      deregistering = null;
      const failed = {
        ok: false,
        message: `The console could not reach its own deregister route: ${e.message}`,
      };
      unjudgedOutcomes = { ...unjudgedOutcomes, [row.id]: failed };
      outcome = failed;
      return;
    }

    const result = readDeregisterResult(status, body);
    deregistering = null;
    unjudgedOutcomes = { ...unjudgedOutcomes, [row.id]: result };
    // Also at panel level: a successful deregistration re-reads the census, and
    // the row it was shown on is gone from the answer along with its message.
    outcome = result;
    if (result.ok) {
      disposed = { ...disposed, [row.id]: 'gone' };
      await scanAfterPrune();
      onPruned();
    }
  }

  function openConfirm() {
    if (chosen.length === 0) return;
    stage = 'confirm';
  }

  function cancel() {
    stage = census ? 'reviewing' : 'idle';
  }

  /**
   * Send the confirmed ids and believe only what the response says.
   *
   * A partial batch leaves the panel open with one row per id, so an operator
   * whose cleanup half-worked sees which half. The roster is re-read only when
   * something was actually removed.
   */
  async function prune() {
    stage = 'busy';
    let status = 0;
    let body = null;
    try {
      const resp = await fetch(PRUNE_URL, {
        method: 'POST',
        headers: { 'content-type': 'application/json' },
        body: JSON.stringify({ ids: chosen, delete_data: deleteData }),
      });
      status = resp.status;
      body = await resp.json().catch(() => null);
    } catch (e) {
      stage = 'done';
      outcome = { ok: false, message: `The console could not reach its own prune route: ${e.message}` };
      return;
    }

    const result = readPruneResult(status, body);
    outcome = { ok: result.ok, message: result.message };
    rows = result.rows;
    stage = 'done';
    if ((body?.removed ?? 0) > 0) {
      await scanAfterPrune();
      onPruned();
    }
  }

  /** Re-read the census so the panel shows what is left, not what was found. */
  async function scanAfterPrune() {
    try {
      const resp = await fetch(CENSUS_URL);
      if (resp.ok) census = await resp.json();
    } catch {
      // The prune already happened; a failed re-census must not turn it into a
      // reported failure. The next scan will refresh it.
    }
  }
</script>

<section class="cleanup" aria-labelledby="stale-cleanup-title">
  <div class="head">
    <h3 id="stale-cleanup-title">Stale registrations</h3>
    <button class="scan" onclick={scan} disabled={stage === 'scanning' || stage === 'busy'}>
      {stage === 'scanning' ? 'Checking…' : 'Check for stale registrations'}
    </button>
  </div>
  <p class="lede">
    Registrations whose root directory is gone. trusty-search decides which
    those are. Roots it cannot check are never part of the batch; each one can
    be reviewed and settled on its own.
  </p>

  {#if outcome}
    <p class="outcome" class:bad={!outcome.ok} role="status">{outcome.message}</p>
  {/if}

  {#if rows.length > 0}
    <ul class="rows">
      {#each rows as row (row.id)}
        <li class:bad={!row.ok}>
          <code>{row.id}</code>
          <span>{row.ok ? 'removed' : row.error}</span>
        </li>
      {/each}
    </ul>
  {/if}

  {#if census && stage !== 'scanning'}
    <p class="summary">{censusSummary(census)}</p>

    {#if candidates.length > 0}
      <ul class="candidates">
        {#each candidates as c (c.id)}
          <li>
            <label>
              <input type="checkbox" bind:checked={selected[c.id]} disabled={stage !== 'reviewing'} />
              <code>{c.id}</code>
              <span class="path">{c.root_path}</span>
            </label>
          </li>
        {/each}
      </ul>

      {#if stage === 'confirm' || stage === 'busy'}
        <div class="confirm" role="group" aria-label={pruneConfirmMessage(chosen, deleteData)}>
          <p class="prompt">{pruneConfirmMessage(chosen, deleteData)}</p>
          <ul class="doomed">
            {#each chosen as id (id)}<li><code>{id}</code></li>{/each}
          </ul>
          <div class="buttons">
            <button class="cancel" onclick={cancel} disabled={stage === 'busy'}>Cancel</button>
            <button class="danger" onclick={prune} disabled={stage === 'busy'}>
              {stage === 'busy' ? 'Removing…' : `Remove ${chosen.length}`}
            </button>
          </div>
        </div>
      {:else}
        <label class="opt">
          <input type="checkbox" bind:checked={deleteData} disabled={stage !== 'reviewing'} />
          Delete the on-disk index data too — untick to deregister only and keep the corpus
        </label>
        <button class="danger" onclick={openConfirm} disabled={chosen.length === 0}>
          Remove {chosen.length} selected
        </button>
      {/if}
    {/if}

    {#if unjudged.length > 0}
      <h4 class="unjudged-title">Could not be checked ({unjudged.length})</h4>
      <p class="unjudged-lede">
        Never selected and never swept by the batch above. Review one to see its
        full path and decide: keep it registered, or deregister it.
      </p>
      <ul class="unjudged">
        {#each unjudged as u (u.id)}
          <UnjudgedReview
            row={u}
            disposition={disposed[u.id] ?? 'none'}
            busy={deregistering === u.id}
            outcome={unjudgedOutcomes[u.id] ?? null}
            onKeep={() => keepUnjudged(u.id)}
            onDeregister={() => deregisterUnjudged(u)}
          />
        {/each}
      </ul>
    {/if}
  {/if}
</section>

<style>
  .cleanup {
    margin: 1.5rem 0;
    padding: 1rem;
    background: var(--trusty-card-bg);
    border: 1px solid var(--trusty-border);
    border-radius: 0.5rem;
  }
  .head { display: flex; align-items: center; justify-content: space-between; gap: 0.75rem; }
  h3 { margin: 0; font-size: 1rem; font-weight: 600; color: var(--trusty-text-primary); }
  h4 { font-size: 0.8rem; font-weight: 600; color: var(--trusty-text-secondary); margin: 0.9rem 0 0.35rem; }
  .lede { margin: 0.35rem 0 0.75rem; font-size: 0.8rem; color: var(--trusty-text-secondary); }
  .unjudged-lede { margin: 0 0 0.4rem; font-size: 0.76rem; color: var(--trusty-text-muted); }
  .summary { margin: 0.5rem 0; font-size: 0.85rem; color: var(--trusty-text-primary); font-weight: 600; }

  button { font: inherit; cursor: pointer; border-radius: 0.35rem; border: 1px solid transparent; }
  button:disabled { cursor: default; opacity: 0.6; }
  .scan {
    background: transparent; border-color: var(--trusty-border);
    color: var(--trusty-text-primary); font-size: 0.78rem; padding: 0.25rem 0.6rem;
  }
  .danger {
    background: var(--trusty-danger); color: #fff;
    font-size: 0.78rem; font-weight: 600; padding: 0.25rem 0.7rem;
  }
  .cancel {
    background: transparent; border-color: var(--trusty-border);
    color: var(--trusty-text-secondary); font-size: 0.78rem; padding: 0.25rem 0.7rem;
  }

  ul { list-style: none; margin: 0.4rem 0; padding: 0; }
  li { display: flex; align-items: baseline; gap: 0.5rem; font-size: 0.8rem; padding: 0.15rem 0; }
  label { display: flex; align-items: baseline; gap: 0.5rem; cursor: pointer; }
  .opt {
    display: flex; align-items: center; gap: 0.35rem; margin: 0.6rem 0;
    font-size: 0.78rem; color: var(--trusty-text-secondary); cursor: pointer;
  }
  .path { color: var(--trusty-text-secondary); overflow-wrap: anywhere; }
  code {
    font-family: 'JetBrains Mono', monospace; font-size: 0.75rem;
    background: var(--trusty-surface-raised); padding: 0.1rem 0.35rem; border-radius: 0.25rem;
  }

  .confirm {
    display: flex; flex-direction: column; gap: 0.4rem; margin-top: 0.6rem; padding: 0.7rem;
    background: var(--trusty-surface-raised);
    border: 1px solid var(--trusty-danger); border-radius: 0.4rem;
  }
  .prompt { margin: 0; font-size: 0.82rem; font-weight: 600; color: var(--trusty-text-primary); }
  .doomed { max-height: 9rem; overflow-y: auto; }
  .buttons { display: flex; gap: 0.4rem; }

  .outcome { margin: 0.5rem 0; font-size: 0.8rem; color: var(--trusty-success); }
  .outcome.bad { color: var(--trusty-danger); }
  .rows li.bad span { color: var(--trusty-danger); overflow-wrap: anywhere; }
</style>
