/*
 * Why: #7116 — the freeze was not the force math (3.15 ms per step on the live
 * 1,242-node payload, 0.6 s for all 200). It was that the pre-fix driver
 * repainted after EVERY step: `setInterval(tick, 16)` ending in
 * `nodes = nodes`, so 200 steps meant 200 commits of ~6,300 SVG elements. The
 * assertions below are the ones that pre-fix arrangement fails — one commit per
 * frame rather than one per step, and never all the steps in a single task.
 * Test target: `src/lib/graph/layoutRunner.js`
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { defaultScheduleFrame, defaultWorkerFactory, runLayout } from './layoutRunner.js';

/**
 * A manual frame scheduler. Nothing runs until `flush()` is called, so a test
 * can count exactly how many commits each frame produced.
 */
function manualFrames() {
  const queue = [];
  let count = 0;
  return {
    schedule: (cb) => {
      queue.push(cb);
      return ++count;
    },
    pending: () => queue.length,
    /** Run every callback queued right now; ones they queue wait for the next flush. */
    flush() {
      const batch = queue.splice(0, queue.length);
      for (const cb of batch) cb(performance.now());
      return batch.length;
    },
    /** Flush repeatedly until the queue drains or `limit` rounds elapse. */
    drain(limit = 5000) {
      let rounds = 0;
      while (queue.length && rounds < limit) {
        this.flush();
        rounds++;
      }
      return rounds;
    }
  };
}

function graph(count) {
  const positions = new Float64Array(count * 2);
  for (let i = 0; i < count; i++) {
    positions[2 * i] = 450 + (i % 40) * 3;
    positions[2 * i + 1] = 300 + Math.floor(i / 40) * 3;
  }
  const n = Math.max(0, count - 1);
  const linkSource = new Int32Array(n);
  const linkTarget = new Int32Array(n);
  for (let i = 0; i < n; i++) {
    linkSource[i] = i;
    linkTarget[i] = i + 1;
  }
  return { count, positions, linkSource, linkTarget, width: 900, height: 600 };
}

/** No worker available — exercises the chunked local fallback. */
const noWorker = () => null;

