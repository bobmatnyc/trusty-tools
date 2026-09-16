import { test, expect } from '@playwright/test';

// Why: Three issues blocked Playwright CDP (page.content(), locators, evaluate())
// after the app bootstrapped, requiring both app and test fixes:
//
// 1. afterUpdate+tick() infinite loop (primary blocker): ChatView.svelte had
//    afterUpdate(async () => { await tick(); scrollEl.scrollTop = ... }). In Svelte 4,
//    tick() inside afterUpdate creates an infinite microtask chain that permanently
//    occupies V8, starving CDP and all async work. Fixed in ChatView.svelte by
//    removing async/tick() — afterUpdate already fires post-DOM-update so tick() is
//    redundant.
//
// 2. EventSource real connection: After apiReady=true, App.svelte opens a real
//    browser EventSource('/api/events'). This keeps Chromium's network layer
//    non-idle and can block CDP. Fixed by replacing window.EventSource with a
//    no-op mock via addInitScript (runs before any bundle code).
//
// 3. Chunked API responses: The Rust server uses transfer-encoding:chunked on all
//    endpoints. Playwright's requestfinished only fires when the body stream closes;
//    any fetch whose body is not consumed keeps a connection "pending" and can block
//    CDP. Fixed by route-stubbing all API endpoints with plain Content-Length
//    responses that close immediately.
test.beforeEach(async ({ page }) => {
  // Why: page.addInitScript runs before any page scripts. Replacing window.EventSource
  // prevents the Svelte app from opening a real SSE connection, which would block
  // Playwright's CDP channel (page.content(), page.evaluate(), locators, etc.)
  // after the health check completes and apiReady becomes true.
  // The mock stays in CONNECTING state so the app does not trigger error handlers.
  // Note: addInitScript content runs as plain JavaScript — no TypeScript syntax.
  await page.addInitScript(() => {
    class MockEventSource extends EventTarget {
      static CONNECTING = 0;
      static OPEN = 1;
      static CLOSED = 2;
      constructor(url) {
        super();
        this.url = url;
        this.readyState = 0;
        this.withCredentials = false;
        this.onopen = null;
        this.onmessage = null;
        this.onerror = null;
        this.CONNECTING = 0;
        this.OPEN = 1;
        this.CLOSED = 2;
      }
      close() { this.readyState = 2; }
    }
    window.EventSource = MockEventSource;
  });

  // Why: All tagent API responses use chunked transfer encoding. Any in-flight
  // fetch keeps a persistent connection that blocks Playwright's CDP channel
  // (page.content(), locators, page.evaluate() all hang indefinitely).
  // A single catch-all route (pattern "**") intercepts every request and either
  // stubs API endpoints with clean non-chunked JSON responses or passes non-API
  // requests through to the real server.
  // Note: specific per-path patterns like "**/api/tasks**" fail to intercept
  // requests when multiple route handlers are registered — the catch-all "**"
  // pattern is the only reliable way to intercept all URLs.
  await page.route('**', (route) => {
    const url = route.request().url();
    if (url.includes('/api/events/ticket')) {
      // #5052: the SSE stream is authenticated by a short-lived ticket the app
      // mints here before opening EventSource. Must be checked BEFORE the
      // `/api/events` branch below, which would otherwise swallow it.
      route.fulfill({
        status: 200,
        headers: { 'Content-Type': 'application/json' },
        body: '{"ticket":"test-ticket","expires_in_secs":300}',
      });
    } else if (url.includes('/api/events')) {
      // SSE endpoint — return a closed stream. EventSource is mocked anyway via
      // addInitScript; this catches any code path that bypasses the mock.
      route.fulfill({
        status: 200,
        headers: { 'Content-Type': 'text/event-stream', 'Cache-Control': 'no-cache' },
        body: '',
      });
    } else if (url.includes('/api/health')) {
      route.fulfill({
        status: 200,
        headers: { 'Content-Type': 'application/json' },
        body: '{"status":"ok"}',
      });
    } else if (url.includes('/api/config')) {
      route.fulfill({
        status: 200,
        headers: { 'Content-Type': 'application/json' },
        body: '{"auth_required":false}',
      });
    } else if (url.includes('/api/tasks')) {
      route.fulfill({
        status: 200,
        headers: { 'Content-Type': 'application/json' },
        body: '[]',
      });
    } else {
      // Non-API requests (HTML, JS bundle, CSS) — let them hit the real server.
      route.continue();
    }
  });
});

// Why: Native Node.js timer avoids any Playwright-internal network-idle waiting.
// page.waitForTimeout() can stall on pending network activity; setTimeout does not.
const wait = (ms) => new Promise((resolve) => setTimeout(resolve, ms));

// Why: Use domcontentloaded so goto() returns as soon as the HTML is parsed,
// without waiting for external resources that may stall in headless mode.
const GOTO_OPTS = { waitUntil: 'domcontentloaded' };

