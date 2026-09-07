// Which call is allowed to say "the daemon is unreachable" (issue #6155).
//
// Why: the topbar's version badge flips to `offline` off `getHealth()`, and
// `getError()` reads as "is the daemon reachable". `refreshStatus` wrote into
// the same error slot, so a slow or failing `/api/v1/status` presented as a
// connection failure against a daemon answering `/health` in 25 ms. Only
// `/health` may set either now.
// What: stubs `api.health` and `api.status` independently and asserts each
// refresh touches only its own signal.
// Test: this file — `pnpm test`.

import { beforeEach, describe, expect, it, vi } from 'vitest';

import { api } from './api.js';
import {
  getError,
  getHealth,
  getStatus,
  getStatusError,
  refreshHealth,
  refreshStatus
} from './state.svelte.js';

beforeEach(() => {
  vi.restoreAllMocks();
});

describe('refreshHealth / refreshStatus — separate signals (issue #6155)', () => {
  it('marks the daemon unreachable when /health itself fails', async () => {
    vi.spyOn(api, 'health').mockRejectedValue(new Error('502 Bad Gateway: '));

    await refreshHealth();

    expect(getHealth().status).toBe('unreachable');
    expect(getError()).toContain('502');
  });

  it('leaves health alone when only /api/v1/status fails', async () => {
    vi.spyOn(api, 'health').mockResolvedValue({ status: 'ok', version: '0.26.0' });
    await refreshHealth();

    vi.spyOn(api, 'status').mockRejectedValue(new Error('502 Bad Gateway: '));
    await refreshStatus();

    // The badge stays green: `/health` answered, and nothing else may say
    // otherwise.
    expect(getHealth()).toEqual({ status: 'ok', version: '0.26.0' });
    expect(getError()).toBeNull();
    expect(getStatusError()).toContain('502');
  });

  it('clears the status error once status answers again', async () => {
    vi.spyOn(api, 'status').mockRejectedValue(new Error('boom'));
    await refreshStatus();
    expect(getStatusError()).toBe('boom');

    vi.spyOn(api, 'status').mockResolvedValue({ palace_count: 93 });
    await refreshStatus();

    expect(getStatusError()).toBeNull();
    expect(getStatus().palace_count).toBe(93);
  });
});
