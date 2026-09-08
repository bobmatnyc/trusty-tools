// Which call is allowed to say "the daemon is unreachable" (issue #6155).
//
// Why: the topbar's version badge flips to `offline` off `getHealth()`, and
// `getError()` reads as "is the daemon reachable". `refreshStatus` wrote into
// the same error slot, so a slow or failing `/api/v1/status` presented as a
// connection failure against a daemon answering `/health` in 25 ms. Only
// `/health` may set either now — and only after
// `UNREACHABLE_AFTER_FAILURES` consecutive failures, cold start included.
// What: stubs `api.health` and `api.status` independently and asserts each
// refresh touches only its own signal.
// Test: this file — `pnpm test`.

import { beforeEach, describe, expect, it, vi } from 'vitest';

// Why a fresh module per test: `_health` and the failure count are module-level
// state, and `_health` never returns to `null` once a poll has written it. A
// cold start — the case that regressed — is therefore only reachable in a
// module nothing has touched yet. `api.js` is re-imported in the same
// generation so the spy lands on the object `state.svelte.js` actually calls.
async function freshState() {
  vi.resetModules();
  const { api } = await import('./api.js');
  const state = await import('./state.svelte.js');
  return { api, state };
}

beforeEach(() => {
  vi.restoreAllMocks();
});

describe('refreshHealth / refreshStatus — separate signals (issue #6155)', () => {
  it('a cold start below the threshold stays connecting, not offline', async () => {
    const { api, state } = await freshState();
    vi.spyOn(api, 'health').mockRejectedValue(new Error('502 Bad Gateway: '));

    await state.refreshHealth();

    // Topbar.svelte renders a null snapshot as the muted `connecting…` badge
    // and any non-null one that is not `ok` as the red `offline` badge. One
    // failed poll after mount is not evidence of an outage, so it must leave
    // the snapshot null — the error is still surfaced immediately.
    expect(state.getHealth()).toBeNull();
    expect(state.getError()).toContain('502');
  });

  it('a cold start reaching the threshold reports unreachable', async () => {
    const { api, state } = await freshState();
    vi.spyOn(api, 'health').mockRejectedValue(new Error('502 Bad Gateway: '));

    for (let i = 0; i < state.UNREACHABLE_AFTER_FAILURES; i += 1) {
      await state.refreshHealth();
    }

    expect(state.getHealth().status).toBe('unreachable');
    expect(state.getError()).toContain('502');
  });

  it('leaves health alone when only /api/v1/status fails', async () => {
    const { api, state } = await freshState();
    vi.spyOn(api, 'health').mockResolvedValue({ status: 'ok', version: '0.26.0' });
    await state.refreshHealth();

    vi.spyOn(api, 'status').mockRejectedValue(new Error('502 Bad Gateway: '));
    await state.refreshStatus();

    // The badge stays green: `/health` answered, and nothing else may say
    // otherwise.
    expect(state.getHealth()).toEqual({ status: 'ok', version: '0.26.0' });
    expect(state.getError()).toBeNull();
    expect(state.getStatusError()).toContain('502');
  });

  it('one failed poll does not flip the badge offline', async () => {
    // #6155: at 200 events/s on the activity stream the main thread completed
    // 1 of ~69 one-per-second polls; the rest hit `api.js`'s 35 s abort. A
    // daemon answering curl in 1.5 ms was reported offline by a client that
    // never got to read the answer.
    const { api, state } = await freshState();
    vi.spyOn(api, 'health').mockResolvedValue({ status: 'ok', version: '0.26.0' });
    await state.refreshHealth();

    vi.spyOn(api, 'health').mockRejectedValue(new Error('timed out after 35s: /health'));
    await state.refreshHealth();

    // The error is visible at once; the snapshot is not discarded yet.
    expect(state.getError()).toContain('timed out');
    expect(state.getHealth()).toEqual({ status: 'ok', version: '0.26.0' });

    await state.refreshHealth();

    // A second poll agrees, so now it is an outage.
    expect(state.getHealth().status).toBe('unreachable');
  });

  it('a recovered poll resets the failure count', async () => {
    const { api, state } = await freshState();
    vi.spyOn(api, 'health').mockResolvedValue({ status: 'ok', version: '0.26.0' });
    await state.refreshHealth();

    vi.spyOn(api, 'health').mockRejectedValue(new Error('boom'));
    await state.refreshHealth();

    vi.spyOn(api, 'health').mockResolvedValue({ status: 'ok', version: '0.26.0' });
    await state.refreshHealth();

    // One more failure after a success must not flip it — the count restarted.
    vi.spyOn(api, 'health').mockRejectedValue(new Error('boom'));
    await state.refreshHealth();

    expect(state.getHealth()).toEqual({ status: 'ok', version: '0.26.0' });
  });

  it('clears the status error once status answers again', async () => {
    const { api, state } = await freshState();
    vi.spyOn(api, 'status').mockRejectedValue(new Error('boom'));
    await state.refreshStatus();
    expect(state.getStatusError()).toBe('boom');

    vi.spyOn(api, 'status').mockResolvedValue({ palace_count: 93 });
    await state.refreshStatus();

    expect(state.getStatusError()).toBeNull();
    expect(state.getStatus().palace_count).toBe(93);
  });
});
