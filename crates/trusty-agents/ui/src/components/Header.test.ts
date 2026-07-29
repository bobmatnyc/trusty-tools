// Why: the header's version line is a stale-build DIAGNOSTIC, so its wiring
// carries two properties only a mounted DOM can prove — that the daemon
// version actually reaches the rendered header, and that it is read ONCE,
// after readiness, rather than becoming a second poller against the sidecar.
// `buildInfo.test.ts` pins the pure state/format contract; this pins the
// component's use of it.
// What: Mounts the real `Header` with `fetch` stubbed (`ModelSwitcher` also
// fetches its catalog on mount), mirroring `ChatPane.test.ts`'s stub-fetch
// mounting pattern.
// Test: this file.
import { afterEach, describe, expect, it, vi } from 'vitest';
import { mount, unmount } from 'svelte';

// jsdom has no `matchMedia`, and `stores/theme.ts` calls it at MODULE scope
// (via `ThemeToggle`, which this header renders). `vi.hoisted` runs before the
// hoisted `import` below, which is the only point early enough to install it.
vi.hoisted(() => {
  Object.defineProperty(window, 'matchMedia', {
    writable: true,
    value: (query: string) => ({
      matches: false,
      media: query,
      addEventListener: () => {},
      removeEventListener: () => {},
      addListener: () => {},
      removeListener: () => {},
      onchange: null,
      dispatchEvent: () => false,
    }),
  });
});

import Header from './Header.svelte';

let target: HTMLDivElement | null = null;
let instance: Record<string, unknown> | null = null;

async function waitFor(predicate: () => boolean, timeoutMs = 2000): Promise<void> {
  const start = Date.now();
  while (Date.now() - start < timeoutMs) {
    if (predicate()) return;
    await new Promise((resolve) => setTimeout(resolve, 10));
  }
  throw new Error('timed out waiting for condition');
}

/**
 * Answers `/api/health` with `health`, and every other route the header's
 * children touch on mount with an empty object. Returns the spy so a test can
 * count the health reads.
 */
function stubApi(health: { ok: boolean; body?: unknown }) {
  const spy = vi.fn(async (input: RequestInfo | URL) => {
    const url = String(input);
    if (url.includes('/api/health')) {
      return {
        ok: health.ok,
        status: health.ok ? 200 : 500,
        json: async () => health.body,
      } as Response;
    }
    // `ModelSwitcher` loads its catalog on mount; an empty-but-well-formed
    // catalog keeps it out of the way of what these tests assert.
    return {
      ok: true,
      status: 200,
      json: async () => ({
        providers: [],
        local: { available: false, default_model: 'none' },
      }),
    } as Response;
  });
  vi.stubGlobal('fetch', spy);
  return spy;
}

function mountHeader(apiReady: boolean) {
  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(Header, { target, props: { activeView: 'chat', apiReady } }) as Record<
    string,
    unknown
  >;
}

const provenance = () =>
  target?.querySelector('[data-testid="build-provenance"]') as HTMLElement | null;

const healthCalls = (spy: ReturnType<typeof stubApi>) =>
  spy.mock.calls.filter(([u]) => String(u).includes('/api/health')).length;

afterEach(() => {
  if (instance) unmount(instance);
  instance = null;
  target?.remove();
  target = null;
  vi.unstubAllGlobals();
  vi.restoreAllMocks();
});

describe('Header build provenance', () => {
  it('renders the daemon version once the API is ready', async () => {
    stubApi({ ok: true, body: { status: 'ok', version: '0.38.6', pid: 1, ppid: 0 } });
    mountHeader(true);
    await waitFor(() => provenance()?.textContent?.includes('0.38.6') ?? false);
    expect(provenance()?.textContent?.trim()).toBe('v0.38.6');
  });

  it('shows a placeholder — never a guessed number — before the API is ready', () => {
    const spy = stubApi({ ok: true, body: { version: '0.38.6' } });
    mountHeader(false);
    expect(provenance()?.textContent?.trim()).toBe('v…');
    // The probe is gated on readiness, so nothing is asked of a sidecar that
    // may not be listening yet.
    expect(healthCalls(spy)).toBe(0);
  });

  it('reads health exactly once — it is a one-shot read, not a poller', async () => {
    const spy = stubApi({ ok: true, body: { version: '0.38.6' } });
    mountHeader(true);
    await waitFor(() => provenance()?.textContent?.includes('0.38.6') ?? false);
    await new Promise((r) => setTimeout(r, 150));
    expect(healthCalls(spy)).toBe(1);
  });

  it('degrades to a dash when health cannot report a version', async () => {
    stubApi({ ok: false });
    mountHeader(true);
    await waitFor(() => provenance()?.textContent?.includes('—') ?? false);
    expect(provenance()?.textContent?.trim()).toBe('v—');
  });

  it('always renders the slot, so no state collapses the header layout', async () => {
    stubApi({ ok: false });
    mountHeader(true);
    expect(provenance()).not.toBeNull();
    await waitFor(() => provenance()?.textContent?.includes('—') ?? false);
    expect(provenance()).not.toBeNull();
    expect(provenance()?.textContent?.trim().length ?? 0).toBeGreaterThan(0);
  });
});