describe('runLayout — local fallback', () => {
  it('commits far fewer times than it steps', () => {
    const frames = manualFrames();
    const onFrame = vi.fn();
    const steps = 200;

    runLayout({
      spec: graph(120),
      steps,
      onFrame,
      scheduleFrame: frames.schedule,
      createWorker: noWorker
    });

    const rounds = frames.drain();
    expect(onFrame).toHaveBeenCalled();
    // The pre-fix driver repainted once per step. Each chunk here spends its
    // whole 8 ms budget stepping and repaints once at the end of it.
    expect(onFrame.mock.calls.length).toBeLessThan(steps);
    // And never more than one repaint per frame the browser gave us.
    expect(onFrame.mock.calls.length).toBeLessThanOrEqual(rounds);
  });

  it('never runs the whole simulation inside one task', () => {
    const frames = manualFrames();
    let maxStepsInOneChunk = 0;
    let previous = 0;
    const steps = 400;

    runLayout({
      spec: graph(400),
      steps,
      onFrame: (_p, info) => {
        maxStepsInOneChunk = Math.max(maxStepsInOneChunk, info.step - previous);
        previous = info.step;
      },
      scheduleFrame: frames.schedule,
      createWorker: noWorker,
      // A clock that advances past the 8 ms budget on every reading, so one
      // chunk can only ever afford a single step.
      now: (() => {
        let t = 0;
        return () => (t += 20);
      })()
    });

    frames.drain();
    expect(maxStepsInOneChunk).toBe(1);
  });

  it('reaches the requested step count and finishes once', () => {
    const frames = manualFrames();
    const onDone = vi.fn();
    let lastStep = 0;

    runLayout({
      spec: graph(60),
      steps: 90,
      onFrame: (_p, info) => (lastStep = info.step),
      onDone,
      scheduleFrame: frames.schedule,
      createWorker: noWorker
    });

    frames.drain();
    expect(lastStep).toBe(90);
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it('stops committing after cancel', () => {
    const frames = manualFrames();
    const onFrame = vi.fn();

    const handle = runLayout({
      spec: graph(80),
      steps: 500,
      onFrame,
      scheduleFrame: frames.schedule,
      createWorker: noWorker
    });

    frames.flush();
    frames.flush();
    const committed = onFrame.mock.calls.length;
    handle.cancel();
    frames.drain();

    expect(onFrame.mock.calls.length).toBe(committed);
  });

  it('delivers the starting positions when there is nothing to simulate', () => {
    const frames = manualFrames();
    const onFrame = vi.fn();
    const onDone = vi.fn();

    runLayout({
      spec: graph(0),
      steps: 0,
      onFrame,
      onDone,
      scheduleFrame: frames.schedule,
      createWorker: noWorker
    });

    frames.drain();
    expect(onFrame).toHaveBeenCalledTimes(1);
    expect(onDone).toHaveBeenCalledTimes(1);
  });
});

describe('defaultScheduleFrame', () => {
  afterEach(() => {
    vi.restoreAllMocks();
    vi.useRealTimers();
  });

  it('uses requestAnimationFrame while the tab is visible', () => {
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(false);
    const raf = vi.spyOn(globalThis, 'requestAnimationFrame').mockReturnValue(7);
    expect(defaultScheduleFrame(() => {})).toBe(7);
    expect(raf).toHaveBeenCalled();
  });

  it('falls back to a timer while the tab is hidden, so the run still finishes', () => {
    // #7116: rAF does not fire in a hidden tab. Without this the layout stalls
    // the moment the operator switches away and the progress bar never clears.
    vi.spyOn(document, 'hidden', 'get').mockReturnValue(true);
    const raf = vi.spyOn(globalThis, 'requestAnimationFrame');
    vi.useFakeTimers();
    const cb = vi.fn();
    defaultScheduleFrame(cb);
    expect(raf).not.toHaveBeenCalled();
    vi.advanceTimersByTime(50);
    expect(cb).toHaveBeenCalled();
  });
});

describe('defaultWorkerFactory', () => {
  const original = Object.getOwnPropertyDescriptor(globalThis, 'Worker');

  afterEach(() => {
    if (original) Object.defineProperty(globalThis, 'Worker', original);
    else delete globalThis.Worker;
  });

  it('returns null where Worker does not exist', () => {
    delete globalThis.Worker;
    expect(defaultWorkerFactory()).toBeNull();
  });

  it('returns null when the Worker constructor throws', () => {
    // #7116: a CSP or packaging failure must degrade to the local loop, not
    // propagate out of `runLayout` and leave the graph blank.
    globalThis.Worker = function ThrowingWorker() {
      throw new Error('Refused to create a worker (CSP)');
    };
    expect(defaultWorkerFactory()).toBeNull();
  });

  it('returns the constructed worker when the environment allows one', () => {
    const made = [];
    globalThis.Worker = function OkWorker(url, opts) {
      made.push({ url: String(url), opts });
    };
    const w = defaultWorkerFactory();
    expect(w).toBeInstanceOf(globalThis.Worker);
    expect(made).toHaveLength(1);
    expect(made[0].url).toContain('layoutWorker.js');
    expect(made[0].opts).toEqual({ type: 'module' });
  });
});

describe('runLayout — worker path', () => {
  /** Minimal stand-in for the real worker's message protocol. */
  function fakeWorker() {
    const w = {
      onmessage: null,
      onerror: null,
      posted: [],
      terminated: false,
      postMessage: (m) => w.posted.push(m),
      terminate: () => (w.terminated = true),
      emit: (msg) => w.onmessage?.({ data: msg })
    };
    return w;
  }

  it('coalesces a burst of worker snapshots into one committed frame', () => {
    const frames = manualFrames();
    const onFrame = vi.fn();
    const w = fakeWorker();

    runLayout({
      spec: graph(50),
      steps: 200,
      onFrame,
      scheduleFrame: frames.schedule,
      createWorker: () => w
    });

    // Ten snapshots arrive before the browser gets a frame.
    for (let i = 1; i <= 10; i++) {
      w.emit({ type: 'progress', step: i * 10, total: 200, positions: new Float64Array(100) });
    }
    expect(onFrame).not.toHaveBeenCalled();

    frames.flush();
    // Exactly one paint, carrying the newest snapshot.
    expect(onFrame).toHaveBeenCalledTimes(1);
    expect(onFrame.mock.calls[0][1]).toEqual({ step: 100, total: 200 });
  });

  it('finishes and terminates the worker on done', () => {
    const frames = manualFrames();
    const onDone = vi.fn();
    const w = fakeWorker();

    runLayout({
      spec: graph(50),
      steps: 200,
      onFrame: () => {},
      onDone,
      scheduleFrame: frames.schedule,
      createWorker: () => w
    });

    w.emit({ type: 'done', step: 200, total: 200, positions: new Float64Array(100) });
    frames.drain();

    expect(onDone).toHaveBeenCalledTimes(1);
    expect(w.terminated).toBe(true);
  });

  it('keeps the last committed positions when the worker errors', () => {
    const frames = manualFrames();
    const onFrame = vi.fn();
    const onDone = vi.fn();
    const w = fakeWorker();

    runLayout({
      spec: graph(50),
      steps: 200,
      onFrame,
      onDone,
      scheduleFrame: frames.schedule,
      createWorker: () => w
    });

    w.onerror(new Error('worker died'));
    frames.drain();

    expect(onFrame).toHaveBeenCalledTimes(1);
    expect(onDone).toHaveBeenCalledTimes(1);
  });

  it('terminates the worker on cancel', () => {
    const frames = manualFrames();
    const w = fakeWorker();

    const handle = runLayout({
      spec: graph(50),
      steps: 200,
      onFrame: () => {},
      scheduleFrame: frames.schedule,
      createWorker: () => w
    });

    handle.cancel();
    expect(w.terminated).toBe(true);
  });
});
