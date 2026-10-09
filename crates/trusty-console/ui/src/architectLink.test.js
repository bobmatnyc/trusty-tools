/**
 * Tests for the header's Architect dashboard link (#9474).
 *
 * Run: `node --test src/architectLink.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import {
  ARCHITECT_WINDOW_NAME,
  CONSOLE_WINDOW_NAME,
  dashboardHref,
  fetchArchitectDashboard,
} from './architectLink.js';

/** A `fetch` stand-in that answers one request with the given body. */
function respondWith(body, { ok = true } = {}) {
  return async () => ({ ok, json: async () => body });
}

test('a live dashboard URL from the server becomes the link', () => {
  const url = 'http://127.0.0.1:7890/';
  assert.equal(dashboardHref({ url }, '127.0.0.1'), url);
  assert.equal(dashboardHref({ url }, 'localhost'), url);
  assert.equal(dashboardHref({ url: 'http://[::1]:7890/' }, '[::1]'), 'http://[::1]:7890/');
});

test('no discovered dashboard means no link', () => {
  assert.equal(dashboardHref({ url: null }, '127.0.0.1'), null);
  assert.equal(dashboardHref({}, '127.0.0.1'), null);
  assert.equal(dashboardHref(undefined, '127.0.0.1'), null);
  assert.equal(dashboardHref({ url: 7890 }, '127.0.0.1'), null);
  assert.equal(dashboardHref({ url: '' }, '127.0.0.1'), null);
});

test('a non-loopback or non-http URL is refused', () => {
  for (const url of [
    'http://dashboard.invalid:7890/',
    'http://192.0.2.1:7890/',
    'http://127.0.0.1@dashboard.invalid/',
    'javascript:alert(1)',
    'file:///etc/passwd',
    'not a url',
  ]) {
    assert.equal(dashboardHref({ url }, '127.0.0.1'), null, url);
  }
});

test('a console page viewed from another machine shows no link', () => {
  // The dashboard binds this host's loopback; a tailnet viewer's 127.0.0.1 is
  // their own machine, so the link would be dead for them.
  assert.equal(dashboardHref({ url: 'http://127.0.0.1:7890/' }, '100.64.0.7'), null);
});

test('fetchArchitectDashboard reads the link off the route', async () => {
  const href = await fetchArchitectDashboard(
    respondWith({ url: 'http://127.0.0.1:7890/' }),
    '127.0.0.1',
  );
  assert.equal(href, 'http://127.0.0.1:7890/');
});

test('fetchArchitectDashboard resolves to null on every failure', async () => {
  const rejects = async () => {
    throw new Error('connection refused');
  };
  const unparseable = async () => ({
    ok: true,
    json: async () => {
      throw new SyntaxError('bad json');
    },
  });
  assert.equal(await fetchArchitectDashboard(rejects, '127.0.0.1'), null);
  assert.equal(await fetchArchitectDashboard(unparseable, '127.0.0.1'), null);
  assert.equal(
    await fetchArchitectDashboard(respondWith({ url: 'http://127.0.0.1:7890/' }, { ok: false }), '127.0.0.1'),
    null,
  );
});

test('architect link targets the named window and never sets noopener', () => {
  const src = readFileSync(join(dirname(fileURLToPath(import.meta.url)), 'App.svelte'), 'utf8');
  const anchor = src.match(/<a\b[^>]*href=\{architectHref\}[^>]*>/s)?.[0];
  assert.ok(anchor, 'architect anchor present in App.svelte');
  assert.equal(ARCHITECT_WINDOW_NAME, 'architect-dashboard');
  assert.match(anchor, /target=\{ARCHITECT_WINDOW_NAME\}/);
  assert.doesNotMatch(anchor, /_blank|rel=|noopener|noreferrer/);
  assert.equal(CONSOLE_WINDOW_NAME, 'trusty-console');
  assert.match(src, /window\.name = CONSOLE_WINDOW_NAME;/);
});
