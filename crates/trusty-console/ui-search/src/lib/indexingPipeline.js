/**
 * The per-collection indexing pipeline, as data (#6524).
 *
 * Why: the pipeline row renders three stage badges, a pause toggle and a
 * file-change feed, and every one of those is a mapping from a daemon payload to
 * something with a colour. Keeping the mappings here rather than inside the
 * component makes them testable without a DOM, and makes the one case that is
 * easy to get wrong explicit: a PAUSED embedding stage is still `in_progress` on
 * the wire — the daemon deliberately added no sixth `StageStatus` variant — so a
 * component branching on `status` alone shows a spinner against a stage that is
 * stopped, which is the opposite of what the operator needs to see.
 *
 * What: pure functions over the `stages` object of `GET /indexes/{id}/status`
 * and over one `file_events` frame. No fetch, no DOM, and `now` is a parameter
 * so relative times are deterministic under test.
 *
 * #6689 adds the lane-health block at the foot of this file. It exists because
 * the same trap has a second, worse form: a semantic stage that says `ready`
 * over an EMPTY vector store. `stageBadge` cannot see that — nothing in `stages`
 * carries it — so the health functions read `search_capabilities` and
 * `semantic_coverage` alongside the stage rather than the stage alone.
 *
 * Test: `indexingPipeline.test.js`.
 */

/** The three lanes, in pipeline order, with the labels the row shows. */
export const STAGES = [
  { key: 'lexical', label: 'Lexical' },
  { key: 'semantic', label: 'Semantic' },
  { key: 'graph', label: 'Graph' }
];

/**
 * Tone and text for one stage badge.
 *
 * `paused` beats everything: an embedding stage parked by an operator reports
 * `in_progress` with `paused: true`, and showing it as running would be a lie
 * about the one thing this row exists to make visible. Only `semantic` is ever
 * paused — the daemon pauses embedding and nothing else — but the override is
 * written against the flag rather than against the stage name, so a stage that
 * gains a pause later needs no change here.
 *
 * An absent stage (a daemon predating the field, or a status call that failed)
 * is `unknown`/muted rather than a guess.
 *
 * @param {object|null|undefined} stage  One entry of `status.stages`
 * @returns {{ tone: string, label: string, spinner: boolean }}
 */
export function stageBadge(stage) {
  if (!stage || typeof stage !== 'object') {
    return { tone: 'muted', label: 'unknown', spinner: false };
  }
  if (stage.paused === true) {
    return { tone: 'warning', label: 'PAUSED', spinner: false };
  }
  switch (stage.status) {
    case 'ready':
      return { tone: 'success', label: 'Ready', spinner: false };
    case 'in_progress':
      return { tone: 'info', label: 'Working', spinner: true };
    case 'failed':
      return { tone: 'danger', label: 'Failed', spinner: false };
    case 'skipped':
      return { tone: 'muted', label: 'Skipped', spinner: false };
    case 'pending':
      return { tone: '', label: 'Pending', spinner: false };
    default:
      return { tone: 'muted', label: 'unknown', spinner: false };
  }
}

/**
 * The counters line under one stage badge, or `''` when the stage has none.
 *
 * Every counter is optional on the wire — the daemon omits what does not apply
 * to a stage — so this renders only what is present. `embedded/total` is shown
 * as a fraction because that pair is the progress an operator watches while a
 * pause decision is in front of them; `files` and `chunks` stand alone.
 *
 * @param {object|null|undefined} stage  One entry of `status.stages`
 * @returns {string}
 */
export function stageMeta(stage) {
  if (!stage || typeof stage !== 'object') return '';
  const parts = [];
  if (typeof stage.files === 'number') parts.push(`${stage.files} files`);
  if (typeof stage.chunks === 'number') parts.push(`${stage.chunks} chunks`);
  if (typeof stage.embedded === 'number') {
    parts.push(
      typeof stage.total === 'number'
        ? `${stage.embedded}/${stage.total} embedded`
        : `${stage.embedded} embedded`
    );
  } else if (typeof stage.total === 'number') {
    parts.push(`${stage.total} to embed`);
  }
  return parts.join(' · ');
}

