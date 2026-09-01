/**
 * Tests for the header lockup's version descriptor.
 *
 * Run: `node --test src/consoleVersion.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  CONSOLE_DESCRIPTOR,
  describeConsole,
  fetchConsoleVersion,
  versionFrom,
} from './consoleVersion.js';

/** A `fetch` stand-in that answers one request with the given body. */
function respondWith(body, { ok = true } = {}) {
  return async () => ({ ok, json: async () => body });
}

test('a real /health body yields its version', () => {
  assert.equal(versionFrom({ status: 'ok', version: '0.9.2' }), '0.9.2');
  assert.equal(versionFrom({ version: '  0.9.2  ' }), '0.9.2');
});

test('anything that is not a non-empty string is no version', () => {
  // The defect this prevents: a non-string reaching the template renders as
  // "undefined" or "null" inside the brand lockup.
  assert.equal(versionFrom(undefined), null);
  assert.equal(versionFrom({}), null);
  assert.equal(versionFrom({ version: null }), null);
  assert.equal(versionFrom({ version: 92 }), null);
  assert.equal(versionFrom({ version: '' }), null);
  assert.equal(versionFrom({ version: '   ' }), null);
});

test('a known version is appended to the descriptor', () => {
  assert.equal(describeConsole('0.9.2'), 'UNIT-05 · SERVICE CONSOLE · v0.9.2');
});

test('an unknown version leaves the descriptor unchanged', () => {
  assert.equal(describeConsole(null), CONSOLE_DESCRIPTOR);
  assert.equal(describeConsole(undefined), CONSOLE_DESCRIPTOR);
  assert.equal(describeConsole(''), CONSOLE_DESCRIPTOR);
});

test('fetchConsoleVersion reads the version off a healthy response', async () => {
  const version = await fetchConsoleVersion(
    respondWith({ status: 'ok', version: '0.9.2' }),
  );
  assert.equal(version, '0.9.2');
});

test('every failure path resolves to null rather than rejecting', async () => {
  const rejects = async () => {
    throw new Error('connection refused');
  };
  const unparseable = async () => ({
    ok: true,
    json: async () => {
      throw new SyntaxError('not JSON');
    },
  });

  assert.equal(await fetchConsoleVersion(rejects), null);
  assert.equal(await fetchConsoleVersion(unparseable), null);
  assert.equal(await fetchConsoleVersion(respondWith({}, { ok: false })), null);
});

test('fetchConsoleVersion asks the server, not a build-time constant', async () => {
  // The bundle is committed, so a compiled-in version would go stale; the
  // probe must be an actual request to the running binary.
  const seen = [];
  await fetchConsoleVersion(async (url) => {
    seen.push(url);
    return { ok: true, json: async () => ({ version: '0.9.2' }) };
  });
  assert.deepEqual(seen, ['/health']);
});
