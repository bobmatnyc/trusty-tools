<script>
  /**
   * Why (#6941): a host accumulates index registrations whose root was wiped
   * (#4255), and clearing them one row at a time — which is all the roster
   * offers — is why 60 of them were still on the owner's machine keeping
   * `warm_boot_degraded` true (#6371). Worse, an allowlist-excluded
   * registration never reaches the daemon's in-memory registry, so it has no row
   * in the Indexes table at all (#6363). This panel is the only place those
   * registrations are visible. It carried the same job in the console until
   * #6923 made that screen display-only; DOC-73 §13 puts every control on the
   * dashboard, so it lives here and calls trusty-search's own API directly
   * rather than a console proxy route.
   *
   * What: a four-stage flow (idle → reviewing → confirm → done). Nothing is
   * deleted before the operator has seen the exact list: the census is fetched,
   * every candidate is listed with its dead root path and what it holds, and the
   * confirm step names the count and the fate of the on-disk data. Roots the
   * daemon declined to judge are listed too and cannot be selected — `cleanup.js`
   * owns that rule, and `UnjudgedReview.svelte` settles one at a time.
   *
   * The candidate list is the DAEMON's census. This view decides nothing about
   * what is stale; it renders what trusty-search reports and sends back the
   * subset the operator confirmed. Two rules it does enforce, both in
   * `cleanup.js` and both tested there:
   *   - eligibility comes from the census's root classification and `chunk_count`,
   *     never from `size_bytes` / `disk_bytes` (#4706 — those read `0` on a
   *     71,433-chunk index);
   *   - a delete is a removal only when the BODY says `ok` and `removed` (#6363),
   *     so an id the daemon did not remove is shown as not removed.
   *
   * Test: `cleanup.test.js` covers every decision above. Live check: open
   * `#/indexes/cleanup`, press Check for stale registrations, and confirm the
   * summary matches `GET /registry/orphans`.
   */
  import { api, deleteIndexReport } from '../api.js';
  import { navigate } from '../router.svelte.js';
  import { refreshIndexes } from '../state.svelte.js';
  import {
    PRUNE_DELETE_DATA_DEFAULT,
    censusSummary,
    chunkCountOf,
    impactPhrase,
    pruneConfirmMessage,
    pruneEligibility,
    readDeleteOutcome,
    selectableOrphans,
    summarizeBatch,
    unjudgedRows
  } from '../cleanup.js';
  import UnjudgedReview from '../components/UnjudgedReview.svelte';

  /** 'idle' | 'scanning' | 'reviewing' | 'confirm' | 'busy' | 'done' */
  let stage = $state('idle');
  /** The daemon's census body, or null before one has been fetched. */
  let census = $state(null);
  /** Ids the operator has ticked, as a plain object so Svelte tracks writes. */
  let selected = $state({});
  /**
   * Per-id chunk count from `GET /indexes/{id}/status`, or `null` when the
   * daemon has not loaded that registration. Never a byte total (#4706).
   * @type {Record<string, number|null>}
   */
  let chunks = $state({});
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
   * `'gone'` is set only after the daemon confirmed the deregistration, so a
   * failed attempt leaves the row exactly where it was and offers it again.
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

  /**
   * Read each candidate's chunk count so the confirm step can say what is lost.
   *
   * A `404` here is expected and is not an error: an allowlist-excluded
   * registration is absent from the live registry, so the daemon has no status
   * for it. That case records `null`, which `impactPhrase` renders as unknown
   * rather than as an empty index.
   */
  async function readChunkCounts(list) {
    const pairs = await Promise.all(
      list.map(async (row) => {
        try {
          return [row.id, chunkCountOf(await api.indexStatus(row.id))];
        } catch {
          return [row.id, null];
        }
      })
    );
    chunks = Object.fromEntries(pairs);
  }

  async function scan() {
    stage = 'scanning';
    outcome = null;
    rows = [];
    try {
      census = await api.registryOrphans();
    } catch (e) {
      census = null;
      stage = 'idle';
      outcome = {
        ok: false,
        message: `Could not read trusty-search's registry census: ${e.message || e}`
      };
      return;
    }
    // Every candidate starts ticked: the operator asked for a cleanup, and the
    // list they are about to confirm is the daemon's own. Untick, not tick, is
    // the exception. This reads `selectableOrphans` and nothing else, which is
    // what keeps an uncheckable row out of every bulk action. A fresh scan also
    // clears the per-row dispositions — they were decisions about the previous
    // census.
    const found = selectableOrphans(census);
    selected = Object.fromEntries(found.map((c) => [c.id, true]));
    disposed = {};
    unjudgedOutcomes = {};
    stage = 'reviewing';
    await readChunkCounts(found);
  }

  /** Re-read the census so the panel shows what is left, not what was found. */
  async function rescanQuietly() {
    try {
      census = await api.registryOrphans();
    } catch {
      // The delete already happened; a failed re-census must not turn it into a
      // reported failure. The next scan refreshes it.
    }
  }

  /**
   * Record that the operator reviewed a row and chose to leave it registered.
   *
   * Nothing is sent: keeping is the default state, so this only stops the panel
   * offering the row as something still to decide.
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
   * Fail-closed: the row is marked `'gone'` only when the daemon's BODY confirmed
   * the removal. A refusal, an unreachable daemon, a body that did not parse, or
   * an answer carrying `removed: false` all leave the row where it was with the
   * reason beside it, so a failed deregistration is never counted as done.
   *
   * `delete_data` is `false` here and the root is pinned: the daemon could not
   * check that root, so there is nothing yet to decide about its data.
   *
   * @param {{id: string, root_path: string}} row The reviewed registration.
   */
  async function deregisterUnjudged(row) {
    // Single-flight per row: the confirm button's `disabled` reads a prop that
    // has not re-rendered when a fast second click lands, and the loser gets the
    // daemon's "unknown index" — so the row reads as failed when it succeeded.
    if (deregistering === row.id) return;
    deregistering = row.id;
    let result;
    try {
      const { status, body } = await deleteIndexReport(row.id, {
        deleteData: false,
        expectedRootPath: row.root_path
      });
      result = readDeleteOutcome(status, body);
    } catch (e) {
      result = { ok: false, message: `Could not reach trusty-search: ${e.message || e}` };
    }
    deregistering = null;
    unjudgedOutcomes = { ...unjudgedOutcomes, [row.id]: result };
    outcome = result;
    if (result.ok) {
      disposed = { ...disposed, [row.id]: 'gone' };
      await rescanQuietly();
      refreshIndexes().catch(() => {});
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
   * Delete each confirmed id in turn and record what the daemon said about it.
   *
   * One request per id, each pinned to the root the census reported: an index id
   * is derived from its root path, so a path wiped and recreated between the
   * census and this click names a DIFFERENT, live index under the same id. The
   * daemon refuses the mismatch under its own teardown lock (#6380), which is
   * the authoritative check — this panel only supplies the expected root.
   *
   * A partial batch leaves one row per id on screen, so an operator whose
   * cleanup half-worked sees which half.
   */
  async function prune() {
    stage = 'busy';
    const attempted = [];
    for (const id of chosen) {
      const row = candidates.find((c) => c.id === id);
      try {
        const { status, body } = await deleteIndexReport(id, {
          deleteData,
          expectedRootPath: row?.root_path ?? null
        });
        const verdict = readDeleteOutcome(status, body);
        attempted.push({ id, ok: verdict.ok, message: verdict.message });
      } catch (e) {
        attempted.push({
          id,
          ok: false,
          message: `Could not reach trusty-search: ${e.message || e}`
        });
      }
    }
    rows = attempted;
    const summary = summarizeBatch(attempted);
    outcome = { ok: summary.ok, message: summary.message };
    stage = 'done';
    if (summary.removed > 0) {
      await rescanQuietly();
      refreshIndexes().catch(() => {});
    }
  }
</script>

<div class="flex-between mb-4">
  <div>
    <h1 class="page-title">Stale registrations</h1>
    <p class="text-muted text-sm subtitle">
      Registrations whose root directory is gone. trusty-search decides which those are — roots
      it cannot check are never part of the batch, and each one can be reviewed and settled on
      its own.
    </p>
  </div>
  <button class="btn btn-sm" onclick={() => navigate('/indexes')}>← Indexes</button>
</div>

<section
  id="stale-cleanup"
  class="card"
  aria-labelledby="stale-cleanup-title"
  data-testid="stale-cleanup-panel"
>
  <div class="card-header flex-between">
    <span id="stale-cleanup-title">Registry census</span>
    <button
      class="btn btn-sm btn-primary"
      onclick={scan}
      disabled={stage === 'scanning' || stage === 'busy'}
    >
      {stage === 'scanning' ? 'Checking…' : 'Check for stale registrations'}
    </button>
  </div>
  <div class="card-body">
    {#if outcome}
      <p class="outcome" class:bad={!outcome.ok} role="status">{outcome.message}</p>
    {/if}

    {#if rows.length > 0}
      <ul class="rows">
        {#each rows as row (row.id)}
          <li class:bad={!row.ok}>
            <code>{row.id}</code>
            <span>{row.message}</span>
          </li>
        {/each}
      </ul>
    {/if}

    {#if !census && stage !== 'scanning'}
      <p class="empty">
        Nothing has been checked yet. The census reads <code>indexes.toml</code> directly, so it
        also lists registrations the warm-boot allowlist excluded — those have no row in the
        Indexes table at all.
      </p>
    {/if}

    {#if census && stage !== 'scanning'}
      <p class="summary">{censusSummary(census)}</p>

      {#if candidates.length > 0}
        <ul class="candidates">
          {#each candidates as c (c.id)}
            {@const gate = pruneEligibility(c, { chunk_count: chunks[c.id] })}
            <li>
              <label>
                <input
                  type="checkbox"
                  bind:checked={selected[c.id]}
                  disabled={stage !== 'reviewing' || !gate.eligible}
                />
                <code>{c.id}</code>
                <span class="path">{c.root_path}</span>
                <span class="impact">{impactPhrase(gate.chunkCount)}</span>
              </label>
            </li>
          {/each}
        </ul>

        {#if stage === 'confirm' || stage === 'busy'}
          <div
            class="confirm"
            role="group"
            aria-label={pruneConfirmMessage(
              chosen,
              deleteData,
              chosen.map((id) => chunks[id] ?? null)
            )}
          >
            <p class="prompt">
              {pruneConfirmMessage(
                chosen,
                deleteData,
                chosen.map((id) => chunks[id] ?? null)
              )}
            </p>
            <ul class="doomed">
              {#each chosen as id (id)}<li><code>{id}</code></li>{/each}
            </ul>
            <div class="buttons">
              <button class="btn btn-sm" onclick={cancel} disabled={stage === 'busy'}>
                Cancel
              </button>
              <button
                class="btn btn-sm btn-danger"
                onclick={prune}
                disabled={stage === 'busy'}
              >
                {stage === 'busy' ? 'Removing…' : `Remove ${chosen.length}`}
              </button>
            </div>
          </div>
        {:else}
          <label class="opt">
            <input type="checkbox" bind:checked={deleteData} disabled={stage !== 'reviewing'} />
            Delete the on-disk index data too — untick to deregister only and keep the corpus
          </label>
          <button
            class="btn btn-sm btn-danger"
            onclick={openConfirm}
            disabled={chosen.length === 0}
          >
            Remove {chosen.length} selected
          </button>
        {/if}
      {/if}

      {#if unjudged.length > 0}
        <h2 class="unjudged-title">Could not be checked ({unjudged.length})</h2>
        <p class="unjudged-lede">
          Never selected and never swept by the batch above. Review one to see its full path and
          decide: keep it registered, or deregister it.
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
  </div>
</section>

<style>
  .page-title {
    font-size: var(--trusty-fs-xl);
    font-weight: 600;
    margin: 0;
    color: var(--trusty-text-primary);
  }
  .subtitle {
    margin: var(--trusty-space-1) 0 0;
    max-width: 62ch;
  }
  h2 {
    font-size: var(--trusty-fs-sm);
    font-weight: 600;
    color: var(--trusty-text-secondary);
    margin: var(--trusty-space-4) 0 var(--trusty-space-1);
  }
  .unjudged-lede {
    margin: 0 0 var(--trusty-space-2);
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
  }
  .summary {
    margin: var(--trusty-space-2) 0;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-text-primary);
    font-weight: 600;
  }
  ul {
    list-style: none;
    margin: var(--trusty-space-2) 0;
    padding: 0;
  }
  ul li {
    display: flex;
    align-items: baseline;
    gap: var(--trusty-space-2);
    font-size: var(--trusty-fs-sm);
    padding: 2px 0;
  }
  /* An unjudged row is rendered by `UnjudgedReview.svelte`, which carries its
     own scoped `li` styling — Svelte's scoping does not reach a child's
     markup, so this list intentionally styles nothing inside `.unjudged`. */
  label {
    display: flex;
    align-items: baseline;
    gap: var(--trusty-space-2);
    cursor: pointer;
    flex-wrap: wrap;
  }
  .opt {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-1);
    margin: var(--trusty-space-3) 0;
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-secondary);
    cursor: pointer;
  }
  .path {
    color: var(--trusty-text-secondary);
    overflow-wrap: anywhere;
  }
  .impact {
    color: var(--trusty-text-muted);
    font-size: var(--trusty-fs-xs);
  }
  code {
    font-family: var(--trusty-mono);
    font-size: var(--trusty-fs-xs);
    background: var(--trusty-surface-raised);
    padding: 1px var(--trusty-space-1);
    border-radius: var(--trusty-radius-sm);
  }
  .confirm {
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-2);
    margin-top: var(--trusty-space-3);
    padding: var(--trusty-space-3);
    background: var(--trusty-surface-raised);
    border: 1px solid var(--trusty-danger);
    border-radius: var(--trusty-radius);
  }
  .prompt {
    margin: 0;
    font-size: var(--trusty-fs-sm);
    font-weight: 600;
    color: var(--trusty-text-primary);
  }
  .doomed {
    max-height: 9rem;
    overflow-y: auto;
  }
  .buttons {
    display: flex;
    gap: var(--trusty-space-2);
  }
  .outcome {
    margin: var(--trusty-space-2) 0;
    font-size: var(--trusty-fs-sm);
    color: var(--trusty-success);
  }
  .outcome.bad {
    color: var(--trusty-danger);
  }
  .rows li.bad span {
    color: var(--trusty-danger);
    overflow-wrap: anywhere;
  }
</style>
