/**
 * Tests for the Memory tab's row-click target (#6928).
 *
 * Run: `node --test src/palaceNav.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  MEMORY_DASHBOARD_URL,
  NO_PALACE_ID_HINT,
  palaceDashboardHref,
  palaceRowAriaLabel,
  palaceRowHint,
} from './palaceNav.js';

test('a row links to the dashboard route that actually exists', () => {
  assert.equal(palaceDashboardHref('trusty-tools'), '/tools/memory/#/palace/trusty-tools');
  assert.ok(
    palaceDashboardHref('x').startsWith(MEMORY_DASHBOARD_URL),
    'the href must stay under the console-served dashboard mount',
  );
});

test('an id carrying a route separator is encoded, not spliced into the route', () => {
  // A raw `/` would land on `#/palace/a/b`, which the SPA reads as the graph
  // route's shape; a raw `#` would truncate the hash entirely.
  assert.equal(palaceDashboardHref('a/b'), '/tools/memory/#/palace/a%2Fb');
  assert.equal(palaceDashboardHref('a#b'), '/tools/memory/#/palace/a%23b');
});

test("a row's accessible name carries every cell it replaces", () => {
  const label = palaceRowAriaLabel({
    name: 'trusty-tools',
    id: 'trusty-tools',
    drawers: '412',
    disk: '96.4 MB',
    lastUsed: '3h ago',
  });
  for (const cell of ['trusty-tools', '412', '96.4 MB', '3h ago']) {
    assert.ok(label.includes(cell), `label omits ${cell}`);
  }
  assert.match(label, /open palace management$/);
});

test('a row with no id carries the hint, in both places it is rendered', () => {
  assert.equal(palaceRowHint({ name: 'nameless' }), NO_PALACE_ID_HINT);
  assert.equal(palaceRowHint({ id: '' }), NO_PALACE_ID_HINT, 'an empty id is no id');
  assert.equal(palaceRowHint(undefined), NO_PALACE_ID_HINT);
  assert.match(NO_PALACE_ID_HINT, /no management view/);
});

test('a row with an id carries no hint at all', () => {
  assert.equal(palaceRowHint({ id: 'trusty-tools' }), null);
});
