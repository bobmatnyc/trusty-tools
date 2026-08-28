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
   * Test: the pure decisions live in `cleanupFlow.js` and are covered by
   * `cleanupFlow.test.js`; the batch route's per-item outcomes are covered by
   * `routes::cleanup` in the Rust crate.
   */
  import {
    CENSUS_URL,
    PRUNE_URL,
    censusSummary,
    pruneConfirmMessage,
    readPruneResult,
    selectableOrphans,
    unjudgedRows,
  } from './cleanupFlow.js';

  /** @type {{ onPruned: () => void }} */
  let { onPruned } = $props();

  /** 'idle' | 'scanning' | 'reviewing' | 'confirm' | 'busy' | 'done' */
  let stage = $state('idle');
  /** The daemon's census body, or null before one has been fetched. */
  let census = $state(null);
  /** Ids the operator has ticked, as a plain object so Svelte tracks writes. */
  let selected = $state({});
  /** Whether the prune also destroys each index's on-disk corpus. */
  let deleteData = $state(false);
  /** The message from the last completed attempt, or a fetch failure. */
  let outcome = $state(null);
  /** Per-id rows from the last prune. */
  let rows = $state([]);

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
    selected = Object.fromEntries(selectableOrphans(census).map((c) => [c.id, true]));
    stage = 'reviewing';
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
    those are; roots it cannot check are listed but never removed.
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
          Also delete the on-disk index data (otherwise only deregistered)
        </label>
        <button class="danger" onclick={openConfirm} disabled={chosen.length === 0}>
          Remove {chosen.length} selected
        </button>
      {/if}
    {/if}

    {#if unjudged.length > 0}
      <h4 class="unjudged-title">Could not be checked ({unjudged.length})</h4>
      <ul class="unjudged">
        {#each unjudged as u (u.id)}
          <li>
            <code>{u.id}</code>
            <span class="path">{u.root_path}</span>
            <span class="reason">{u.reason}</span>
          </li>
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
  .reason { color: var(--trusty-text-muted); font-style: italic; overflow-wrap: anywhere; }
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