/**
 * Read the embedding-pause flag out of a status payload.
 *
 * The flag lives on `stages.semantic.paused` and nowhere else. Returning a
 * strict boolean keeps the toggle from rendering an indeterminate state against
 * a daemon that has not reported yet.
 *
 * @param {object|null|undefined} status  A `GET /indexes/{id}/status` body
 * @returns {boolean}
 */
export function isEmbeddingPaused(status) {
  return status?.stages?.semantic?.paused === true;
}

/** Tone for one file-event kind's badge. */
export function eventKindTone(kind) {
  switch (kind) {
    case 'modified':
      return 'info';
    case 'removed':
      return 'danger';
    case 'rescan':
      return 'warning';
    default:
      return 'muted';
  }
}

const MINUTE = 60_000;
const HOUR = 60 * MINUTE;
const DAY = 24 * HOUR;

/**
 * How long ago something happened, in the glanceable form the feed wants.
 *
 * `at` is Unix MILLISECONDS — the unit the daemon sends — and so is `now`. A
 * negative age is clock skew between the daemon and the browser, not a future
 * event, so it reads `just now` rather than as a negative number.
 *
 * @param {number} at   Unix milliseconds
 * @param {number} now  Unix milliseconds, injected for deterministic tests
 * @returns {string}
 */
export function relativeTime(at, now = Date.now()) {
  if (typeof at !== 'number' || !Number.isFinite(at)) return '—';
  const age = now - at;
  if (age < MINUTE) return 'just now';
  if (age < HOUR) return `${Math.floor(age / MINUTE)}m ago`;
  if (age < DAY) return `${Math.floor(age / HOUR)}h ago`;
  return `${Math.floor(age / DAY)}d ago`;
}

/**
 * Turn one raw feed frame into the row the list renders.
 *
 * Two frame shapes arrive on this stream and only one is a file change: the
 * daemon emits a `{ type: 'lag', skipped: N }` notice when a consumer falls
 * behind the broadcast channel, and a `{ type: 'error', … }` frame when the
 * bridge's own stream breaks. Rendering either as a file change would put a row
 * with an empty path in the feed, so they are mapped to their own kinds and
 * carry the count or message as the path text.
 *
 * A `rescan` names no single file — the daemon sends `.` — so it renders as the
 * sentence it stands for rather than as a mysterious dot.
 *
 * @param {object} event  One parsed SSE frame
 * @param {number} now    Unix milliseconds, injected for deterministic tests
 * @returns {{ path: string, kind: string, tone: string, when: string, at: number|null }}
 */
export function fileEventRow(event, now = Date.now()) {
  if (event?.type === 'lag') {
    const skipped = typeof event.skipped === 'number' ? event.skipped : '?';
    return {
      path: `${skipped} change${skipped === 1 ? '' : 's'} not shown — the feed fell behind`,
      kind: 'lag',
      tone: 'warning',
      when: '',
      at: null
    };
  }
  if (event?.type === 'error') {
    return {
      path: event.message || 'the feed failed',
      kind: 'error',
      tone: 'danger',
      when: '',
      at: null
    };
  }
  const at = typeof event?.at_unix_ms === 'number' ? event.at_unix_ms : null;
  const kind = typeof event?.kind === 'string' ? event.kind : 'unknown';
  return {
    path: kind === 'rescan' ? 'the whole tree was rescanned' : event?.path || '—',
    kind,
    tone: eventKindTone(kind),
    when: at === null ? '' : relativeTime(at, now),
    at
  };
}

/** How many feed rows the list keeps — the daemon's own ring size. */
export const FEED_LIMIT = 200;

/**
 * Prepend one row to the feed, newest first, bounded at [`FEED_LIMIT`].
 *
 * Returns a NEW array rather than mutating: the component holds the feed in a
 * rune and a mutation in place would not re-render.
 *
 * @param {Array} rows  The current feed, newest first
 * @param {object} row  The row to add
 * @returns {Array}
 */
