/**
 * The console's own details pane, as data (#6908).
 *
 * Why: the Services roster now carries a `trusty-console` row, and the owner's
 * requirement is that selecting it opens the same kind of details view every
 * other service row opens. What that view can honestly show is a smaller set
 * than the brief's wish list: version, uptime, CPU, resident memory, and how
 * many browsers are watching the machine-status stream. Bus status, service
 * connections and message rates have no transport behind them yet — #6862
 * landed the DOC-73 types and #6460 owns the ingest — so this module states
 * that in one line rather than letting the pane grow widgets with nothing
 * feeding them.
 *
 * What: pure functions over the roster row, the `/health` probe and the
 * machine-status snapshot's `sse_client_count`. Every number the pane prints is
 * formatted here, so `ConsoleTab.svelte` is markup and the rules are testable
 * under `node --test`.
 *
 * Three rules this module exists to hold:
 *   - An unmeasured figure is a dash, never a zero. A `null` uptime is a daemon
 *     that did not report one and a `null` stream count is a snapshot that
 *     predates schema 4; rendering either as `0` invents a measurement.
 *   - The CPU and memory graphs are the SAME graphs the Services row draws.
 *     They come from `servicesList.js` rather than a second scale defined here,
 *     because a row and its details pane disagreeing about how busy the console
 *     is would be worse than either number alone.
 *   - What is not built says so once. [`DEFERRED_LINE`] is one sentence, not
 *     three empty panels.
 *
 * Test: `consoleDetails.test.js` — run `node --test src/consoleDetails.test.js`
 * from `crates/trusty-console/ui`.
 */

import { DASH, formatCpu, formatMemory } from './servicesList.js';

/** The roster id the console reports for itself (`detect/console.rs`). */
export const CONSOLE_SERVICE_ID = 'trusty-console';

/**
 * The one line the pane prints where the brief implied three more panels.
 *
 * Kept as a constant so the pane and its test name the same three subsystems —
 * a line that drifts from what is actually missing is worse than no line.
 */
export const DEFERRED_LINE =
  'Bus status, service connections and message rates are not yet available — ' +
  'they wait on the event-bus transport (#6460).';

/** What the stream-count card calls the figure it shows. */
export const STREAM_CARD_LABEL = 'Browser Streams';

const SECONDS_PER_MINUTE = 60;
const SECONDS_PER_HOUR = 60 * SECONDS_PER_MINUTE;
const SECONDS_PER_DAY = 24 * SECONDS_PER_HOUR;

/**
 * A duration in seconds as the two largest units it fills, or the dash.
 *
 * Why two units and not three: `2d 4h` and `4h 12m` are the readings an
 * operator acts on, and appending seconds to a multi-day uptime adds a digit
 * that changes every tick and answers nothing. Under a minute the raw seconds
 * are shown, because a console that has been up for eight seconds is the one
 * case where the exact figure matters.
 * Test: `formatUptime picks the two largest units it fills`, `formatUptime
 * dashes an unreported uptime`.
 */
export function formatUptime(secs) {
  if (typeof secs !== 'number' || !Number.isFinite(secs) || secs < 0) return DASH;
  const whole = Math.floor(secs);
  if (whole >= SECONDS_PER_DAY) {
    const days = Math.floor(whole / SECONDS_PER_DAY);
    return `${days}d ${Math.floor((whole % SECONDS_PER_DAY) / SECONDS_PER_HOUR)}h`;
  }
  if (whole >= SECONDS_PER_HOUR) {
    const hours = Math.floor(whole / SECONDS_PER_HOUR);
    return `${hours}h ${Math.floor((whole % SECONDS_PER_HOUR) / SECONDS_PER_MINUTE)}m`;
  }
  if (whole >= SECONDS_PER_MINUTE) {
    const minutes = Math.floor(whole / SECONDS_PER_MINUTE);
    return `${minutes}m ${whole % SECONDS_PER_MINUTE}s`;
  }
  return `${whole}s`;
}

/**
 * The open browser streams as a bare count, or the dash when none was reported.
 *
 * Zero is a real answer and prints as `0` — a console nobody is watching is a
 * fact. `null` is the absence of an answer, which is the dash.
 * Test: `formatStreamCount prints zero as a real count`, `formatStreamCount
 * dashes an unreported count`.
 */
export function formatStreamCount(count) {
  return typeof count === 'number' && Number.isFinite(count) && count >= 0
    ? String(Math.floor(count))
    : DASH;
}

/**
 * The console's own row out of the list `serviceRows` built, or `null`.
 *
 * Reusing that row rather than re-deriving one is what keeps the pane's %CPU
 * and MEM figures identical to the roster line the operator clicked.
 * Test: `consoleRow finds the console among the roster rows`.
 */
export function consoleRow(rows) {
  return (rows ?? []).find((row) => row?.id === CONSOLE_SERVICE_ID) ?? null;
}

/**
 * The four stat cards the pane's top row renders, in display order.
 *
 * Why this order: uptime and the watcher count are the two figures with no
 * graph, so they lead; CPU and memory follow as the last two cards, which is
 * what puts their bottom-edge graphs side by side in the four-column grid —
 * the same pairing, drawn the same way, as the Services row this pane opened
 * from.
 *
 * What: `{ key, label, value, meta, graph }`, where `graph` names the row spec
 * the card's footer draws (`'cpu'`, `'memory'`, or `null` for a card with no
 * graph). Every value is already formatted; the pane prints them as given.
 * Test: `consoleDetailCards leads with uptime and the stream count`,
 * `consoleDetailCards puts the two graphed cards last`, `consoleDetailCards
 * dashes every figure nothing reported`.
 *
 * @param {{ row?: object|null, uptimeSecs?: number|null, sseClientCount?: number|null }} input
 */
export function consoleDetailCards({ row = null, uptimeSecs = null, sseClientCount = null } = {}) {
  return [
    {
      key: 'uptime',
      label: 'Uptime',
      value: formatUptime(uptimeSecs),
      meta: 'since this process started serving',
      graph: null,
    },
    {
      key: 'streams',
      label: STREAM_CARD_LABEL,
      value: formatStreamCount(sseClientCount),
      // Named for what it counts. The figure is open machine-status SSE
      // responses, and calling it "connections" would read as the service
      // connections that do not exist yet.
      meta: 'open machine-status streams',
      graph: null,
    },
    {
      key: 'cpu',
      label: 'CPU',
      // The row already applied the dash-not-zero rule; `formatCpu` is only
      // reached when the pane is opened before the roster answered.
      value: row?.cpuLabel ?? formatCpu(null),
      meta: 'sampled once per second',
      graph: 'cpu',
    },
    {
      key: 'memory',
      label: 'Memory',
      value: row?.memoryLabel ?? formatMemory(null),
      meta: 'resident set size',
      graph: 'memory',
    },
  ];
}

/**
 * The pane's heading, naming the build the operator is looking at.
 *
 * An unknown version drops the suffix rather than printing a dash inside a
 * title — the same rule `describeConsole` uses for the header lockup.
 * Test: `consoleHeading names the version when one is known`.
 */
export function consoleHeading(version) {
  return version ? `Trusty Console · v${version}` : 'Trusty Console';
}
