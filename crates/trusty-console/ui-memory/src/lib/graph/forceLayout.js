/*
 * Why: #7116 — the force simulation used to live inline in `PalaceGraph.svelte`
 * and mutate reactive `$state` node objects on every one of its 200 ticks, so
 * each tick repainted the whole SVG. Pulling the math out into a DOM-free
 * module with typed-array storage is what lets the same code run inside a Web
 * Worker (which has no DOM to touch) and be unit-tested without a browser.
 * What: a fixed-size force-directed layout over a Float64Array of interleaved
 * `[x0,y0,x1,y1,…]` coordinates. `step()` advances one tick; the coordinate
 * array is the layout's own buffer, so a caller can transfer or copy it.
 * Test: `src/lib/graph/forceLayout.test.js`
 */

/** Force constants. Lifted verbatim from the pre-#7116 inline simulation. */
export const LAYOUT_DEFAULTS = Object.freeze({
  linkDistance: 90,
  repulsion: 1200,
  centerStrength: 0.04,
  damping: 0.85,
  linkStrength: 0.05
});

/** Full re-layout step count, unchanged from the inline simulation. */
export const MAX_STEPS = 200;
/** Steps used to settle newly-added nodes during a click-to-expand. */
export const EXPAND_STEPS = 70;

/*
 * Why (#7116): repulsion is O(n²) per step, so the total work of a re-layout
 * grows as steps × n². Measured on the live trusty-tools palace (1,242 rendered
 * nodes, 770,661 pairs) one step costs 3.15 ms, i.e. ~4.1 ns per pair. Capping
 * total pair work rather than step count keeps a re-layout's duration bounded
 * whatever the graph size, instead of letting a bigger palace multiply the
 * runtime quadratically.
 * 3e8 pairs ≈ 1.2 s of simulation, which is short enough to sit behind a
 * progress bar.
 */
const MAX_PAIR_OPS = 3e8;
/** Never drop below this many steps — fewer produces no readable structure. */
const MIN_STEPS = 40;

/**
 * Steps to actually run, given how many nodes there are.
 *
 * Why: see `MAX_PAIR_OPS`. Returns `requested` unchanged for any graph small
 * enough to afford it, so the seed view and click-to-expand are unaffected.
 * What: clamps `requested` into `[MIN_STEPS, MAX_PAIR_OPS / pairs]`.
 * Test: `stepBudgetFor` cases in `forceLayout.test.js`.
 *
 * @param {number} nodeCount
 * @param {number} requested
 * @returns {number}
 */
export function stepBudgetFor(nodeCount, requested = MAX_STEPS) {
  if (nodeCount <= 1) return 0;
  const pairs = (nodeCount * (nodeCount - 1)) / 2;
  const affordable = Math.floor(MAX_PAIR_OPS / pairs);
  if (affordable >= requested) return requested;
  return Math.max(MIN_STEPS, affordable);
}

/**
 * Build a runnable layout over pre-allocated typed arrays.
 *
 * Why: every input is a transferable typed array so the worker boundary costs a
 * structured clone of raw buffers, not of 1,242 object graphs.
 * What: returns `{ step, steps, positions, count }`. `positions` is the same
 * buffer that was passed in — `step()` mutates it in place.
 * Test: `createLayout` cases in `forceLayout.test.js`.
 *
 * @param {object} spec
 * @param {number} spec.count          node count
 * @param {Float64Array} spec.positions interleaved x,y — length `2 * count`
 * @param {Int32Array} spec.linkSource  link endpoint indices
 * @param {Int32Array} spec.linkTarget  link endpoint indices
 * @param {Uint8Array} [spec.pinned]    1 = node is held in place
 * @param {number} spec.width
 * @param {number} spec.height
 * @param {object} [spec.options]       overrides for `LAYOUT_DEFAULTS`
 */
export function createLayout(spec) {
  const count = spec.count | 0;
  const p = spec.positions;
  const linkSource = spec.linkSource ?? new Int32Array(0);
  const linkTarget = spec.linkTarget ?? new Int32Array(0);
  const pinned = spec.pinned ?? new Uint8Array(count);
  const width = spec.width || 900;
  const height = spec.height || 600;
  const o = { ...LAYOUT_DEFAULTS, ...(spec.options ?? {}) };

  if (p.length < count * 2) {
    throw new Error(`positions holds ${p.length} slots, need ${count * 2}`);
  }

  const vx = new Float64Array(count);
  const vy = new Float64Array(count);

  function step() {
    // Repulsion. Pinned/pinned pairs are skipped because neither endpoint can
    // move — during a click-to-expand that reduces the real cost from
    // (all × all) to (new × all), which is what keeps expansion responsive.
    for (let i = 0; i < count; i++) {
      const xi = p[2 * i];
      const yi = p[2 * i + 1];
      const pi = pinned[i];
      for (let j = i + 1; j < count; j++) {
        if (pi && pinned[j]) continue;
        const dx = p[2 * j] - xi;
        const dy = p[2 * j + 1] - yi;
        let dist2 = dx * dx + dy * dy;
        if (dist2 < 1) dist2 = 1;
        const force = o.repulsion / dist2;
        const dist = Math.sqrt(dist2);
        const fx = (dx / dist) * force;
        const fy = (dy / dist) * force;
        vx[i] -= fx;
        vy[i] -= fy;
        vx[j] += fx;
        vy[j] += fy;
      }
    }

    // Link spring — pull connected nodes toward `linkDistance` apart.
    for (let k = 0; k < linkSource.length; k++) {
      const a = linkSource[k];
      const b = linkTarget[k];
      if (a < 0 || b < 0 || a >= count || b >= count) continue;
      const dx = p[2 * b] - p[2 * a];
      const dy = p[2 * b + 1] - p[2 * a + 1];
      const dist = Math.sqrt(dx * dx + dy * dy) || 1;
      const diff = (dist - o.linkDistance) * o.linkStrength;
      const fx = (dx / dist) * diff;
      const fy = (dy / dist) * diff;
      vx[a] += fx;
      vy[a] += fy;
      vx[b] -= fx;
      vy[b] -= fy;
    }

    // Centering, then integrate.
    const cx = width / 2;
    const cy = height / 2;
    for (let i = 0; i < count; i++) {
      vx[i] += (cx - p[2 * i]) * o.centerStrength;
      vy[i] += (cy - p[2 * i + 1]) * o.centerStrength;
      if (!pinned[i]) {
        p[2 * i] += vx[i];
        p[2 * i + 1] += vy[i];
      }
      vx[i] *= o.damping;
      vy[i] *= o.damping;
    }
  }

  return {
    count,
    positions: p,
    pinned,
    step,
    /** Run `n` steps back to back. Callers chunk this to stay yieldable. */
    steps(n) {
      for (let i = 0; i < n; i++) step();
    }
  };
}
