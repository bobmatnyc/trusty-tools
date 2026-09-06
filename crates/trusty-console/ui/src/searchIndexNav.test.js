/**
 * Tests for the Search tab's row-click target (#6923).
 *
 * Run: `node --test src/searchIndexNav.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  SEARCH_DASHBOARD_URL,
  indexDashboardHref,
  indexRowAriaLabel,
} from './searchIndexNav.js';

test('a row links to the dashboard route that actually exists', () => {
  assert.equal(
    indexDashboardHref('trusty-tools'),
    '/tools/search/#/indexes/trusty-tools/config',
  );
  assert.ok(
    indexDashboardHref('x').startsWith(SEARCH_DASHBOARD_URL),
    'the href must stay under the console-served dashboard mount',
  );
});

test('an id carrying a route separator is encoded, not spliced into the route', () => {
  // A raw `/` would land on `#/indexes/a/b/config`, which the SPA reads as a
  // different index; a raw `#` would truncate the hash entirely.
  assert.equal(
    indexDashboardHref('a/b'),
    '/tools/search/#/indexes/a%2Fb/config',
  );
  assert.equal(
    indexDashboardHref('a#b'),
    '/tools/search/#/indexes/a%23b/config',
  );
});

test("a row's accessible name carries every cell it replaces", () => {
  const label = indexRowAriaLabel({
    id: 'trusty-tools',
    rootPath: '/Users/masa/trusty-tools',
    size: '1.20 GB',
    lastUsed: '3h ago',
  });
  assert.equal(
    label,
    'Index trusty-tools, /Users/masa/trusty-tools, 1.20 GB, last used 3h ago — open index management',
  );
  for (const cell of ['trusty-tools', '/Users/masa/trusty-tools', '1.20 GB', '3h ago']) {
    assert.ok(label.includes(cell), `label omits ${cell}`);
  }
});
