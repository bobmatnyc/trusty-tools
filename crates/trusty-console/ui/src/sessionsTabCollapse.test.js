/**
 * How the Sessions tab wires the #8282 collapse decision into its markup.
 *
 * Why a source-text test: this package installs no DOM and no component
 * runner — `node --test` is the whole harness (see `memoryTabDisplayOnly.test.js`
 * for the same split). The BEHAVIOUR of collapsing is pure and lives in
 * `sessionRows.test.js`; what cannot be asserted there is the wiring, and the
 * wiring is where this feature can go wrong in ways a user feels: rows left in
 * the DOM while the group reads as closed, a heading that is not a button, a
 * bulk action whose checkboxes outlive the rows they target, or a poll that
 * reopens what the viewer just closed.
 *
 * Run: `node --test src/sessionsTabCollapse.test.js` from
 * `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { fileURLToPath } from 'node:url';

const read = (name) =>
  readFileSync(fileURLToPath(new URL(`./${name}`, import.meta.url)), 'utf8');

const TAB = read('SessionsTab.svelte');
const HEADER = read('SessionGroupHeader.svelte');

test('the group header is a button carrying aria-expanded and aria-controls', () => {
  // A heading that toggles has to be operable by keyboard and announce its
  // state; `<h3 onclick>` does neither.
  assert.match(HEADER, /<button[\s\S]*?type="button"/);
  assert.match(HEADER, /aria-expanded=\{!collapsed\}/);
  assert.match(HEADER, /aria-controls=\{controls\}/);
  assert.match(HEADER, /onclick=\{onToggle\}/);
  // The tab passes an id that the region below actually carries, so
  // `aria-controls` resolves to a real element.
  assert.match(TAB, /controls=\{`session-group-\$\{st\}`\}/);
  assert.match(TAB, /id=\{`session-group-\$\{st\}`\}/);
});

test('a collapsed group renders its count but none of its rows', () => {
  // The count is unconditional in the header component — a collapsed group
  // whose header hid its count would say nothing about what it holds.
  assert.match(HEADER, /\{count\}/);
  assert.ok(!HEADER.includes('{#if'), 'the header renders unconditionally');
  assert.match(TAB, /count=\{grouped\[st\]\.length\}/);

  // The rows sit behind the guard, so a collapsed group contributes nothing to
  // the tab order or the accessibility tree.
  const guard = TAB.indexOf('{#if !collapsed}');
  const rows = TAB.indexOf('<div class="session-list">');
  assert.ok(guard > 0, 'the tab guards its rows on the collapsed flag');
  assert.ok(rows > guard, 'the row list renders inside the collapsed guard');
});

test('a collapsed errored group still reads as errored', () => {
  // The trade the owner ruling (2026-09-19) makes: `errored` folds away with
  // the other inactive ends, so the header is the ONLY thing left saying a
  // session failed. It therefore has to name the group, count it, and colour it
  // — all outside any collapse guard.
  assert.match(HEADER, /<span class="group-name">\{group\}<\/span>/);
  assert.match(HEADER, /<span class="group-count">\(\{count\}\)<\/span>/);
  assert.match(HEADER, /\.group-title\.errored \{ color: var\(--trusty-danger\); \}/);
  assert.match(HEADER, /<h3 class="group-title \{group\}">/);
  assert.ok(!HEADER.includes('{#if'), 'nothing in the header is conditional');

  // Second, independent signal: the supervisor bar's fleet-wide errored count
  // is rendered above the groups, so it cannot be inside a collapsed one.
  const supervisor = TAB.indexOf('<span class="count errored">');
  const firstGroup = TAB.indexOf('{#each GROUP_ORDER as st}');
  assert.ok(supervisor > 0, 'the supervisor bar reports an errored count');
  assert.ok(supervisor < firstGroup, 'that count renders before any group');
});

test('empty groups are still not rendered at all', () => {
  // Pre-existing behaviour the collapse must not turn into "an empty header".
  assert.match(TAB, /\{#if grouped\[st\] && grouped\[st\]\.length > 0\}/);
});

test('a toggle is persisted and nothing else writes the collapse state', () => {
  assert.match(TAB, /collapsedGroups = \$state\(readCollapsedGroups\(\)\)/);
  assert.match(TAB, /writeCollapsedGroups\(next\)/);
  // #8282 acceptance: a poll must not reopen a group. The state is assigned in
  // exactly two places — its declaration and the toggle — so no refresh path
  // can reach it. A third assignment fails here and has to justify itself.
  const assignments = [...TAB.matchAll(/^\s*(let\s+)?collapsedGroups = /gm)];
  assert.equal(assignments.length, 2, 'only the declaration and the toggle assign it');
  assert.ok(
    !/fetchAll[\s\S]{0,400}collapsedGroups/.test(TAB),
    'the refresh path does not touch the collapse state',
  );
});

test('the bulk-delete controls cannot outlive the rows they target', () => {
  // #6431's bulk bar is inside the collapsed region, and collapsing the bucket
  // drops the selection — so the action can never run against rows the viewer
  // is not looking at.
  const guard = TAB.indexOf('{#if !collapsed}');
  const bulkBar = TAB.indexOf('<div class="bulk-bar">');
  assert.ok(bulkBar > guard, 'the bulk bar renders inside the collapsed guard');
  assert.match(TAB, /if \(next\[group\] && group === OTHER_STATE\) clearSelection\(\)/);
});

test('the tab still offers no session filter, so no match can hide when collapsed', () => {
  // There is no filter/search box in this tab today, which is why the collapse
  // needs no auto-expand rule. This test is the guard on that premise: add one
  // and it fails, which is the prompt to decide how a match inside a collapsed
  // group surfaces. The count already tracks the rendered rows
  // (`count={grouped[st].length}`), so a filter applied before grouping shows
  // its matches in a collapsed header without further work.
  const inputs = [...TAB.matchAll(/<input[\s\S]*?\/>/g)].map((m) => m[0]);
  for (const tag of inputs) {
    assert.ok(
      !/filter|search|query/i.test(tag),
      `SessionsTab.svelte gained a filter control: ${tag.slice(0, 80)}`,
    );
  }
  assert.ok(
    !/\b(filterText|searchText|filterQuery|searchQuery)\b/.test(TAB),
    'SessionsTab.svelte gained filter state; decide how a collapsed group shows a match',
  );
});
