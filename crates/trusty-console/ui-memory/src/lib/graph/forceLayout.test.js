/*
 * Why: #7116 — the simulation used to be inline in `PalaceGraph.svelte`, where
 * nothing could reach it without a browser. These cover the extracted math and,
 * more importantly, the step budget that keeps a re-layout's runtime bounded as
 * the graph grows.
 * Test target: `src/lib/graph/forceLayout.js`
 */
import { describe, expect, it } from 'vitest';
import { EXPAND_STEPS, MAX_STEPS, createLayout, stepBudgetFor } from './forceLayout.js';

/** Two connected nodes placed far apart, so the link spring has work to do. */
function pair(ax, ay, bx, by) {
  return {
    count: 2,
    positions: Float64Array.from([ax, ay, bx, by]),
    linkSource: Int32Array.from([0]),
    linkTarget: Int32Array.from([1]),
    width: 900,
    height: 600
  };
}

describe('stepBudgetFor', () => {
  it('leaves small graphs at the requested step count', () => {
    // The seed view (75 nodes) and a click-to-expand must be untouched.
    expect(stepBudgetFor(75, MAX_STEPS)).toBe(MAX_STEPS);
    expect(stepBudgetFor(200, EXPAND_STEPS)).toBe(EXPAND_STEPS);
  });

  it('leaves the measured full-load size at the requested step count', () => {
    // 1,242 nodes is what the live trusty-tools palace renders under
    // "load everything" — 770,661 pairs, comfortably inside the budget.
    expect(stepBudgetFor(1242, MAX_STEPS)).toBe(MAX_STEPS);
  });

  it('clips a graph large enough that steps x n^2 would run away', () => {
    // 20,000 nodes is ~2e8 pairs — one step is already most of the budget.
    const clipped = stepBudgetFor(20000, MAX_STEPS);
    expect(clipped).toBeLessThan(MAX_STEPS);
    // ...but never to zero: a clipped layout still has to place clusters.
    expect(clipped).toBeGreaterThanOrEqual(40);
  });

  it('reports nothing to do for a graph that cannot have pairs', () => {
    expect(stepBudgetFor(0, MAX_STEPS)).toBe(0);
    expect(stepBudgetFor(1, MAX_STEPS)).toBe(0);
  });
});

describe('createLayout', () => {
  it('pulls a stretched link toward the target distance', () => {
    const layout = createLayout(pair(100, 300, 800, 300));
    const before = Math.abs(layout.positions[2] - layout.positions[0]);
    layout.steps(60);
    const after = Math.abs(layout.positions[2] - layout.positions[0]);
    expect(after).toBeLessThan(before);
  });

  it('pushes two coincident nodes apart rather than dividing by zero', () => {
    const spec = pair(400, 300, 400, 300);
    spec.linkSource = new Int32Array(0);
    spec.linkTarget = new Int32Array(0);
    const layout = createLayout(spec);
    layout.steps(20);
    for (const v of layout.positions) expect(Number.isFinite(v)).toBe(true);
  });

  it('holds a pinned node exactly where it started', () => {
    const spec = pair(100, 300, 800, 300);
    spec.pinned = Uint8Array.from([1, 0]);
    const layout = createLayout(spec);
    layout.steps(50);
    expect(layout.positions[0]).toBe(100);
    expect(layout.positions[1]).toBe(300);
    // The unpinned end still moved.
    expect(layout.positions[2]).not.toBe(800);
  });

  it('mutates the caller-supplied buffer in place', () => {
    const spec = pair(100, 300, 800, 300);
    const buf = spec.positions;
    const layout = createLayout(spec);
    expect(layout.positions).toBe(buf);
  });

  it('refuses a positions buffer too small for the node count', () => {
    expect(() =>
      createLayout({
        count: 4,
        positions: new Float64Array(4),
        width: 900,
        height: 600
      })
    ).toThrow(/need 8/);
  });

  it('ignores link endpoints outside the node range', () => {
    const spec = pair(100, 300, 800, 300);
    spec.linkSource = Int32Array.from([0, 9]);
    spec.linkTarget = Int32Array.from([1, 1]);
    const layout = createLayout(spec);
    layout.steps(10);
    for (const v of layout.positions) expect(Number.isFinite(v)).toBe(true);
  });
});
