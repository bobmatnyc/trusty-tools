<script>
  /*
   * Why: the Indexes table says "ready" or "error" for a whole collection, which
   * is three lanes collapsed into one word. An operator triaging a slow index
   * needs to know WHICH lane is behind — a corpus whose BM25 is ready and whose
   * embedding stage has 40k chunks to go looks identical to a finished one from
   * the table alone (#6524). This row opens that up: the three stages with their
   * counters, the pause toggle for the one stage that can be paused, and the
   * file changes the watcher has seen.
   * What: an expandable detail panel for one index. It polls
   * `GET /indexes/{id}/status` while open (the table's own refresh does not
   * carry `stages`), opens the `file-events` SSE feed on mount and closes it on
   * destroy, and drives the pause toggle optimistically — the badge flips
   * immediately and rolls back if the POST fails, because an embedder under load
   * can take a moment to answer and a toggle that does nothing for a second
   * reads as broken.
   * Test: `indexingPipeline.test.js` covers every mapping this renders; the live
   * check is expanding a row against a real daemon and watching the semantic
   * badge turn PAUSED after the toggle.
   */
  import { onMount, onDestroy } from 'svelte';
  import { api, fileEventsStreamUrl } from '../api.js';
  import Badge from './Badge.svelte';
  import {
    STAGES,
    stageBadge,
    stageMeta,
    isEmbeddingPaused,
    fileEventRow,
    pushFeedRow
  } from '../indexingPipeline.js';

  let { id } = $props();

  /** The most recent `GET /indexes/{id}/status` body, or null before the first. */
  let status = $state(null);
  let statusError = $state(null);
  /** Feed rows, newest first, bounded by `pushFeedRow`. */
  let feed = $state([]);
  let feedError = $state(null);
  let toggling = $state(false);
  let toggleError = $state(null);

  /*
   * Why an override rather than reading the status directly: the POST answers
   * before the next poll runs, so the badge would snap back to its old value for
   * up to one poll interval. `pausedOverride` holds the optimistic value until a
   * status arrives that agrees with it.
   */
  let pausedOverride = $state(null);

  let reportedPaused = $derived(isEmbeddingPaused(status));
  let paused = $derived(pausedOverride === null ? reportedPaused : pausedOverride);

  /** How often the open row re-reads its status. */
  const POLL_MS = 15_000;
  let timer = null;
  let source = null;

  /**
   * Read this index's status, keeping the previous one on a transient failure.
   *
   * A failed poll must not blank the badges — a daemon restarting mid-poll would
   * otherwise clear a panel the operator is reading.
   */
  async function refreshStatus() {
    try {
      const next = await api.indexStatus(id);
      status = next;
      statusError = null;
      // Let the optimistic value go once the daemon agrees with it.
      if (pausedOverride !== null && isEmbeddingPaused(next) === pausedOverride) {
        pausedOverride = null;
      }
    } catch (err) {
      statusError = err.message || String(err);
    }
  }

  /**
   * Flip the embedding pause, optimistically.
   *
   * On failure the badge goes back to what the daemon last reported and the
   * error is shown beside the toggle — silently reverting would look like a
   * click that never registered.
   */
  async function togglePause() {
    if (toggling) return;
    const next = !paused;
    toggling = true;
    toggleError = null;
    pausedOverride = next;
    try {
      const res = next ? await api.pauseEmbedding(id) : await api.resumeEmbedding(id);
      // Trust the daemon's answer over the optimistic guess.
      if (typeof res?.embedding_paused === 'boolean') {
        pausedOverride = res.embedding_paused;
      }
      await refreshStatus();
    } catch (err) {
      pausedOverride = null;
      toggleError = `${next ? 'Pause' : 'Resume'} failed: ${err.message || err}`;
    } finally {
      toggling = false;
    }
  }

  /** Open the file-change feed. Replays the daemon's ring, then streams live. */
  function openFeed() {
    closeFeed();
    try {
      const src = new EventSource(fileEventsStreamUrl(id));
      src.onmessage = (ev) => {
        let event;
        try {
          event = JSON.parse(ev.data);
        } catch {
          return;
        }
        feed = pushFeedRow(feed, fileEventRow(event));
        feedError = null;
      };
      src.onerror = () => {
        // EventSource reconnects on its own; say so rather than showing an
        // empty feed that reads as "nothing has changed".
        feedError = 'Feed disconnected — reconnecting…';
      };
      source = src;
    } catch (err) {
      feedError = `Feed unavailable: ${err.message || err}`;
    }
  }

  function closeFeed() {
    if (source) {
      source.close();
      source = null;
    }
  }

  onMount(() => {
    refreshStatus();
    timer = setInterval(refreshStatus, POLL_MS);
    openFeed();
  });

  onDestroy(() => {
    if (timer) clearInterval(timer);
    timer = null;
    closeFeed();
  });