export function pushFeedRow(rows, row) {
  return [row, ...(rows ?? [])].slice(0, FEED_LIMIT);
}

// ---------------------------------------------------------------------------
// Lane health (#6689)
// ---------------------------------------------------------------------------

/**
 * The vector-coverage fault an index is in, or `null` when its semantic lane is
 * telling the truth.
 *
 * Why: `stages.semantic.status` is a claim about the last embedding PASS, not a
 * measurement of what the vector store now holds, and the two come apart. Live,
 * `tm-trusty-tools-19` (58,415 chunks) and `tm-trusty-tools-21` (58,167 chunks)
 * report `semantic: "ready"` and advertise `vector` in `search_capabilities`
 * while `semantic_coverage.vectors_present` is `0` — their `hnsw.usearch` files
 * are 112 bytes against ~98 MB for healthy same-size siblings. Every vector
 * query against them returns nothing, and a badge reading `status` alone paints
 * both green. So this reads the three signals TOGETHER: the stage, the
 * capability the daemon advertises off it, and the vectors that actually exist.
 *
 * What: `null`, or `{ code, label, detail }`. Two faults are reported.
 * `empty_vector_store` is the one above — the daemon says vector search works
 * and the store is empty. `count_unreadable` is the daemon's own word for a
 * store that is attached but whose length errored, which `status.rs` classifies
 * as "a real fault, not an absence".
 *
 * An index that was never meant to embed is not at fault for holding no
 * vectors, so `skip_vector`, `lexical_only` and a `skipped` semantic stage all
 * return `null` before anything is measured. So does `no_vector_store`, the
 * daemon's answer for BM25-only, which its own comment calls "the correct,
 * healthy answer". A lane still filling in is not a fault either:
 * `search_capabilities` grows its `vector` entry only once semantic reports
 * ready, so a mid-embedding index holding no vectors yet fails the capability
 * test and is passed over rather than flagged for being unfinished.
 *
 * @param {object|null|undefined} status  A `GET /indexes/{id}/status` body
 * @returns {{ code: string, label: string, detail: string }|null}
 */
export function vectorCoverageFault(status) {
  if (!status || typeof status !== 'object') return null;
  // Never meant to have a vector lane — nothing to be wrong about.
  if (status.skip_vector === true || status.lexical_only === true) return null;
  if (status.stages?.semantic?.status === 'skipped') return null;

  const coverage = status.semantic_coverage;
  if (!coverage || typeof coverage !== 'object') return null;

  const present = coverage.vectors_present;
  if (typeof present !== 'number') {
    // #6689: `count_unreadable` is a store that exists and would not answer.
    // `no_vector_store` is BM25-only and healthy; anything else is a daemon too
    // old to carry the field, which is not evidence of a fault.
    if (coverage.vectors_unavailable_reason === 'count_unreadable') {
      return {
        code: 'count_unreadable',
        label: 'Unreadable',
        detail:
          'A vector store is attached but its size could not be read, so vector search cannot be trusted.'
      };
    }
    return null;
  }

  const chunks =
    typeof coverage.chunk_count === 'number' ? coverage.chunk_count : status.chunk_count;
  const capabilities = Array.isArray(status.search_capabilities) ? status.search_capabilities : [];

  if (present === 0 && typeof chunks === 'number' && chunks > 0 && capabilities.includes('vector')) {
    return {
      code: 'empty_vector_store',
      label: 'Empty',
      detail: `Reports ready and advertises vector search, but holds 0 vectors for ${chunks.toLocaleString()} chunks — every vector query returns nothing.`
    };
  }
  return null;
}

