/**
 * The Memory tab renders no control that mutates daemon state (#6928).
 *
 * Why a source-text test: the closure condition is about what the component
 * tree does NOT contain, and `node --test` cannot mount a Svelte component to
 * walk that tree. What it can do is read the single file the tree is built
 * from and assert the absence of every way this tab could reach a mutating
 * surface — which is stronger than a rendered-DOM check anyway, because it
 * fails on a mutating control that happens to be behind an `{#if}`.
 *
 * The rule this pins: the tab may READ. Its refresh button and its sort header
 * are controls, and neither changes anything on the daemon. What it may not do
 * is issue a mutating request or import a component that does.
 *
 * Run: `node --test src/memoryTabDisplayOnly.test.js` from
 * `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const SOURCE = readFileSync(
  fileURLToPath(new URL('./MemoryTab.svelte', import.meta.url)),
  'utf8',
);

test('the tab issues no request that is not a metrics read', () => {
  const targets = [...SOURCE.matchAll(/fetch\(\s*'([^']*)'/g)].map((m) => m[1]);
  assert.ok(targets.length > 0, 'the tab must still read its metrics');
  for (const target of targets) {
    assert.equal(
      target,
      '/api/console/metrics/memory',
      `the tab fetches ${target}; the only endpoint it may reach is the metrics read`,
    );
  }
});

test('the tab names no mutating HTTP method', () => {
  // A `fetch` with no `method` is a GET. Naming one of these is the only way
  // this file could ask the daemon to change something.
  for (const verb of ['DELETE', 'POST', 'PUT', 'PATCH']) {
    assert.ok(
      !SOURCE.includes(verb),
      `MemoryTab.svelte names ${verb}; a display-only tab issues none`,
    );
  }
});

test('the tab imports no action component', () => {
  // `DeleteAction` and `CompactAction` are the two that used to live here
  // (#6360, #6371); the pattern catches any successor named the same way.
  const imports = [...SOURCE.matchAll(/^\s*import\s+(\w+)\s+from\s+'([^']+)'/gm)];
  for (const [, name, path] of imports) {
    assert.ok(
      !/Action\.svelte$/.test(path),
      `MemoryTab.svelte imports ${name} from ${path}; actions live on /tools/memory`,
    );
  }
});

test('the tab reaches no palace-management route', () => {
  // The console's own mutating routes for a palace. Neither may be named here
  // even in a comment-free string, because naming one is how it comes back.
  for (const route of ['/api/console/memory/palaces', '/compact', '/reembed']) {
    assert.ok(
      !SOURCE.includes(route),
      `MemoryTab.svelte references ${route}; that surface belongs to the dashboard`,
    );
  }
});

test('the tab still links every palace row to the dashboard', () => {
  // The other half of the ruling: display-only is not the same as inert.
  assert.ok(
    SOURCE.includes('palaceDashboardHref'),
    'a palace row must open its management view on the dashboard',
  );
  assert.ok(
    SOURCE.includes('disk_bytes') || SOURCE.includes('palaceDiskCell'),
    'a palace row must carry its own disk size',
  );
});