</script>

<div class="pipeline">
  <section class="stages">
    <h3 class="section-title">Pipeline</h3>
    <div class="stage-grid">
      {#each STAGES as stage (stage.key)}
        {@const lane = status?.stages?.[stage.key]}
        {@const badge = stageBadge(lane)}
        <div class="stage">
          <span class="stage-name">{stage.label}</span>
          <Badge tone={badge.tone} spinner={badge.spinner}>{badge.label}</Badge>
          {#if stageMeta(lane)}
            <span class="stage-meta">{stageMeta(lane)}</span>
          {/if}
          {#if lane?.failure}
            <span class="stage-failure" title={lane.failure}>{lane.failure}</span>
          {/if}
        </div>
      {/each}
    </div>
    {#if statusError}
      <p class="note note-error">Status unavailable: {statusError}</p>
    {/if}
  </section>

  <section class="control">
    <h3 class="section-title">Embedding</h3>
    <button
      class="btn btn-sm"
      class:btn-warning={!paused}
      disabled={toggling}
      onclick={togglePause}
    >
      {#if toggling}Working…{:else if paused}Resume embedding{:else}Pause embedding{/if}
    </button>
    <p class="note">
      Pausing stops the embedding stage only — lexical search, the knowledge
      graph and the file watcher keep running. The pause is held in memory and
      clears if the daemon restarts.
    </p>
    {#if toggleError}
      <p class="note note-error">{toggleError}</p>
    {/if}
  </section>

  <section class="feed">
    <h3 class="section-title">Recent file changes</h3>
    {#if feedError}
      <p class="note note-error">{feedError}</p>
    {/if}
    {#if feed.length === 0}
      <p class="note">No changes recorded since the daemon started.</p>
    {:else}
      <ul class="feed-list">
        {#each feed as row, i (`${row.at ?? 'n'}-${row.path}-${i}`)}
          <li class="feed-row">
            <Badge tone={row.tone}>{row.kind}</Badge>
            <span class="feed-path" title={row.path}>{row.path}</span>
            <span class="feed-when">{row.when}</span>
          </li>
        {/each}
      </ul>
    {/if}
  </section>
</div>

<style>
  .pipeline {
    display: grid;
    grid-template-columns: minmax(240px, 1.2fr) minmax(200px, 0.8fr) minmax(260px, 1.4fr);
    gap: var(--trusty-space-5);
    padding: var(--trusty-space-4) var(--trusty-space-5);
    background: var(--trusty-surface-raised);
    border-top: 1px solid var(--trusty-border);
  }
  @media (max-width: 900px) {
    .pipeline {
      grid-template-columns: 1fr;
    }
  }
  .section-title {
    font-size: var(--trusty-fs-xs);
    text-transform: uppercase;
    letter-spacing: 0.06em;
    color: var(--trusty-text-muted);
    font-weight: 600;
    margin: 0 0 var(--trusty-space-3) 0;
  }
  .stage-grid {
    display: flex;
    flex-direction: column;
    gap: var(--trusty-space-2);
  }
  .stage {
    display: flex;
    align-items: center;
    gap: var(--trusty-space-2);
    flex-wrap: wrap;
  }
  .stage-name {
    font-size: var(--trusty-fs-sm);
    font-weight: 600;
    min-width: 72px;
  }
  .stage-meta {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    font-variant-numeric: tabular-nums;
  }
  .stage-failure {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-danger);
    max-width: 100%;
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .note {
    font-size: var(--trusty-fs-xs);
    color: var(--trusty-text-muted);
    margin: var(--trusty-space-2) 0 0 0;
    line-height: 1.5;
  }
  .note-error {
    color: var(--trusty-danger);
  }
  .btn-warning {
    border-color: var(--trusty-warning);
    color: var(--trusty-warning);
  }
  .feed-list {
    list-style: none;
    margin: 0;
    padding: 0;
    max-height: 220px;
    overflow-y: auto;
    display: flex;
    flex-direction: column;
    gap: 4px;
  }
  .feed-row {
    display: grid;
    grid-template-columns: auto 1fr auto;
    align-items: center;
    gap: var(--trusty-space-2);
    font-size: var(--trusty-fs-xs);
  }
  .feed-path {
    font-family: var(--trusty-font-mono, ui-monospace, monospace);
    overflow: hidden;
    text-overflow: ellipsis;
    white-space: nowrap;
  }
  .feed-when {
    color: var(--trusty-text-muted);
    white-space: nowrap;
    font-variant-numeric: tabular-nums;
  }
</style>
