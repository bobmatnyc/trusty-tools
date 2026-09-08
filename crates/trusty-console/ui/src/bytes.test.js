/**
 * Tests for the shared byte formatter (#6928).
 *
 * Run: `node --test src/bytes.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import { UNKNOWN_SIZE, formatBytes } from './bytes.js';

test('each unit boundary reads in the unit above it', () => {
  assert.equal(formatBytes(0), '0 B');
  assert.equal(formatBytes(1023), '1023 B');
  assert.equal(formatBytes(1024), '1.0 KB');
  assert.equal(formatBytes(1024 * 1024), '1.0 MB');
  assert.equal(formatBytes(1024 * 1024 * 1024), '1.00 GB');
  // The live palace store at the time of #6928's ruling.
  assert.equal(formatBytes(1_797_156_864), '1.67 GB');
});

test('an unreadable figure is unknown, never zero bytes', () => {
  // A `0 B` where the daemon reported nothing would read as a measured empty
  // store — the exact confusion this distinction exists to prevent.
  for (const bad of [null, undefined, NaN, Infinity, -1, '1024']) {
    assert.equal(formatBytes(bad), UNKNOWN_SIZE, `${bad} must not format as a size`);
  }
});
