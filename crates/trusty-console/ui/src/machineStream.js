/**
 * The home page's single machine-status `EventSource` client (#6642).
 *
 * Why: every graph on the Overview tab — the four host stat cards and one row
 * per service — reads the same 1 Hz history. One connection feeding one store
 * is the only arrangement where the bars on two different cards are guaranteed
 * to be the same second; N components each opening their own stream would drift
 * against each other and hold N sockets open against a daemon that fans the
 * identical bytes out to all of them.
 *
 * What: a state machine over `GET /api/console/machine-status/stream`
 * (`crates/trusty-console/src/routes/machine_history.rs`). The reducers below
 * are pure functions over a plain object, and the connection wrapper takes its
 * `EventSource`, `fetch` and timer as parameters, so every branch — seeding,
 * capping, resync, a malformed frame — is testable under `node --test` with no
 * browser. `machineStream.svelte.js` does not exist on purpose: keeping this
 * file free of runes is what makes it testable.
 *
 * Four rules this module exists to hold:
 *   - A malformed frame is dropped, never fatal. The console and the daemon can
 *     be different builds, and one unparseable `data:` line must not take the
 *     other 599 samples off the screen.
 *   - `lagged` means the browser has a hole it cannot see. The only correct
 *     response is to re-fetch the snapshot, not to carry on appending onto a
 *     window that silently skipped time.
 *   - Capacity is the server's, not a constant here. The snapshot reports
 *     `sample_capacity` and `service_sample_capacity`; [`DEFAULT_CAPACITY`] is
 *     only the value used before the first snapshot arrives.
 *   - `cpu_pct` and `rss_bytes` may be `null`, and `null` is not `0`. Each
 *     survives into its series so the graph draws a gap rather than an idle bar.
 *
 * Test: `machineStream.test.js` — run `node --test src/machineStream.test.js`
 * from `crates/trusty-console/ui`.
 */

/** The snapshot route, also used to resync after a `lagged` event. */
export const HISTORY_URL = '/api/console/machine-status/history';

/** The SSE route. */
export const STREAM_URL = '/api/console/machine-status/stream';

/**
 * The `schema_version` this client was written against (`HistorySnapshot`).
 *
 * A mismatch is logged once and then ignored: the payload is additive, so an
 * older or newer daemon still carries the fields the graphs read, and blanking
 * the page over a version integer would be worse than rendering what parsed.
 */
export const EXPECTED_SCHEMA_VERSION = 3;

/** Ring size assumed before the first snapshot states the server's. */
export const DEFAULT_CAPACITY = 600;

/** Backoff between reconnect attempts, milliseconds; the last value repeats. */
export const RECONNECT_DELAYS_MS = [1_000, 2_000, 5_000, 10_000, 30_000];

/**
 * The store every consumer renders from.
 *
 * `samples` are whole `HostMetrics` objects oldest-first; `serviceSamples` maps
 * a service id to its own oldest-first `{ id, status, cpu_pct, rss_bytes }` list.
 * `connected` drives nothing but a diagnostic today — it exists so a future
 * "stream offline" notice does not need a second source of truth.
 */
export function initialState() {
  return {
    samples: [],
    serviceSamples: {},
    capacity: DEFAULT_CAPACITY,
    serviceCapacity: DEFAULT_CAPACITY,
    sampleIntervalSecs: 1,
    connected: false,
    seeded: false,
  };
}

/** Keep at most `capacity` entries, dropping from the oldest end. */
function capped(list, capacity) {
  const limit = Number.isFinite(capacity) && capacity > 0 ? capacity : DEFAULT_CAPACITY;
  return list.length <= limit ? list : list.slice(list.length - limit);
}

/** A positive integer capacity from the payload, or the fallback. */
function readCapacity(value, fallback) {
  return Number.isFinite(value) && value > 0 ? Math.floor(value) : fallback;
}

/**
 * Seed the whole window from a `history` snapshot.
 *
 * Replaces rather than merges: the snapshot IS the server's window, so anything
 * this client is holding that the snapshot does not carry has aged out. That is
 * also what makes it the correct answer to a `lagged` event.
 */
export function applyHistory(state, snapshot) {
  if (!snapshot || typeof snapshot !== 'object') return state;
  const capacity = readCapacity(snapshot.sample_capacity, state.capacity);
  const serviceCapacity = readCapacity(snapshot.service_sample_capacity, state.serviceCapacity);

  const serviceSamples = {};
  const incoming = snapshot.service_samples;
  if (incoming && typeof incoming === 'object') {
    for (const [id, list] of Object.entries(incoming)) {
      if (Array.isArray(list)) serviceSamples[id] = capped(list.slice(), serviceCapacity);
    }
  }

  return {
    ...state,
    samples: Array.isArray(snapshot.samples) ? capped(snapshot.samples.slice(), capacity) : [],
    serviceSamples,
    capacity,
    serviceCapacity,
    sampleIntervalSecs: Number.isFinite(snapshot.sample_interval_secs)
      ? snapshot.sample_interval_secs
      : state.sampleIntervalSecs,
    seeded: true,
  };
}

/** Append one host sample, dropping the oldest once the ring is full. */
export function applySample(state, metrics) {
  if (!metrics || typeof metrics !== 'object') return state;
  return { ...state, samples: capped([...state.samples, metrics], state.capacity) };
}

