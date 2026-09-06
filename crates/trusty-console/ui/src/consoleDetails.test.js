/**
 * Tests for the console's own details pane and the schema constant it depends
 * on (#6908).
 *
 * Why: four claims here are the ones a later edit would quietly break. The
 * schema constant must track the daemon's, or every page load logs a warning
 * nobody reads. The `trusty-console` row must resolve to a view, or the row the
 * previous slice added is the one inert entry on a list whose whole affordance
 * is that rows open something. The pane must render the watcher count the
 * snapshot carries, and it must state what is not built in one line rather than
 * growing empty widgets for it.
 *
 * What: the pure functions are exercised directly; the wiring claims read
 * `App.svelte` and the built bundle the way `consoleNav.test.js` does, because
 * no test runner here can mount a Svelte component.
 *
 * Run: `node --test src/consoleDetails.test.js` from `crates/trusty-console/ui`.
 * The bundle test needs `pnpm build` to have run.
 */

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  CONSOLE_SERVICE_ID,
  DEFERRED_LINE,
  consoleDetailCards,
  consoleHeading,
  consoleRow,
  formatStreamCount,
  formatUptime,
} from './consoleDetails.js';
import { EXPECTED_SCHEMA_VERSION, applyHistory, initialState } from './machineStream.js';
import { fetchConsoleHealth, uptimeFrom } from './consoleVersion.js';
import { serviceRows } from './servicesList.js';

const SRC_DIR = dirname(fileURLToPath(import.meta.url));
const DIST_ASSETS = join(SRC_DIR, '..', 'dist', 'assets');
const APP = readFileSync(join(SRC_DIR, 'App.svelte'), 'utf8');
const PANE = readFileSync(join(SRC_DIR, 'ConsoleTab.svelte'), 'utf8');
const HISTORY_RS = join(
  SRC_DIR,
  '..',
  '..',
  'src',
  'machine_history',
  'mod.rs',
);

// ── the schema constant ────────────────────────────────────────────────────

test('the client expects schema 4, the version the daemon now serves', () => {
  assert.equal(EXPECTED_SCHEMA_VERSION, 4);
});

test('the expected schema version is the one the daemon compiles in', () => {
  const rust = readFileSync(HISTORY_RS, 'utf8');
  const declared = rust.match(/MACHINE_HISTORY_SCHEMA_VERSION:\s*u32\s*=\s*(\d+)/);
  assert.ok(declared, 'MACHINE_HISTORY_SCHEMA_VERSION is gone from machine_history/mod.rs');
  assert.equal(
    Number(declared[1]),
    EXPECTED_SCHEMA_VERSION,
    'the client would log a schema warning on every page load against this daemon',
  );
});

test('a schema-4 snapshot carries its stream count into the store', () => {
  const next = applyHistory(initialState(), {
    samples: [],
    service_samples: {},
    sample_capacity: 600,
    service_sample_capacity: 600,
    sample_interval_secs: 1,
    sse_client_count: 3,
    schema_version: 4,
  });
  assert.equal(next.sseClientCount, 3);
});

test('a snapshot with no stream count reports null, not zero watchers', () => {
  const seeded = applyHistory(initialState(), {
    samples: [],
    service_samples: {},
    sse_client_count: 2,
    schema_version: 4,
  });
  const older = applyHistory(seeded, { samples: [], service_samples: {}, schema_version: 3 });
  assert.equal(older.sseClientCount, null, 'an absent count must not carry the old one forward');
});

// ── the roster row resolves to a view ──────────────────────────────────────

test("the console's roster row maps to a view App.svelte renders", () => {
  const map = APP.match(/const SERVICE_TAB_MAP = \{([\s\S]*?)\};/);
  assert.ok(map, 'SERVICE_TAB_MAP is gone from App.svelte');

  const entry = map[1].match(
    new RegExp(`'${CONSOLE_SERVICE_ID}'\\s*:\\s*'([a-z-]+)'`),
  );
  assert.ok(entry, `no SERVICE_TAB_MAP entry for '${CONSOLE_SERVICE_ID}' — the row stays inert`);
  assert.match(
    APP,
    new RegExp(`view === '${entry[1]}'`),
    `the panel renders no branch for the '${entry[1]}' view`,
  );
  assert.match(APP, /<ConsoleTab\b/, 'the console view renders no ConsoleTab');
});

// ── what the pane shows ────────────────────────────────────────────────────

test('formatUptime picks the two largest units it fills', () => {
  assert.equal(formatUptime(45), '45s');
  assert.equal(formatUptime(125), '2m 5s');
  assert.equal(formatUptime(3 * 3600 + 12 * 60), '3h 12m');
  assert.equal(formatUptime(2 * 86400 + 5 * 3600), '2d 5h');
});

test('formatUptime dashes an unreported uptime', () => {
  for (const value of [null, undefined, -1, NaN, '600']) {
    assert.equal(formatUptime(value), '—', `${String(value)} is not a duration`);
  }
});

test('formatStreamCount prints zero as a real count and dashes an absent one', () => {
  assert.equal(formatStreamCount(0), '0');
  assert.equal(formatStreamCount(4), '4');
  assert.equal(formatStreamCount(null), '—');
  assert.equal(formatStreamCount(undefined), '—');
});

