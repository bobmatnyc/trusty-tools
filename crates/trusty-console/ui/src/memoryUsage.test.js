/**
 * Tests for the Memory tab's RAM and disk readings (#6928).
 *
 * Run: `node --test src/memoryUsage.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import { UNKNOWN_SIZE } from './bytes.js';
import {
  NO_BREAKDOWN_NOTE,
  NO_FOOTPRINT_NOTE,
  RAM_COMPONENTS,
  diskUsage,
  palaceDiskCell,
  ramUsage,
} from './memoryUsage.js';

/** A schema-5 payload from a macOS daemon, which supplies every ledger. */
const FULL = {
  ram_bytes: 1_815_939_688,
  ram_heap_bytes: 900_000_000,
  ram_file_backed_bytes: 800_000_000,
  ram_compressed_bytes: 115_939_688,
  disk_bytes: 1_797_156_864,
  data_root: '/Users/masa/Library/Application Support/trusty-memory',
};

test('RAM reads as a footprint plus all three ledgers, in byte units', () => {
  const ram = ramUsage(FULL);
  assert.equal(ram.footprintBytes, 1_815_939_688);
  assert.equal(ram.footprint, '1.69 GB');
  assert.equal(ram.reported, true);
  assert.equal(ram.note, null);
  assert.deepEqual(
    ram.components.map((c) => c.label),
    ['Heap', 'File-backed', 'Compressed'],
  );
  for (const c of ram.components) {
    assert.match(c.text, /\d (B|KB|MB|GB)$/, `${c.label} must carry a byte unit`);
  }
});

test('a footprint with no split is shown as a footprint, and says so', () => {
  // #7084's fallback: the OS supplies no `task_info` ledgers, so the three
  // keys are ABSENT from the payload rather than null.
  const ram = ramUsage({ ram_bytes: 1024 * 1024 });
  assert.equal(ram.footprint, '1.0 MB');
  assert.equal(ram.reported, false);
  assert.deepEqual(ram.components, [], 'a partial split is never rendered');
  assert.equal(ram.note, NO_BREAKDOWN_NOTE);
});

test('a partially-reported split is treated as no split at all', () => {
  // One `task_info` call supplies every ledger or none, so a payload carrying
  // two of three is a bug — rendering two would present it as the whole
  // picture.
  const ram = ramUsage({ ram_bytes: 10, ram_heap_bytes: 4, ram_compressed_bytes: 1 });
  assert.equal(ram.reported, false);
  assert.deepEqual(ram.components, []);
});

test('no footprint reads as unknown and says why — never as zero bytes', () => {
  const ram = ramUsage({ disk_bytes: 5 });
  assert.equal(ram.footprintBytes, null);
  assert.equal(ram.footprint, UNKNOWN_SIZE);
  assert.equal(ram.note, NO_FOOTPRINT_NOTE);
});

test('disk reads as the aggregate store size with the directory it measured', () => {
  const disk = diskUsage(FULL);
  assert.equal(disk.bytes, 1_797_156_864);
  assert.equal(disk.text, '1.67 GB');
  assert.equal(disk.dataRoot, FULL.data_root);
});

test('a pre-schema-5 payload reports an unknown store size, not an empty one', () => {
  const disk = diskUsage({ palace_count: 94 });
  assert.equal(disk.bytes, null);
  assert.equal(disk.text, UNKNOWN_SIZE);
  assert.equal(disk.dataRoot, null);
});

test('a real zero stays a zero', () => {
  assert.equal(diskUsage({ disk_bytes: 0 }).text, '0 B');
});

test('per-palace disk is the drill-down half of the same figure', () => {
  assert.equal(palaceDiskCell({ id: 'x', disk_bytes: 101_187_584 }), '96.5 MB');
  assert.equal(palaceDiskCell({ id: 'x' }), UNKNOWN_SIZE);
  assert.equal(palaceDiskCell(undefined), UNKNOWN_SIZE);
});

test('the ledger list is exactly the three #7084 names in a stable order', () => {
  assert.deepEqual(
    RAM_COMPONENTS.map((c) => c.field),
    ['ram_heap_bytes', 'ram_file_backed_bytes', 'ram_compressed_bytes'],
  );
});