/**
 * Append one tick's whole service roster.
 *
 * A service that appears for the first time gets a series starting now rather
 * than being back-filled with zeros — a graph that claims history it never
 * observed is a lie the operator cannot detect.
 */
export function applyServices(state, batch) {
  const rows = batch?.services;
  if (!Array.isArray(rows)) return state;
  const next = { ...state.serviceSamples };
  for (const row of rows) {
    if (!row || typeof row.id !== 'string') continue;
    next[row.id] = capped([...(next[row.id] ?? []), row], state.serviceCapacity);
  }
  return { ...state, serviceSamples: next };
}

/**
 * Parse one SSE `data:` payload, or `null` when it is not usable JSON.
 *
 * Returning `null` rather than throwing is the whole point: the caller drops
 * the frame and keeps the connection. An array is rejected alongside a scalar:
 * every one of the five event payloads is a JSON object, so an array here is a
 * frame this build cannot read, not an empty one.
 */
export function parseFrame(data) {
  if (typeof data !== 'string' || data === '') return null;
  try {
    const parsed = JSON.parse(data);
    return parsed && typeof parsed === 'object' && !Array.isArray(parsed) ? parsed : null;
  } catch {
    return null;
  }
}

/** The delay before reconnect attempt `n` (0-based); the last entry repeats. */
export function reconnectDelay(attempt, delays = RECONNECT_DELAYS_MS) {
  const index = Math.min(Math.max(attempt, 0), delays.length - 1);
  return delays[index];
}

/**
 * Open the stream and keep `onState` fed with the current window.
 *
 * Why the injected `EventSourceImpl` / `fetchImpl` / timer: `node --test` has no
 * `EventSource`, and the four behaviours worth testing — seeding, capping,
 * resync-on-lagged, survive-a-malformed-frame — are all connection-level. A
 * module that could only be exercised in a browser would have none of them
 * covered.
 *
 * @returns {{ start: () => void, stop: () => void, getState: () => object }}
 */
export function createMachineStream({
  onState = () => {},
  EventSourceImpl = typeof EventSource === 'undefined' ? null : EventSource,
  fetchImpl = typeof fetch === 'undefined' ? null : fetch,
  streamUrl = STREAM_URL,
  historyUrl = HISTORY_URL,
  setTimeoutImpl = setTimeout,
  clearTimeoutImpl = clearTimeout,
  logger = console,
} = {}) {
  let state = initialState();
  let source = null;
  let retryTimer = null;
  let attempt = 0;
  let stopped = false;
  let warnedSchema = false;

  function publish(next) {
    state = next;
    onState(state);
  }

  /** Log an unexpected `schema_version` once per page, then carry on. */
  function checkSchema(snapshot) {
    const version = snapshot?.schema_version;
    if (warnedSchema || version === EXPECTED_SCHEMA_VERSION) return;
    warnedSchema = true;
    logger?.warn?.(
      `machine-status history schema_version ${version} != ${EXPECTED_SCHEMA_VERSION}; ` +
        'rendering the fields this build understands',
    );
  }

  /** Re-read the snapshot; used on `lagged`, where appending would lie. */
  async function resync() {
    if (!fetchImpl) return;
    try {
      const resp = await fetchImpl(historyUrl);
      if (!resp?.ok) return;
      const snapshot = await resp.json();
      checkSchema(snapshot);
      publish(applyHistory(state, snapshot));
    } catch {
      // A failed resync leaves the window as it was. The next `history` frame
      // on reconnect fixes it, and an error box for a transient fetch would be
      // noisier than the gap it reports.
    }
  }

  function connect() {
    if (stopped || !EventSourceImpl) return;
    source = new EventSourceImpl(streamUrl);

    source.addEventListener('open', () => {
      attempt = 0;
      publish({ ...state, connected: true });
    });

    source.addEventListener('history', (event) => {
      const snapshot = parseFrame(event?.data);
      if (!snapshot) return;
      checkSchema(snapshot);
      publish(applyHistory(state, snapshot));
    });

    source.addEventListener('sample', (event) => {
      const metrics = parseFrame(event?.data);
      if (metrics) publish(applySample(state, metrics));
    });

    source.addEventListener('services', (event) => {
      const batch = parseFrame(event?.data);
      if (batch) publish(applyServices(state, batch));
    });

    source.addEventListener('lagged', () => {
      void resync();
    });

    source.addEventListener('error', () => {
      // The browser's own EventSource reconnects, but only on its schedule and
      // without re-seeding; closing and redialling ourselves is what guarantees
      // the fresh `history` frame that closes the gap.
      close();
      if (stopped) return;
      publish({ ...state, connected: false });
      const delay = reconnectDelay(attempt);
      attempt += 1;
      retryTimer = setTimeoutImpl(connect, delay);
    });
  }

  function close() {
    try {
      source?.close?.();
    } catch {
      // A close on an already-dead source is not worth reporting.
    }
    source = null;
  }

  return {
    start() {
      stopped = false;
      connect();
    },
    stop() {
      stopped = true;
      if (retryTimer !== null) clearTimeoutImpl(retryTimer);
      retryTimer = null;
      close();
    },
    getState: () => state,
  };
}