test('uptimeFrom reads whole seconds and rejects a non-duration', () => {
  assert.equal(uptimeFrom({ uptime_secs: 91.7 }), 91);
  assert.equal(uptimeFrom({ uptime_secs: 0 }), 0);
  assert.equal(uptimeFrom({ uptime_secs: -3 }), null);
  assert.equal(uptimeFrom({ uptime_secs: '90' }), null);
  assert.equal(uptimeFrom({}), null);
});

test('fetchConsoleHealth reads both fields, and nulls a failed probe', async () => {
  const ok = async () => ({
    ok: true,
    json: async () => ({ status: 'ok', version: '0.4.1', uptime_secs: 7200 }),
  });
  assert.deepEqual(await fetchConsoleHealth(ok), { version: '0.4.1', uptimeSecs: 7200 });

  const dead = async () => {
    throw new Error('offline');
  };
  assert.deepEqual(await fetchConsoleHealth(dead), { version: null, uptimeSecs: null });
});

test('consoleRow finds the console among the roster rows', () => {
  const rows = serviceRows([
    { id: 'trusty-search', display_name: 'Trusty Search', status: 'running' },
    { id: CONSOLE_SERVICE_ID, display_name: 'Trusty Console', status: 'running', version: '0.4.1' },
  ]);
  assert.equal(consoleRow(rows)?.id, CONSOLE_SERVICE_ID);
  assert.equal(consoleRow([]), null);
});

test('the details cards render the snapshot stream count and the roster figures', () => {
  const rows = serviceRows(
    [{ id: CONSOLE_SERVICE_ID, display_name: 'Trusty Console', status: 'running' }],
    { [CONSOLE_SERVICE_ID]: [{ id: CONSOLE_SERVICE_ID, cpu_pct: 2.5, rss_bytes: 40 * 1024 * 1024 }] },
  );
  const cards = consoleDetailCards({
    row: consoleRow(rows),
    uptimeSecs: 3 * 3600 + 12 * 60,
    sseClientCount: 3,
  });

  assert.deepEqual(
    cards.map((c) => c.key),
    ['uptime', 'streams', 'cpu', 'memory'],
    'CPU and memory must be the last two cards so their graphs sit side by side',
  );
  const byKey = Object.fromEntries(cards.map((c) => [c.key, c]));
  assert.equal(byKey.uptime.value, '3h 12m');
  assert.equal(byKey.streams.value, '3');
  assert.equal(byKey.cpu.value, '2.5%');
  assert.equal(byKey.memory.value, '40 MiB');
  assert.equal(byKey.cpu.graph, 'cpu');
  assert.equal(byKey.memory.graph, 'memory');
});

test('every figure nothing reported reads as a dash, never a zero', () => {
  const cards = consoleDetailCards();
  for (const card of cards) {
    assert.equal(card.value, '—', `${card.key} invented a measurement`);
  }
});

test('consoleHeading names the version when one is known', () => {
  assert.equal(consoleHeading('0.4.1'), 'Trusty Console · v0.4.1');
  assert.equal(consoleHeading(null), 'Trusty Console');
});

// ── what is deferred says so once ──────────────────────────────────────────

test('the deferred line names all three subsystems that have no transport', () => {
  for (const subsystem of ['bus status', 'service connections', 'message rates']) {
    assert.match(DEFERRED_LINE, new RegExp(subsystem, 'i'), `${subsystem} is unaccounted for`);
  }
  assert.match(DEFERRED_LINE, /not yet available/i);
});

test('the pane states the deferred line rather than building placeholder panels', () => {
  assert.match(PANE, /DEFERRED_LINE/, 'the pane does not render the deferred line');
  const offenders = ['Message Rates', 'Service Connections', 'Bus Status'].filter((label) =>
    PANE.includes(`label="${label}"`),
  );
  assert.deepEqual(
    offenders,
    [],
    'these wait on the #6460 transport — one line, not empty widgets:\n' + offenders.join('\n'),
  );
});

// ── #6911's guard, kept ────────────────────────────────────────────────────

test('adding the console view put no tab bar back', () => {
  const offenders = ['role="tablist"', 'role="tab"', 'tab-btn', 'class="tabs"'].filter((s) =>
    APP.includes(s),
  );
  assert.deepEqual(
    offenders,
    [],
    'the Services list is the navigation (#6909, #6911):\n' + offenders.join('\n'),
  );
});

test('the built bundle carries the details pane and still no tab bar', () => {
  const assets = readdirSync(DIST_ASSETS).filter((f) => /\.(js|css)$/.test(f));
  assert.ok(
    assets.length > 0,
    `no bundle under ${DIST_ASSETS} — run \`pnpm build\` before this test`,
  );

  const js = assets
    .filter((f) => f.endsWith('.js'))
    .map((f) => readFileSync(join(DIST_ASSETS, f), 'utf8'))
    .join('\n');
  assert.ok(
    js.includes('Browser Streams'),
    'the shipped bundle has no details pane — run `pnpm build` after editing the UI',
  );
  assert.ok(js.includes('not yet available'), 'the deferred line did not reach the bundle');
  for (const marker of ['tablist', 'tab-btn']) {
    assert.ok(!js.includes(marker), `a tab bar reached the shipped bundle: ${marker}`);
  }
});
