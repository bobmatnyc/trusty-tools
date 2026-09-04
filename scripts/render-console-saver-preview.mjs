// scripts/render-console-saver-preview.mjs
//
// Why: `TrustyConsole.saver` has two states that cannot show the live console —
//   the System Settings gallery tile (`isPreview`, which never builds a
//   WKWebView) and the pre-load/offline fallback. Both drew a text wordmark, so
//   the tile was a blank card and an outage looked like a dead screen (#6838,
//   #6839). They now draw a bundled PNG of the real services frame, and #6839
//   requires that PNG be regenerable from the live page rather than
//   hand-captured.
// What: drives the Chromium already cached by `website/`'s Playwright install
//   over `http://127.0.0.1:7788/ui/screensaver`, waits for the rotation to reach
//   the services frame with a populated roster, and writes a 1920x1080 PNG into
//   the saver source tree. `scripts/build-console-saver.sh` copies it into
//   `Contents/Resources/`.
// Test: `scripts/render-console-saver-preview.sh` is the entry point; the asset
//   it produces is asserted present and drawable by `PaintHarness.swift`'s
//   `preview` mode.
//
// Playwright is a devDependency of `website/`, not of the repo root, so it is
// resolved through that package rather than by this file's own directory —
// Node's resolver walks up from the IMPORTING file and would never reach
// `website/node_modules`.

import { createRequire } from 'node:module';
import { mkdirSync, statSync } from 'node:fs';
import path from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const REPO_ROOT = path.resolve(path.dirname(fileURLToPath(import.meta.url)), '..');

const CONSOLE_URL = process.env.CONSOLE_URL ?? 'http://127.0.0.1:7788/ui/screensaver';
const OUT_PATH =
  process.env.PREVIEW_OUT ??
  path.join(REPO_ROOT, 'crates/trusty-console/macos/saver/Resources/ConsolePreview.png');
const WIDTH = Number.parseInt(process.env.PREVIEW_WIDTH ?? '1920', 10);
const HEIGHT = Number.parseInt(process.env.PREVIEW_HEIGHT ?? '1080', 10);
// The services frame is the SECOND of two frames on a 20 s rotation
// (`ui/src/screensaver.js`, ROTATE_MS), so the wait has to outlast one full
// cycle plus the first poll.
const FRAME_TIMEOUT_MS = Number.parseInt(process.env.PREVIEW_TIMEOUT_MS ?? '90000', 10);

function note(message) {
  process.stderr.write(`RENDER: ${message}\n`);
}

function loadChromium() {
  const require = createRequire(pathToFileURL(path.join(REPO_ROOT, 'website/package.json')));
  let entry;
  try {
    entry = require.resolve('playwright');
  } catch (cause) {
    throw new Error(
      'playwright is not installed. Run `pnpm install` inside website/ first.',
      { cause },
    );
  }
  return import(pathToFileURL(entry)).then((mod) => (mod.chromium ?? mod.default?.chromium));
}

const chromium = await loadChromium();
if (!chromium) throw new Error('playwright resolved but exposes no `chromium` launcher');

note(`url=${CONSOLE_URL} viewport=${WIDTH}x${HEIGHT} out=${OUT_PATH}`);

const browser = await chromium.launch();
// `colorScheme: 'dark'` rather than a localStorage write: the page's own theme
// bootstrap resolves the default 'system' setting through
// `prefers-color-scheme`, and the saver's native fallback colours are the dark
// Foundry tokens, so a light capture would clash with the background it is
// drawn on.
const context = await browser.newContext({
  viewport: { width: WIDTH, height: HEIGHT },
  colorScheme: 'dark',
  deviceScaleFactor: 1,
  reducedMotion: 'reduce',
});
const page = await context.newPage();

let exitCode = 0;
try {
  const response = await page.goto(CONSOLE_URL, { waitUntil: 'domcontentloaded', timeout: 30_000 });
  if (!response || !response.ok()) {
    throw new Error(`GET ${CONSOLE_URL} returned ${response ? response.status() : '<no response>'}`);
  }

  // `table.saver-table` renders only on the services frame AND only once the
  // roster read has returned at least one row, so one selector covers both "the
  // rotation reached frame 1" and "the data arrived" — no sleep needed.
  note(`waiting up to ${FRAME_TIMEOUT_MS}ms for the services frame to render a populated roster`);
  await page.waitForSelector('table.saver-table tbody tr', {
    state: 'visible',
    timeout: FRAME_TIMEOUT_MS,
  });
  // One extra frame so the bar graphs finish their first paint before capture.
  await page.waitForTimeout(750);

  const rows = await page.locator('table.saver-table tbody tr').count();
  note(`services frame visible with ${rows} row(s)`);

  // "click for fullscreen" is an affordance of the live page. Baked into a
  // static image it advertises an interaction that does nothing, so it is the
  // one element the capture suppresses.
  await page.addStyleTag({ content: '.hint { display: none !important; }' });

  mkdirSync(path.dirname(OUT_PATH), { recursive: true });
  await page.screenshot({ path: OUT_PATH, type: 'png', fullPage: false });

  const bytes = statSync(OUT_PATH).size;
  note(`wrote ${OUT_PATH} (${bytes} bytes)`);
  process.stdout.write(`${OUT_PATH}\t${bytes}\n`);
} catch (error) {
  note(`FAILED — ${error instanceof Error ? error.message : String(error)}`);
  exitCode = 1;
} finally {
  await context.close();
  await browser.close();
}

process.exit(exitCode);