test.describe('Trusty Agents web UI smoke tests', () => {

  test('page loads and shows non-blank content', async ({ page }) => {
    // Why: Svelte renders the spinner into #app synchronously at parse time.
    // What: Asserts #app has meaningful HTML (> 50 chars) on first paint,
    // catching a blank-screen regression where the Svelte mount fails silently.
    // Test: Remove App.svelte mount target — fails; fix it — passes.
    await page.goto('/', GOTO_OPTS);
    const fullContent = await page.content();
    const appStart = fullContent.indexOf('<div id="app">');
    const appEnd = fullContent.indexOf('</div>', appStart);
    const appHtml = appStart >= 0 ? fullContent.substring(appStart, appEnd) : '';
    expect(appHtml.length).toBeGreaterThan(50);
  });

  test('no JavaScript errors on load', async ({ page }) => {
    // Why: Unhandled JS errors cause a blank screen but don't appear in server logs.
    // What: Collects console errors and page exceptions during 3s of JS execution.
    // Test: Throw in App.svelte onMount — fails; remove throw — passes.
    const errors = [];
    page.on('console', msg => {
      if (msg.type() === 'error') errors.push(msg.text());
    });
    page.on('pageerror', err => errors.push(err.message));
    await page.goto('/', GOTO_OPTS);
    await wait(3000);
    expect(errors).toEqual([]);
  });

  test('API health endpoint returns ok', async ({ page }) => {
    // Why: /api/health is the liveness probe the app uses to detect server readiness.
    // What: Hits /api/health directly and asserts status:ok in JSON body.
    // Test: Stop the API server — fails; restart — passes.
    const resp = await page.request.get('/api/health');
    expect(resp.ok()).toBe(true);
    const body = await resp.json();
    expect(body.status).toBe('ok');
  });

  test('app reaches ready state within 8s (not stuck on connecting)', async ({ page }) => {
    // Why: The blank-screen bug manifests as the app staying on the
    // "Connecting to API server" spinner when the health probe fails silently.
    //
    // #7456: this test used to infer readiness INDIRECTLY, from the set of
    // startup endpoints the app happened to request — `/api/health`,
    // `/api/config` and `/api/tasks` — and then from an `<nav>` substring in
    // the rendered HTML. Both proxies decayed out from under it while the app
    // itself stayed healthy:
    //   * `/api/tasks` is only ever requested by `TaskHistory.svelte`, which
    //     #3819 unrouted from the sidebar (see `Sidebar.svelte:13-15`). No
    //     mounted component calls `list_tasks` at boot any more, so the third
    //     flag could never flip and the race always lost — `tasks=false`.
    //   * `<nav>` is rendered nowhere in `src/`; `Header.svelte` uses
    //     `<header>` with a `role="tablist"` group.
    // What: asserts the state the app actually publishes —
    // `Header.svelte`'s `data-api-status` attribute, the machine-readable twin
    // of the "API Ready" pill — inside the same 8s budget. No endpoint the app
    // may stop calling stands between the assertion and `apiReady`.
    // Test: this test itself; kill `bootstrap()`'s `apiReady = true` in
    // `App.svelte` — fails with the attribute still reading `connecting`.
    await page.goto('/', GOTO_OPTS);

    const status = page.locator('[data-api-status]');
    try {
      await expect(status).toHaveAttribute('data-api-status', 'ready', { timeout: 8000 });
    } catch (e) {
      const html = await page.content().catch(() => '');
      const { writeFile, mkdir } = await import('fs/promises');
      await mkdir('test-results', { recursive: true }).catch(() => {});
      await writeFile('test-results/stuck-connecting.html', html).catch(() => {});
      const seen = await status.getAttribute('data-api-status').catch(() => null);
      throw new Error(
        `App did not reach ready state within 8s — data-api-status=${seen ?? '<absent>'}`
      );
    }

    // Ready means the shell is mounted, not just that a flag flipped: the
    // sidebar rail renders only in `App.svelte`'s post-spinner branch. Anchored
    // on `data-app-sidebar` because RecapPanel, SlackMirror and
    // KnowledgeGraphBrowser each render an `<aside>` too, and a bare `aside`
    // locator would become a strict-mode violation the moment a second one
    // mounts.
    await expect(page.locator('[data-app-sidebar]')).toBeVisible();
  });

  test('all JS bundle assets load (no 404s)', async ({ page }) => {
    // Why: Wrong asset hash in index.html causes /assets/*.js to 404 and blank screen.
    // What: Collects all failed /assets/ responses during page load.
    // Test: Delete a built asset from dist/ — fails; rebuild — passes.
    const failed = [];
    page.on('response', resp => {
      if (!resp.ok() && resp.url().includes('/assets/')) {
        failed.push(resp.status() + ' ' + resp.url());
      }
    });
    await page.goto('/', GOTO_OPTS);
    await wait(2000);
    expect(failed).toEqual([]);
  });

});
