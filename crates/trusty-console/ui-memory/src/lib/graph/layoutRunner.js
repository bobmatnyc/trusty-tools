/*
 * Why: #7116 — the pre-fix view drove its simulation with
 * `setInterval(tick, 16)` and reassigned the reactive `nodes` array at the end
 * of every tick, so all 200 steps of a full re-layout each triggered a repaint
 * of the whole SVG. Measured on the live trusty-tools palace that is 1,242
 * nodes and 5,000 edges repainted 200 times, and the tab was unresponsive for
 * over six minutes. The steps themselves cost only 3.15 ms each (0.6 s for all
 * 200) — the paints were the whole cost.
 * What: this driver separates *stepping* from *committing*. The worker (or, if
 * `Worker` is unavailable, a chunked local loop) does the stepping; the driver
 * commits at most ONE snapshot per animation frame, coalescing anything that
 * arrived in between. The browser therefore paints on its own schedule and
 * keeps a slot for input, which is the responsiveness the issue asks for.
 * Test: `src/lib/graph/layoutRunner.test.js`
 */
import { createLayout } from './forceLayout.js';

/*
 * Local-fallback frame budget. Half a 16.7 ms frame leaves the other half for
 * the commit and for input; the loop stops stepping the moment it is exceeded.
 */
const FRAME_BUDGET_MS = 8;

/**
 * Default worker factory. Kept behind a function so tests can inject a fake and
 * so a `Worker`-less environment (vitest/jsdom) falls back cleanly.
 *
 * Why the `new URL(..., import.meta.url)` form: it is the shape Vite statically
 * detects and bundles the worker chunk for. A string path would not be rewritten
 * for the hashed asset layout the console embeds.
 * @returns {Worker|null}
 */
export function defaultWorkerFactory() {
  if (typeof Worker === 'undefined') return null;
  try {
    return new Worker(new URL('./layoutWorker.js', import.meta.url), { type: 'module' });
  } catch {
    // A CSP or packaging failure must degrade to the local loop, not to a
    // blank graph.
    return null;
  }
}

/*
 * Why (#7116): `requestAnimationFrame` does not fire in a hidden tab, so a
 * layout scheduled purely on rAF stalls the moment the operator switches away
 * and leaves the progress bar up forever. Falling back to a timer keeps it
 * finishing in the background — the browser throttles that timer hard, which is
 * exactly right for work nobody is looking at — and the next schedule returns to
 * rAF as soon as the tab is visible again.
 */
export function defaultScheduleFrame(cb) {
  if (typeof document !== 'undefined' && document.hidden) {
    return setTimeout(() => cb(Date.now()), 32);
  }
  return requestAnimationFrame(cb);
}

/**
 * Run a layout and stream its positions back, one commit per frame at most.
 *
 * @param {object} opts
 * @param {object} opts.spec       `createLayout` spec — see `forceLayout.js`
 * @param {number} opts.steps      how many steps to run
 * @param {(positions: Float64Array, info: {step:number,total:number}) => void} opts.onFrame
 * @param {() => void} [opts.onDone]
 * @param {(cb: (t:number)=>void) => number} [opts.scheduleFrame]  defaults to rAF
 * @param {() => Worker|null} [opts.createWorker]
 * @param {() => number} [opts.now]
 * @returns {{cancel: () => void}}
 */
export function runLayout(opts) {
  const {
    spec,
    steps,
    onFrame,
    onDone,
    scheduleFrame = defaultScheduleFrame,
    createWorker = defaultWorkerFactory,
    now = () => (typeof performance !== 'undefined' ? performance.now() : Date.now())
  } = opts;

  let cancelled = false;
  let framePending = false;
  /** Latest snapshot the stepper produced but the frame has not committed. */
  let pending = null;
  let finished = false;

  // #7116: the coalescing point. Any number of snapshots can land between two
  // frames; only the newest is ever painted, so a fast stepper cannot outrun
  // the compositor and queue work the tab must chew through later.
  function requestCommit() {
    if (framePending || cancelled) return;
    framePending = true;
    scheduleFrame(() => {
      framePending = false;
      if (cancelled) return;
      const snap = pending;
      pending = null;
      if (snap) onFrame(snap.positions, { step: snap.step, total: snap.total });
      if (finished) settle();
      else if (pending) requestCommit();
    });
  }

  let settled = false;
  function settle() {
    if (settled) return;
    settled = true;
    onDone?.();
  }

  function publish(positions, step, total) {
    pending = { positions, step, total };
    requestCommit();
  }

  if (steps <= 0 || spec.count <= 0) {
    // Nothing to simulate — still deliver the initial positions once so the
    // caller can paint, then finish on the next frame rather than re-entrantly.
    finished = true;
    publish(spec.positions, 0, 0);
    return { cancel: () => { cancelled = true; } };
  }

  const worker = createWorker();

  if (worker) {
    worker.onmessage = (ev) => {
      if (cancelled) return;
      const m = ev.data;
      if (!m) return;
      if (m.type === 'done') finished = true;
      publish(m.positions, m.step, m.total);
      if (m.type === 'done') worker.terminate();
    };
    worker.onerror = () => {
      // The worker died mid-layout; the nodes keep whatever positions were last
      // committed rather than vanishing.
      finished = true;
      publish(spec.positions, steps, steps);
      worker.terminate();
    };
    // The spec's buffers are handed to the worker; the caller keeps its own copy.
    worker.postMessage({ type: 'start', spec, steps });
    return {
      cancel: () => {
        cancelled = true;
        worker.terminate();
      }
    };
  }

  // ---- Local fallback: same math, chunked so it never holds a frame. -------
  const layout = createLayout(spec);
  let step = 0;

  function chunk() {
    if (cancelled) return;
    const deadline = now() + FRAME_BUDGET_MS;
    do {
      layout.step();
      step++;
    } while (step < steps && now() < deadline);

    if (step >= steps) finished = true;
    publish(layout.positions.slice(), step, steps);
    if (!finished) scheduleFrame(chunk);
  }

  scheduleFrame(chunk);
  return { cancel: () => { cancelled = true; } };
}
