import { test, expect } from '@playwright/test';

// Why (#3894; PR #3895 code-critic MEDIUM-4): the instructions editor's size
// is the one property of this feature that has now shipped wrong twice — a
// 2-line strip (#3826), then a `70vh` cap that left dead space in the
// full-pane takeover (#3862). Both slipped through because nothing MEASURED
// it: jsdom does no layout, so the unit tests can only assert the classes
// that are supposed to produce the size. This spec measures the real rendered
// pixel height in a real browser, at two viewport heights, and fails if the
// editor stops tracking the space available.
// What: Boots the real app with every `/api/*` route stubbed (same technique
// and rationale as `smoke.spec.ts` — chunked responses and a live EventSource
// both stall Playwright's CDP channel), opens the takeover from the chat
// header's gear, and asserts the editor grows with the viewport, is the only
// scroll container in its tab, and covers the full content area.
// Test: `pnpm dev` in one shell, then
//   `TAGENT_URL=http://127.0.0.1:5173 pnpm test tests/config-takeover.spec.ts`
// It also runs unchanged against a real `tagent --api` on :7654 (the default
// baseURL). Not wired into CI — `pnpm test` is excluded there by design (it
// needs a running server plus browser binaries); see ci.yml's ui-checks
// header.

const PERSONA = Array.from(
  { length: 400 },
  (_, i) => `line ${i + 1}: you are a careful assistant.`,
).join('\n');

