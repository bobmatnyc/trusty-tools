/**
 * Tests for the console's navigation model, and the guard that keeps the top
 * tab bar deleted (#6909).
 *
 * Why: the owner's ruling — "the top nav is redundant with the service list,
 * remove it unless there is a separate purpose it serves" — is a claim about
 * markup, and markup is what a later edit would quietly put back. The Services
 * list navigates to every service view, so the only things the header still
 * owes are an entry point for Config (the one view with no Services row) and a
 * way back to the Overview. Those three facts are asserted here rather than
 * left to a screenshot.
 *
 * What: `viewLabel` is exercised directly; the rest read `App.svelte` and the
 * built bundle. No test runner is installed in this package — `node --test` is
 * built in — and it cannot mount a Svelte component, so the markup assertions
 * parse the source the way `bodyOverflow.test.js` does.
 *
 * Run: `node --test src/consoleNav.test.js` from `crates/trusty-console/ui`.
 * The bundle test needs `pnpm build` to have run.
 */

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

import { VIEW_LABELS, viewLabel } from './consoleNav.js';

const SRC_DIR = dirname(fileURLToPath(import.meta.url));
const DIST_ASSETS = join(SRC_DIR, '..', 'dist', 'assets');
const APP = readFileSync(join(SRC_DIR, 'App.svelte'), 'utf8');

/**
 * Every `<button …>…</button>` in `source`, as its attributes and its text.
 *
 * A Svelte handler is `onclick={() => …}`, so the tag's attributes contain the
 * `>` of an arrow function. The open tag therefore ends at the first `>` at
 * brace depth zero, not at the first `>` — a `[^>]*` attribute match stops
 * inside the handler and reports the button as unlabelled.
 */
function buttons(source) {
  const found = [];
  for (const open of source.matchAll(/<button\b/g)) {
    let depth = 0;
    let i = open.index + open[0].length;
    for (; i < source.length; i += 1) {
      const c = source[i];
      if (c === '{') depth += 1;
      else if (c === '}') depth -= 1;
      else if (c === '>' && depth === 0) break;
    }
    const close = source.indexOf('</button>', i);
    if (close === -1) continue;
    found.push({
      attrs: source.slice(open.index + open[0].length, i).replace(/\s+/g, ' ').trim(),
      text: source
        .slice(i + 1, close)
        .replace(/<[^>]*>/g, ' ')
        .replace(/\s+/g, ' ')
        .trim(),
    });
  }
  return found;
}

/** A `<button>` whose click handler assigns `view = '<target>'`. */
function buttonSetting(view) {
  const assigns = new RegExp(`view\\s*=\\s*'${view}'`);
  return buttons(APP).find((b) => assigns.test(b.attrs));
}

// ── the view model ─────────────────────────────────────────────────────────

test('viewLabel names each view the panel renders', () => {
  assert.equal(viewLabel('overview'), 'Overview');
  assert.equal(viewLabel('sessions'), 'MPM Sessions');
  assert.equal(viewLabel('config'), 'Config');
});

test('an unknown view id reads as the Overview, never as undefined', () => {
  assert.equal(viewLabel('no-such-view'), 'Overview');
  assert.equal(viewLabel(undefined), 'Overview');
});

test('every view a Services row opens has a label and a panel branch', () => {
  const map = APP.match(/const SERVICE_TAB_MAP = \{([\s\S]*?)\};/);
  assert.ok(map, 'SERVICE_TAB_MAP is gone from App.svelte — the Services list needs it');

  const targets = [...map[1].matchAll(/:\s*'([a-z-]+)'/g)].map(([, v]) => v);
  assert.ok(targets.length >= 5, `expected the five service views, found ${targets.length}`);

  for (const target of targets) {
    assert.ok(target in VIEW_LABELS, `no label for the '${target}' view`);
    assert.match(
      APP,
      new RegExp(`view === '${target}'`),
      `the panel renders no branch for the '${target}' view`,
    );
  }
});

// ── the tab bar stays deleted ──────────────────────────────────────────────

test('App.svelte renders no tab bar', () => {
  const offenders = ['role="tablist"', 'role="tab"', 'tab-btn', 'class="tabs"'].filter((s) =>
    APP.includes(s),
  );
  assert.deepEqual(
    offenders,
    [],
    'the Services list is the navigation (#6909) — these belong to the removed tab bar:\n' +
      offenders.join('\n'),
  );
});

test('the built bundle carries no tab bar', () => {
  const assets = readdirSync(DIST_ASSETS).filter((f) => /\.(js|css)$/.test(f));
  assert.ok(
    assets.length > 0,
    `no bundle under ${DIST_ASSETS} — run \`pnpm build\` before this test`,
  );

  const offenders = [];
  for (const asset of assets) {
    const built = readFileSync(join(DIST_ASSETS, asset), 'utf8');
    for (const marker of ['tablist', 'tab-btn']) {
      if (built.includes(marker)) offenders.push(`${asset}: ${marker}`);
    }
  }
  assert.deepEqual(offenders, [], 'a tab bar reached the shipped bundle:\n' + offenders.join('\n'));
});

// ── what the header owes instead ───────────────────────────────────────────

test('Config keeps a button entry point, since no Services row opens it', () => {
  const action = buttonSetting('config');
  assert.ok(action, "no <button> opens the Config view — it has no Services row to open it");
  assert.match(action.text, /Config/, 'the Config action is unlabelled');
  assert.match(action.attrs, /class="header-action"/, 'the Config action is not a header action');
});

test('a detail view offers a keyboard-reachable way back to the Overview', () => {
  const back = buttonSetting('overview');
  assert.ok(back, 'no <button> returns to the Overview — a detail view is a dead end');
  assert.match(back.text, /Overview/, 'the way back is unlabelled');
  assert.match(APP, /aria-label="Breadcrumb"/, 'the way back is not announced as a breadcrumb');
});
