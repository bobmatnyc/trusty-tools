// What the activity feed does with a flood of events (issue #6155).
//
// Why: the feed wrote reactive state once per SSE frame. At 200 events/s that
// produced a single 46,420 ms main-thread task and left 97% of a 69 s window
// stalled, so the SPA's own `/health` and `/api/v1/palaces` calls hit the 35 s
// abort in `api.js` against a daemon answering curl in 19 ms. These assert the
// two properties that stop it recurring: the buffer never grows past its cap,
// and a burst costs a bounded number of state writes rather than one per frame.
// What: drives `createBatcher` with an injected clock so "5,000 events" is a
// deterministic assertion rather than a timing race, and mounts the real
// component to prove an unmounted feed holds no subscription.
// Test: this file — `pnpm test`.

import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

import ActivityFeed from './components/ActivityFeed.svelte';
import {
  BATCH_WINDOW_MS,
  MAX_EVENTS,
  MAX_PER_FLUSH,
  appendOlder,
  createBatcher,
  mergeLive
} from './feed-buffer.js';

/** One frame shaped like the daemon's `hook_fired`, which dominates the stream. */
function hookEvent(i) {
  return {
    type: 'hook_fired',
    palace_id: `palace-${i % 7}`,
    palace_name: `palace-${i % 7}`,
    hook_type: 'UserPromptSubmit',
    injection_kind: 'prompt-context',
    injection_length: 3000 + i,
    duration_ms: 61,
    source: 'hook',
    seq: i
  };
}

/** A controllable stand-in for `setTimeout` / `clearTimeout`. */
function fakeClock() {
  let next = 1;
  const timers = new Map();
  return {
    setTimer(fn, ms) {
      const id = next++;
      timers.set(id, { fn, ms });
      return id;
    },
    clearTimer(id) {
      timers.delete(id);
    },
    /** Fire every armed timer once — one batch window elapsing. */
    tick() {
      const due = [...timers.entries()];
      timers.clear();
      for (const [, t] of due) t.fn();
      return due.length;
    },
    get armed() {
      return timers.size;
    }
  };
}

describe('createBatcher — a burst costs a bounded number of state writes', () => {
  it('turns 5,000 events into one write per batch window, not 5,000', () => {
    const clock = fakeClock();
    const batches = [];
    const b = createBatcher({
      sink: (batch) => batches.push(batch),
      setTimer: clock.setTimer,
      clearTimer: clock.clearTimer
    });

    for (let i = 0; i < 5000; i++) b.push(hookEvent(i));

    // Nothing has reached the sink yet: pushing never writes synchronously.
    expect(batches).toHaveLength(0);
    expect(b.pending).toBe(5000);

    clock.tick();

    // One window elapsed, so exactly one state update — the property the
    // 46 s stall violated 5,000 times over.
    expect(batches).toHaveLength(1);
    expect(b.pending).toBe(0);
  });

  it('caps each flush at MAX_PER_FLUSH, keeps the newest, and reports the drop', () => {
    const clock = fakeClock();
    const batches = [];
    const drops = [];
    const b = createBatcher({
      sink: (batch, dropped) => {
        batches.push(batch);
        drops.push(dropped);
      },
      setTimer: clock.setTimer,
      clearTimer: clock.clearTimer
    });

    for (let i = 0; i < 5000; i++) b.push(hookEvent(i));
    clock.tick();

    const batch = batches[0];
    // #6155: bounding the rows one flush hands the keyed {#each} is what keeps
    // the reconciliation off the 8 s path; the buffer cap alone did not.
    expect(batch).toHaveLength(MAX_PER_FLUSH);
    expect(drops[0]).toBe(5000 - MAX_PER_FLUSH);
    // Newest-first: frame 4999 arrived last and leads the batch.
    expect(batch[0].seq).toBe(4999);
    expect(batch[MAX_PER_FLUSH - 1].seq).toBe(4999 - (MAX_PER_FLUSH - 1));
  });

  it('passes 200 events/s through intact at the default window', () => {
    const clock = fakeClock();
    const batches = [];
    const drops = [];
    const b = createBatcher({
      sink: (batch, dropped) => {
        batches.push(batch);
        drops.push(dropped);
      },
      setTimer: clock.setTimer,
      clearTimer: clock.clearTimer
    });

    // One 250 ms window's worth at the rate the fix must survive.
    const perWindow = Math.round(200 * (BATCH_WINDOW_MS / 1000));
    for (let i = 0; i < perWindow; i++) b.push(hookEvent(i));
    clock.tick();

    expect(batches[0]).toHaveLength(perWindow);
    expect(drops[0]).toBe(0);
  });

  it('spreads a sustained stream across windows, one write each', () => {
    const clock = fakeClock();
    const batches = [];
    const b = createBatcher({
      sink: (batch) => batches.push(batch),
      setTimer: clock.setTimer,
      clearTimer: clock.clearTimer
    });

    // 60 s at 200 events/s, released once per window.
    const windows = Math.round(60_000 / BATCH_WINDOW_MS);
    const perWindow = Math.round(200 * (BATCH_WINDOW_MS / 1000));
    for (let w = 0; w < windows; w++) {
      for (let i = 0; i < perWindow; i++) b.push(hookEvent(w * perWindow + i));
      clock.tick();
    }

    expect(batches).toHaveLength(windows);
    // 12,000 frames, 240 state writes — 50x fewer, and none over the cap.
    expect(windows * perWindow).toBe(12_000);
    for (const batch of batches) expect(batch.length).toBeLessThanOrEqual(MAX_PER_FLUSH);
  });

  it('stop() drops the queue and disarms the window', () => {
    const clock = fakeClock();
    const batches = [];
    const b = createBatcher({
      sink: (batch) => batches.push(batch),
      setTimer: clock.setTimer,
      clearTimer: clock.clearTimer
    });

    for (let i = 0; i < 100; i++) b.push(hookEvent(i));
    expect(clock.armed).toBe(1);

    b.stop();

    expect(clock.armed).toBe(0);
    expect(b.pending).toBe(0);
    clock.tick();
    expect(batches).toHaveLength(0);
  });
});