test.beforeEach(async ({ page }) => {
  // See smoke.spec.ts: a real EventSource keeps Chromium's network layer
  // non-idle and blocks CDP once the app reaches ready.
  await page.addInitScript(() => {
    class MockEventSource extends EventTarget {
      constructor(url) {
        super();
        this.url = url;
        this.readyState = 0;
        this.onopen = null;
        this.onmessage = null;
        this.onerror = null;
      }
      close() {
        this.readyState = 2;
      }
    }
    window.EventSource = MockEventSource;
  });

  await page.route('**', (route) => {
    // Match on the PATH, and only under `/api/` — against a Vite dev server
    // the app's own modules are served from `/src/stores/…`, which a
    // substring match on "/stores" would happily answer with JSON.
    const path = new URL(route.request().url()).pathname;
    const json = (body: unknown) =>
      route.fulfill({
        status: 200,
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify(body),
      });

    if (!path.startsWith('/api/')) {
      route.continue(); // HTML, JS bundle, CSS, fonts
    } else if (path.startsWith('/api/events')) {
      route.fulfill({
        status: 200,
        headers: { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' },
        body: '',
      });
    } else if (path.startsWith('/api/health')) json({ status: 'ok' });
    else if (path.startsWith('/api/config')) json({ auth_required: false });
    else if (path.startsWith('/api/tasks')) json([]);
    else if (path.endsWith('/persona')) json({ content: PERSONA, editable: true });
    else if (path.endsWith('/stores')) json({ stores: [], issues: [] });
    else if (path.match(/^\/api\/agents\/[^/]+$/))
      json({ name: 'ctrl', display_name: 'Concierge', tools_allow: [], scopes: [] });
    else if (path.startsWith('/api/agents'))
      json({ agents: [{ name: 'izzie', display_name: 'Izzie' }] });
    else json({});
  });
});

/** Boots the app, opens the takeover, and returns the editor locator. */
async function openTakeover(page) {
  await page.goto('/', { waitUntil: 'domcontentloaded' });
  await page.locator('[aria-label="Configure agent"]').click({ timeout: 15_000 });
  const editor = page.locator('[data-instructions-editor]');
  await expect(editor).toHaveValue(/line 400/, { timeout: 10_000 });
  return editor;
}

test.describe('agent configuration takeover (#3894)', () => {
  for (const height of [900, 1400]) {
    test(`instructions editor fills the pane at a ${height}px viewport`, async ({ page }) => {
      await page.setViewportSize({ width: 1440, height });
      const editor = await openTakeover(page);

      const measured = await editor.evaluate((el: HTMLTextAreaElement) => {
        const style = getComputedStyle(el);
        const column = el.parentElement!;
        const last = column.lastElementChild!;
        return {
          height: el.getBoundingClientRect().height,
          lineHeight: parseFloat(style.lineHeight),
          scrolls: el.scrollHeight > el.clientHeight,
          fontFamily: style.fontFamily,
          maxHeight: style.maxHeight,
          minHeight: style.minHeight,
          // Free space left over at the bottom of the tab column. A growing
          // editor consumes all of it; anything that caps the editor (a `vh`
          // clamp, a fixed height, `rows`) leaves a dead band here.
          deadSpaceBelow:
            column.getBoundingClientRect().bottom - last.getBoundingClientRect().bottom,
        };
      });

      // Half the viewport is a floor no "too small" report can survive.
      expect(measured.height).toBeGreaterThan(height * 0.5);
      expect(measured.height / measured.lineHeight).toBeGreaterThan(20);
      // A 400-line persona must scroll INSIDE the editor.
      expect(measured.scrolls).toBe(true);
      expect(measured.fontFamily.toLowerCase()).toContain('mono');
      // The exact regression #3862 introduced: a viewport clamp caps the
      // editor independently of the space available. Resolved computed values
      // — `70vh` shows up here as a pixel figure, `none` is the only pass.
      expect(measured.maxHeight).toBe('none');
      expect(measured.minHeight).toBe('0px');
      expect(measured.deadSpaceBelow).toBeLessThan(4);
    });
  }

  test('the editor tracks the viewport instead of being clamped', async ({ page }) => {
    // The assertion that actually catches a `vh` clamp: at 70vh the editor
    // would gain only ~350px across this 500px viewport change, and a fixed
    // `rows`/height would gain none. Growing nearly 1:1 is the property the
    // owner asked for.
    await page.setViewportSize({ width: 1440, height: 900 });
    const editor = await openTakeover(page);
    const measure = () => editor.evaluate((el: HTMLElement) => el.getBoundingClientRect().height);

    const short = await measure();
    await page.setViewportSize({ width: 1440, height: 1400 });
    await expect.poll(measure).toBeGreaterThan(short);
    const tall = await measure();

    expect(tall - short).toBeGreaterThan(450);
  });

  test('the editor is the only scroll container in its tab', async ({ page }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    const editor = await openTakeover(page);

    const ancestorScrollers = await editor.evaluate((el: HTMLElement) => {
      const found: string[] = [];
      const stop = el.closest('[data-config-takeover]');
      for (let n = el.parentElement; n && n !== stop; n = n.parentElement) {
        const s = getComputedStyle(n);
        if (['auto', 'scroll'].includes(s.overflowY) && n.scrollHeight > n.clientHeight + 1) {
          found.push(n.className);
        }
      }
      return found;
    });
    expect(ancestorScrollers).toEqual([]);
  });

  test('the takeover covers the full content area and Esc returns to an intact chat', async ({
    page,
  }) => {
    await page.setViewportSize({ width: 1440, height: 900 });
    await page.goto('/', { waitUntil: 'domcontentloaded' });

    const composer = page.locator('footer textarea');
    await composer.fill('half-written thought', { timeout: 15_000 });
    const chatBox = await page.locator('[data-chat-surface]').boundingBox();

    await page.locator('[aria-label="Configure agent"]').click();
    const overlay = page.locator('[data-config-takeover]');
    await expect(overlay).toBeVisible();

    const overlayBox = await overlay.boundingBox();
    // Covers the chat column AND the recap rail — the whole content area.
    expect(Math.round(overlayBox!.width)).toBe(Math.round(chatBox!.width));
    expect(Math.round(overlayBox!.height)).toBe(Math.round(chatBox!.height));

    await page.keyboard.press('Escape');
    await expect(overlay).toHaveCount(0);
    await expect(composer).toHaveValue('half-written thought');
  });
});