/**
 * The badge for one lane, with the vector-coverage cross-check folded in.
 *
 * Why: [`stageBadge`] maps the stage status and nothing else, which is right for
 * the two lanes whose status is the whole story and wrong for `semantic`, where
 * an empty store hides behind a `ready`. Callers rendering lane health reach for
 * this; `stageBadge` stays the pure status mapping the pause logic and its tests
 * are written against.
 * What: `stageBadge(stage)` for `lexical` and `graph`. For `semantic`, the same
 * unless [`vectorCoverageFault`] finds something, in which case a danger badge
 * carrying the fault's label and a `detail` sentence for the row to show.
 *
 * The fault beats a `PAUSED` label rather than the other way round. In practice
 * the two cannot collide — pausing leaves the stage `in_progress`, so the daemon
 * withdraws the `vector` capability the fault requires — but if they ever did,
 * an empty store is a fact about the index and a pause is a fact about the
 * operator, and this row exists to show the first.
 *
 * @param {string} key  One of `lexical`, `semantic`, `graph`
 * @param {object|null|undefined} status  A `GET /indexes/{id}/status` body
 * @returns {{ tone: string, label: string, spinner: boolean, detail: string }}
 */
export function laneBadge(key, status) {
  const badge = { ...stageBadge(status?.stages?.[key]), detail: '' };
  if (key !== 'semantic') return badge;
  const fault = vectorCoverageFault(status);
  if (!fault) return badge;
  return { tone: 'danger', label: fault.label, spinner: false, detail: fault.detail };
}

/**
 * The whole index's health verdict, as the panel's banner renders it.
 *
 * Why: three lane badges make an operator read three things and combine them.
 * The banner states the answer, and it is the one place that can say "this index
 * is degraded" for a reason no single lane's status string carries.
 * What: `{ tone, label, healthy, faults }`. `healthy` is `null` before a status
 * has arrived, so a panel that has not loaded shows `Unknown` rather than a
 * green it has not earned. `faults` lists every failed lane, carrying the
 * daemon's own failure string when it sent one, plus any vector-coverage fault.
 *
 * @param {object|null|undefined} status  A `GET /indexes/{id}/status` body
 * @returns {{ tone: string, label: string, healthy: boolean|null, faults: Array }}
 */
export function indexHealth(status) {
  if (!status || typeof status !== 'object') {
    return { tone: 'muted', label: 'Unknown', healthy: null, faults: [] };
  }
  const faults = [];
  for (const { key, label } of STAGES) {
    const lane = status.stages?.[key];
    if (lane?.status === 'failed') {
      faults.push({
        lane: key,
        label: `${label} lane failed`,
        detail: lane.failure || 'The daemon reported this lane as failed.'
      });
    }
  }
  const coverage = vectorCoverageFault(status);
  // A semantic lane already reported `failed` above says the same thing once.
  if (coverage && status.stages?.semantic?.status !== 'failed') {
    faults.push({
      lane: 'semantic',
      label: `Semantic lane ${coverage.label.toLowerCase()}`,
      detail: coverage.detail
    });
  }
  return faults.length === 0
    ? { tone: 'success', label: 'Healthy', healthy: true, faults }
    : { tone: 'danger', label: 'Degraded', healthy: false, faults };
}

/**
 * The cumulative vector-coverage line the semantic lane shows, or `''`.
 *
 * `stageMeta`'s `embedded/total` counts THIS boot's pass — #4787's whole point
 * is that a healthy index whose snapshot was already current reports `0` there.
 * This is the cumulative pair beside it, so the row shows both and neither is
 * mistaken for the other.
 *
 * @param {object|null|undefined} status  A `GET /indexes/{id}/status` body
 * @returns {string}
 */
export function coverageMeta(status) {
  const coverage = status?.semantic_coverage;
  if (!coverage || typeof coverage !== 'object') return '';
  const present = coverage.vectors_present;
  if (typeof present !== 'number') {
    return coverage.vectors_unavailable_reason
      ? `vectors: ${coverage.vectors_unavailable_reason}`
      : '';
  }
  const chunks = typeof coverage.chunk_count === 'number' ? coverage.chunk_count : null;
  return chunks === null
    ? `${present.toLocaleString()} vectors stored`
    : `${present.toLocaleString()} / ${chunks.toLocaleString()} vectors stored`;
}
