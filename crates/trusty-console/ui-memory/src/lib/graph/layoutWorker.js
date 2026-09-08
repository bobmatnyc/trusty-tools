/*
 * Why: #7116 — running the layout on the main thread is what froze the tab.
 * Even chunked, the simulation competes with paint and input for the same
 * thread. A worker removes it from that thread entirely, so the only main-
 * thread cost left is committing coordinates the worker has already computed.
 * What: receives one `{type:'start'}` spec, runs the layout, and posts a
 * position snapshot on a wall-clock cadence plus a final one at the end.
 * Snapshots are copies (the worker keeps stepping its own buffer), transferred
 * so the copy costs no clone.
 * Test: the worker's stepping logic is `forceLayout.js`, covered by
 * `forceLayout.test.js`; `layoutRunner.test.js` covers the protocol below
 * against a fake worker.
 */
import { createLayout } from './forceLayout.js';

/** Snapshot cadence. Faster than this only produces frames nobody paints. */
const SNAPSHOT_MS = 60;

self.onmessage = (ev) => {
  const msg = ev.data;
  if (!msg || msg.type !== 'start') return;

  const layout = createLayout(msg.spec);
  const total = msg.steps | 0;
  let last = 0;

  const snapshot = (step) => {
    const copy = layout.positions.slice();
    self.postMessage({ type: 'progress', step, total, positions: copy }, [copy.buffer]);
  };

  for (let i = 0; i < total; i++) {
    layout.step();
    const now = Date.now();
    if (now - last >= SNAPSHOT_MS) {
      last = now;
      snapshot(i + 1);
    }
  }

  const done = layout.positions.slice();
  self.postMessage({ type: 'done', step: total, total, positions: done }, [done.buffer]);
};
