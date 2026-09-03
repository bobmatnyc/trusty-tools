/**
 * Tests for the home page's machine-status stream client (#6642).
 *
 * Run: `node --test src/machineStream.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  DEFAULT_CAPACITY,
  EXPECTED_SCHEMA_VERSION,
  HISTORY_URL,
  STREAM_URL,
  applyHistory,
  applySample,
  applyServices,
  createMachineStream,
  initialState,
  parseFrame,
  reconnectDelay,
} from './machineStream.js';

// ── fakes ──────────────────────────────────────────────────────────────────

/**
 * A stand-in for the browser's `EventSource`.
 *
 * Only what the client uses: named listeners, `close`, and an `emit` the test
 * drives the connection with. No network, no timers.
 */
class FakeEventSource {
  constructor(url) {
    this.url = url;
    this.listeners = new Map();
    this.closed = false;
    FakeEventSource.instances.push(this);
  }

  addEventListener(kind, handler) {
    const list = this.listeners.get(kind) ?? [];
    list.push(handler);
    this.listeners.set(kind, list);
  }

  emit(kind, data) {
    for (const handler of this.listeners.get(kind) ?? []) handler({ data });
  }

  close() {
    this.closed = true;
  }
}
FakeEventSource.instances = [];

/** A minimal `HostMetrics`-shaped sample; only the fields the graphs read. */
function hostSample(pct) {
  return {
    cpu: { usage_pct: pct, logical_cores: 8, physical_cores: 8, pressure: 'nominal' },
    memory: { usage_pct: pct, total_bytes: 100, used_bytes: pct, pressure: 'nominal' },
    disks: { aggregate_usage_pct: pct, pressure: 'nominal' },
    network: { rx_bytes_per_sec: pct, tx_bytes_per_sec: 0, window_secs: 1 },
    overall_pressure: 'nominal',
    sampled_at_unix: 1_700_000_000 + pct,
  };
}

/** A `HistorySnapshot`-shaped payload with `count` host samples. */
function snapshot(count, extra = {}) {
  return {
    samples: Array.from({ length: count }, (_, i) => hostSample(i)),
    transitions: [],
    service_samples: {
      'trusty-search': [{ id: 'trusty-search', status: 'running', cpu_pct: 1.5 }],
    },
    sample_capacity: DEFAULT_CAPACITY,
    service_sample_capacity: DEFAULT_CAPACITY,
    transition_capacity: 200,
    sample_interval_secs: 1,
    schema_version: EXPECTED_SCHEMA_VERSION,
    ...extra,
  };
}

/** Build a client wired to fakes; returns the client plus what it was given. */
function harness({ fetchImpl } = {}) {
  FakeEventSource.instances = [];
  const states = [];
  const warnings = [];
  const client = createMachineStream({
    onState: (s) => states.push(s),
    EventSourceImpl: FakeEventSource,
    fetchImpl,
    setTimeoutImpl: () => 0,
    clearTimeoutImpl: () => {},
    logger: { warn: (m) => warnings.push(m) },
  });
  client.start();
  return { client, states, warnings, source: () => FakeEventSource.instances.at(-1) };
}

// ── reducers ───────────────────────────────────────────────────────────────

test('a history snapshot seeds the whole window', () => {
  const state = applyHistory(initialState(), snapshot(3));
  assert.equal(state.samples.length, 3);
  assert.equal(state.samples[0].cpu.usage_pct, 0, 'oldest first, as the server sent');
  assert.equal(state.samples[2].cpu.usage_pct, 2);
  assert.deepEqual(state.serviceSamples['trusty-search'], [
    { id: 'trusty-search', status: 'running', cpu_pct: 1.5 },
  ]);
  assert.equal(state.sampleIntervalSecs, 1);
  assert.equal(state.seeded, true);
});

test('a snapshot with a shorter ring is capped to the capacity it declares', () => {
  const payload = snapshot(10, { sample_capacity: 4 });
  const state = applyHistory(initialState(), payload);
  assert.equal(state.capacity, 4);
  assert.equal(state.samples.length, 4);
  assert.equal(state.samples[3].cpu.usage_pct, 9, 'the newest survives the trim');
});

test('appending past the capacity drops the oldest sample, never the newest', () => {
  let state = applyHistory(initialState(), snapshot(DEFAULT_CAPACITY));
  assert.equal(state.samples.length, DEFAULT_CAPACITY);

  state = applySample(state, hostSample(999));

  assert.equal(state.samples.length, DEFAULT_CAPACITY, 'capped at 600');
  assert.equal(state.samples.at(-1).cpu.usage_pct, 999, 'newest is last');
  assert.equal(state.samples[0].cpu.usage_pct, 1, 'sample 0 aged out');
});

