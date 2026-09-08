/*
 * The activity feed's ingest path, with no Svelte in it (#6155).
 *
 * Why: `ActivityFeed.svelte` wrote reactive state once per SSE frame. Measured
 * on the scratch console against the live daemon at 200 events/s, that produced
 * a single 46,420 ms main-thread task, left 97% of a 69 s window stalled, and
 * completed 1 of ~69 one-per-second `/health` polls — so `/health` and
 * `/api/v1/palaces` hit `api.js`'s 35 s abort while curl answered the same
 * endpoints in 19 ms. Collapsing the feed (rendering removed, state churn kept)
 * cut the worst stall to 9,114 ms and still stalled 57%, so BOTH the render and
 * the per-frame state write had to go. Batching is what removes both at once:
 * one reactive write per window instead of one per frame.
 * What: a queue that accepts frames at any rate and hands them to a sink at
 * most once per [`BATCH_WINDOW_MS`], newest-first, never more than [`MAX_EVENTS`]
 * of them — plus the two pure list reducers the component applies to its state.
 * Extracted rather than inlined so the cap and the batch count are assertable
 * without mounting a component or running a browser.
 * Test: `src/lib/feed-buffer.test.js`.
 */

/**
 * How many rows the feed retains, live and history together.
 *
 * The persistent `/api/v1/activity` endpoint owns the real archive (49,551 rows
 * on the machine this was measured on); this buffer is a live tail, and an
 * uncapped one is a leak that grows for as long as the tab stays open.
 */
export const MAX_EVENTS = 500;

/**
 * How long frames accumulate before one reactive write releases them.
 *
 * Why 250 ms: it bounds writes at 4/s no matter the inbound rate, which is
 * below the rate at which a 500-row keyed `{#each}` can reconcile, and it is
 * short enough that the feed still reads as live.
 */
export const BATCH_WINDOW_MS = 250;

/**
 * How many rows one flush may add to the rendered list.
 *
 * Why (#6155): batching alone was not enough. Measured on the scratch console
 * at 200 events/s, one write per window still peaked at an 8,273 ms task and 21
 * tasks over 200 ms, because each flush handed a keyed `{#each}` up to 500 new
 * keys and Svelte built and tore down that many row subtrees. Collapsing the
 * feed — same batching, no `{#each}` — dropped the worst task to 753 ms, which
 * is what identifies the render rather than the ingest as the remaining cost.
 * Capping admissions bounds that reconciliation no matter how long a window
 * actually ran; a throttled background window can stretch to seconds and would
 * otherwise deliver a second's worth of frames in one reconciliation.
 *
 * At the default window this passes 200 events/s intact, so the cap only
 * engages on a burst beyond that — and a flush that drops reports how many, so
 * the feed can say so rather than quietly losing rows.
 */
export const MAX_PER_FLUSH = 50;

/**
 * Collect frames and release them to `sink` at most once per window.
 *
 * Why the timer is injected: a test asserting "5,000 events produce N writes,
 * not 5,000" has to drive the clock, and a batcher that reaches for the global
 * `setTimeout` cannot be driven. The component passes the real pair.
 * What: `push` enqueues and arms the window if it is not already armed. When it
 * expires the queue is handed over newest-first, truncated to `max`. The sink
 * is told how many frames the truncation dropped so the caller can say so.
 * Test: `src/lib/feed-buffer.test.js`.
 *
 * @param {object} opts
 * @param {(batch: Array<object>, dropped: number) => void} opts.sink Receives each batch.
 * @param {number} [opts.windowMs] Batch window; defaults to [`BATCH_WINDOW_MS`].
 * @param {number} [opts.max] Cap per batch; defaults to [`MAX_PER_FLUSH`].
 * @param {(fn: Function, ms: number) => any} [opts.setTimer] Injected `setTimeout`.
 * @param {(handle: any) => void} [opts.clearTimer] Injected `clearTimeout`.
 */
export function createBatcher({
  sink,
  windowMs = BATCH_WINDOW_MS,
  max = MAX_PER_FLUSH,
  setTimer = setTimeout,
  clearTimer = clearTimeout
}) {
  let queue = [];
  let timer = null;

  function release() {
    timer = null;
    if (queue.length === 0) return;
    // Newest-first, and never more than one flush may render.
    const batch = queue.reverse().slice(0, max);
    const dropped = queue.length - batch.length;
    queue = [];
    sink(batch, dropped);
  }

  return {
    /** Enqueue one frame. Never touches the sink synchronously. */
    push(evt) {
      queue.push(evt);
      if (timer === null) timer = setTimer(release, windowMs);
    },
    /** Release whatever is queued now, cancelling the pending window. */
    flush() {
      if (timer !== null) {
        clearTimer(timer);
        timer = null;
      }
      release();
    },
    /** Drop the queue and disarm. Used on unmount and when the tab hides. */
    stop() {
      if (timer !== null) {
        clearTimer(timer);
        timer = null;
      }
      queue = [];
    },
    /** How many frames are waiting for the next window. Tests read this. */
    get pending() {
      return queue.length;
    }
  };
}

/**
 * Put a newest-first batch on the head of the list, dropping the oldest rows.
 *
 * Why a function rather than an inline spread: this is the only place the cap
 * is applied to live frames, and the cap is the property under test.
 * Test: `src/lib/feed-buffer.test.js` — `caps the buffer at MAX_EVENTS`.
 *
 * @param {Array<object>} events Current list, newest-first.
 * @param {Array<object>} batch New rows, newest-first.
 * @param {number} [max] Retention cap.
 * @returns {Array<object>} The new list, newest-first, at most `max` long.
 */
export function mergeLive(events, batch, max = MAX_EVENTS) {
  if (batch.length === 0) return events;
  if (batch.length >= max) return batch.slice(0, max);
  return [...batch, ...events].slice(0, max);
}

/**
 * Append history rows to the tail, under the same cap.
 *
 * Why (#6155): paging appended without a cap, so scrolling the feed grew
 * `events` past the retention limit one 50-row page at a time — against a
 * 49,551-row archive that is unbounded growth reached by scrolling.
 * Test: `src/lib/feed-buffer.test.js` — `caps history paging too`.
 *
 * @param {Array<object>} events Current list, newest-first.
 * @param {Array<object>} rows Older rows to append.
 * @param {number} [max] Retention cap.
 * @returns {Array<object>} The new list, at most `max` long.
 */
export function appendOlder(events, rows, max = MAX_EVENTS) {
  if (rows.length === 0) return events;
  return [...events, ...rows].slice(0, max);
}