describe('the retained buffer never grows past its cap', () => {
  it('caps the buffer at MAX_EVENTS across many merges', () => {
    let events = [];
    for (let w = 0; w < 200; w++) {
      const batch = Array.from({ length: 50 }, (_, i) => hookEvent(w * 50 + i)).reverse();
      events = mergeLive(events, batch);
      expect(events.length).toBeLessThanOrEqual(MAX_EVENTS);
    }
    expect(events).toHaveLength(MAX_EVENTS);
    // Drop-oldest: the newest frame is at the head.
    expect(events[0].seq).toBe(200 * 50 - 1);
  });

  it('caps history paging too', () => {
    // #6155: paging appended without a cap, so scrolling grew the list one
    // 50-row page at a time against a 49,551-row archive.
    let events = Array.from({ length: 50 }, (_, i) => hookEvent(i));
    for (let page = 0; page < 100; page++) {
      const rows = Array.from({ length: 50 }, (_, i) => hookEvent(10_000 + page * 50 + i));
      events = appendOlder(events, rows);
      expect(events.length).toBeLessThanOrEqual(MAX_EVENTS);
    }
    expect(events).toHaveLength(MAX_EVENTS);
  });
});

describe('subscription lifecycle', () => {
  let opened;
  let closed;

  beforeEach(() => {
    opened = [];
    closed = 0;
    // A stand-in EventSource that records opens and closes. jsdom has none.
    class FakeEventSource {
      constructor(url) {
        this.url = url;
        opened.push(this);
        // The component assigns onopen/onmessage/onerror after construction.
        queueMicrotask(() => this.onopen && this.onopen());
      }
      close() {
        closed += 1;
      }
    }
    vi.stubGlobal('EventSource', FakeEventSource);
    // The feed hydrates from these on mount; keep them from touching the net.
    vi.stubGlobal(
      'fetch',
      vi.fn(async () => ({
        ok: true,
        status: 200,
        headers: { get: () => 'application/json' },
        json: async () => ({ entries: [], total: 0 }),
        text: async () => ''
      }))
    );
  });

  afterEach(() => {
    vi.unstubAllGlobals();
  });

  it('an unmounted feed has no live subscription', () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const component = mount(ActivityFeed, { target });
    flushSync();
    expect(opened).toHaveLength(1);

    unmount(component);
    flushSync();

    // The stream is closed, and a frame arriving after unmount reaches
    // nothing — the batcher was stopped with it.
    expect(closed).toBeGreaterThanOrEqual(1);
    expect(() =>
      opened[0].onmessage?.({ data: JSON.stringify(hookEvent(1)) })
    ).not.toThrow();

    target.remove();
  });

  it('drops the stream while the tab is hidden and resumes when it returns', () => {
    const target = document.createElement('div');
    document.body.appendChild(target);

    const component = mount(ActivityFeed, { target });
    flushSync();
    expect(opened).toHaveLength(1);

    // #6155: EventSource delivery is not throttled in a background tab, so a
    // hidden feed kept paying full ingest cost for a panel nobody could see.
    Object.defineProperty(document, 'hidden', { value: true, configurable: true });
    document.dispatchEvent(new Event('visibilitychange'));
    flushSync();
    expect(closed).toBeGreaterThanOrEqual(1);
    expect(opened).toHaveLength(1);

    Object.defineProperty(document, 'hidden', { value: false, configurable: true });
    document.dispatchEvent(new Event('visibilitychange'));
    flushSync();
    expect(opened).toHaveLength(2);

    unmount(component);
    target.remove();
  });
});