test('a service batch appends per id and keeps a null cpu_pct as null', () => {
  let state = initialState();
  state = applyServices(state, {
    sampled_at_unix: 1,
    services: [
      { id: 'trusty-search', status: 'running', cpu_pct: 2.5 },
      { id: 'trusty-review', status: 'available', cpu_pct: null },
    ],
  });
  state = applyServices(state, {
    sampled_at_unix: 2,
    services: [{ id: 'trusty-search', status: 'running', cpu_pct: 3.5 }],
  });

  assert.deepEqual(
    state.serviceSamples['trusty-search'].map((s) => s.cpu_pct),
    [2.5, 3.5],
  );
  assert.equal(state.serviceSamples['trusty-review'][0].cpu_pct, null);
  assert.notEqual(state.serviceSamples['trusty-review'][0].cpu_pct, 0);
});

test('a per-service ring caps at its own capacity', () => {
  let state = { ...initialState(), serviceCapacity: 3 };
  for (let i = 0; i < 5; i += 1) {
    state = applyServices(state, {
      sampled_at_unix: i,
      services: [{ id: 'trusty-memory', status: 'running', cpu_pct: i }],
    });
  }
  assert.deepEqual(
    state.serviceSamples['trusty-memory'].map((s) => s.cpu_pct),
    [2, 3, 4],
  );
});

test('parseFrame returns null for anything that is not a JSON object', () => {
  assert.equal(parseFrame('{"a":1}').a, 1);
  assert.equal(parseFrame('not json'), null);
  assert.equal(parseFrame('[1,2]'), null, 'an array is not a payload this client reads');
  assert.equal(parseFrame(''), null);
  assert.equal(parseFrame(undefined), null);
});

test('reconnect backoff grows then holds at the longest delay', () => {
  assert.equal(reconnectDelay(0, [1, 2, 5]), 1);
  assert.equal(reconnectDelay(1, [1, 2, 5]), 2);
  assert.equal(reconnectDelay(9, [1, 2, 5]), 5, 'the last delay repeats forever');
});

// ── connection ─────────────────────────────────────────────────────────────

test('the client opens the stream route and seeds from its first frame', () => {
  const { states, source } = harness();
  assert.equal(source().url, STREAM_URL);

  source().emit('history', JSON.stringify(snapshot(2)));

  assert.equal(states.at(-1).samples.length, 2);
});

test('a malformed frame is ignored and the stream keeps delivering', () => {
  const { states, source } = harness();
  source().emit('history', JSON.stringify(snapshot(1)));

  source().emit('sample', '{"cpu": trunc');
  source().emit('services', 'not json at all');
  source().emit('sample', JSON.stringify(hostSample(42)));

  assert.equal(source().closed, false, 'a bad frame must not close the connection');
  const state = states.at(-1);
  assert.equal(state.samples.length, 2, 'only the parseable sample was appended');
  assert.equal(state.samples.at(-1).cpu.usage_pct, 42);
});

test('a lagged event re-fetches the snapshot instead of appending onto a hole', async () => {
  const asked = [];
  const { states, source } = harness({
    fetchImpl: async (url) => {
      asked.push(url);
      return { ok: true, json: async () => snapshot(5) };
    },
  });
  source().emit('history', JSON.stringify(snapshot(1)));
  assert.equal(states.at(-1).samples.length, 1);

  source().emit('lagged', JSON.stringify({ dropped: 17 }));
  await new Promise((resolve) => setImmediate(resolve));

  assert.deepEqual(asked, [HISTORY_URL]);
  assert.equal(states.at(-1).samples.length, 5, 'the window is replaced, not extended');
});

test('a failed resync leaves the window as it was', async () => {
  const { states, source } = harness({
    fetchImpl: async () => {
      throw new Error('offline');
    },
  });
  source().emit('history', JSON.stringify(snapshot(3)));

  source().emit('lagged', JSON.stringify({ dropped: 1 }));
  await new Promise((resolve) => setImmediate(resolve));

  assert.equal(states.at(-1).samples.length, 3);
});

test('an unexpected schema_version warns once and still renders', () => {
  const { states, warnings, source } = harness();

  source().emit('history', JSON.stringify(snapshot(2, { schema_version: 99 })));
  source().emit('history', JSON.stringify(snapshot(2, { schema_version: 99 })));

  assert.equal(warnings.length, 1, 'logged once, not once per frame');
  assert.match(warnings[0], /schema_version 99/);
  assert.equal(states.at(-1).samples.length, 2, 'the payload still rendered');
});

test('an error closes the source and schedules a redial; stop() cancels it', () => {
  const scheduled = [];
  FakeEventSource.instances = [];
  const client = createMachineStream({
    EventSourceImpl: FakeEventSource,
    setTimeoutImpl: (fn, ms) => {
      scheduled.push(ms);
      return scheduled.length;
    },
    clearTimeoutImpl: () => {},
    logger: { warn: () => {} },
  });
  client.start();
  const first = FakeEventSource.instances.at(-1);

  first.emit('error', null);

  assert.equal(first.closed, true);
  assert.deepEqual(scheduled, [1_000], 'first retry uses the shortest backoff');

  client.stop();
  assert.equal(client.getState().connected, false);
});
