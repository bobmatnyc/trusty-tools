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
