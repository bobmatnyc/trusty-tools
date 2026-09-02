/**
 * Guards the console page against a component-local `body` rule that clips the
 * whole document (#6658).
 *
 * Why: Vite emits ONE CSS bundle for this SPA, and every component's `<style>`
 * lands in it whether that component mounts or not. A `:global(body)` rule in a
 * full-screen route therefore reaches every tab. Screensaver.svelte's
 * `overflow: hidden` sat later in the bundle than App.svelte's `body` rule at
 * equal specificity, so no console page scrolled.
 *
 * What: reads the built bundle under `dist/assets/` and asserts no UNSCOPED
 * `body` selector clips overflow, then asserts the same at source level so the
 * next `:global(body)` is caught before a rebuild hides it. A full-screen route
 * clips its own root element instead — `.saver` is `position: fixed; inset: 0;
 * overflow: hidden`, which needs nothing from `body`.
 *
 * Run: `node --test src/bodyOverflow.test.js` from `crates/trusty-console/ui`.
 * No test runner is installed in this package; `node --test` is built in.
 */

import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, join } from 'node:path';
import { fileURLToPath } from 'node:url';

const SRC_DIR = dirname(fileURLToPath(import.meta.url));
const DIST_ASSETS = join(SRC_DIR, '..', 'dist', 'assets');

/** Every declaration block in `css`, paired with its selector list. */
function rules(css) {
  // `[^{}]` cannot cross a brace, so this matches innermost blocks only and
  // picks up rules nested in an @media without also matching the @media itself.
  const found = [];
  for (const [, selectors, declarations] of css.matchAll(/([^{}]+)\{([^{}]*)\}/g)) {
    found.push({ selectors: selectors.trim(), declarations });
  }
  return found;
}

/** Whether a selector list targets `body` with nothing narrowing it. */
function targetsBareBody(selectors) {
  return selectors
    .split(',')
    .map((s) => s.trim())
    .some((s) => /(^|[\s>+~])body$/.test(s));
}

/** Whether a declaration block clips overflow on either axis. */
function clipsOverflow(declarations) {
  return /(^|;)\s*overflow(-[xy])?\s*:\s*(hidden|clip)/.test(declarations);
}

test('the built CSS bundle has no unscoped body rule that clips overflow', () => {
  const bundles = readdirSync(DIST_ASSETS).filter((f) => f.endsWith('.css'));
  assert.ok(
    bundles.length > 0,
    `no CSS bundle under ${DIST_ASSETS} — run \`pnpm build\` before this test`,
  );

  const offenders = [];
  for (const bundle of bundles) {
    const css = readFileSync(join(DIST_ASSETS, bundle), 'utf8');
    for (const { selectors, declarations } of rules(css)) {
      if (targetsBareBody(selectors) && clipsOverflow(declarations)) {
        offenders.push(`${bundle}: ${selectors}{${declarations}}`);
      }
    }
  }

  assert.deepEqual(
    offenders,
    [],
    'an unscoped body rule clips the document, so no console tab can scroll:\n' +
      offenders.join('\n'),
  );
});

test('no component style clips overflow on a bare :global(body)', () => {
  const offenders = [];
  for (const file of readdirSync(SRC_DIR).filter((f) => f.endsWith('.svelte'))) {
    const source = readFileSync(join(SRC_DIR, file), 'utf8');
    for (const { selectors, declarations } of rules(source)) {
      // `:global(body)` compiles to a bare `body`; `:global(body:has(.saver))`
      // does not, and is the escape hatch when a route truly must clip.
      if (/:global\(\s*body\s*\)/.test(selectors) && clipsOverflow(declarations)) {
        offenders.push(`${file}: ${selectors.replace(/\s+/g, ' ')}`);
      }
    }
  }

  assert.deepEqual(
    offenders,
    [],
    "clip the route's own fixed-position root instead of the document:\n" +
      offenders.join('\n'),
  );
});
