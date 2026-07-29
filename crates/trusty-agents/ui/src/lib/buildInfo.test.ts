// Unit tests for the header's daemon-build provenance source.
//
// Why: the header's version line is a DIAGNOSTIC — it exists so a stale
// running build is visible without a `strings` dump of the binary. That makes
// its degenerate states load-bearing: it must never render a guessed value, an
// empty slot, or an unhandled rejection when `/api/health` is unusable. These
// pin the loading / ready / unavailable contract and the #4260 SHA slot
// without mounting `Header.svelte`.

import { describe, expect, it } from 'vitest';
import {
  fetchBuildInfo,
  formatProvenance,
  parseHealthBody,
  provenanceTitle,
  type BuildInfoState,
} from './buildInfo';

/** Minimal `fetch` stub — only `ok` and `json()` are consumed. */
function stubFetch(res: { ok: boolean; json?: () => Promise<unknown> }): typeof fetch {
  return (async () => res) as unknown as typeof fetch;
}

describe('parseHealthBody', () => {
  it('accepts the daemon body shipping today (version, no commit)', () => {
    // Verbatim shape of `HealthBody` in api/server/handlers.rs.
    expect(parseHealthBody({ status: 'ok', version: '0.38.6', pid: 123, ppid: 1 })).toEqual({
      status: 'ready',
      version: '0.38.6',
    });
  });

  it('picks up a commit when one is present (#4260 slot)', () => {
    expect(parseHealthBody({ status: 'ok', version: '0.38.6', commit: 'abc1234' })).toEqual({
      status: 'ready',
      version: '0.38.6',
      commit: 'abc1234',
    });
  });

  it('treats a missing, blank, or non-string version as unavailable', () => {
    for (const body of [{}, { version: '' }, { version: '   ' }, { version: 42 }]) {
      expect(parseHealthBody(body)).toEqual({ status: 'unavailable' });
    }
  });

  it('treats a non-object body as unavailable', () => {
    for (const body of [null, undefined, 'ok', 7]) {
      expect(parseHealthBody(body)).toEqual({ status: 'unavailable' });
    }
  });

  it('ignores a blank commit rather than rendering a dangling separator', () => {
    expect(parseHealthBody({ version: '0.38.6', commit: '  ' })).toEqual({
      status: 'ready',
      version: '0.38.6',
    });
  });
});

describe('formatProvenance', () => {
  it('never renders an empty string, so the slot cannot collapse', () => {
    const states: BuildInfoState[] = [
      { status: 'loading' },
      { status: 'unavailable' },
      { status: 'ready', version: '0.38.6' },
    ];
    for (const s of states) expect(formatProvenance(s).length).toBeGreaterThan(0);
  });

  it('shows no number at all until one is known (no wrong-value flash)', () => {
    expect(formatProvenance({ status: 'loading' })).toBe('v…');
    expect(formatProvenance({ status: 'unavailable' })).toBe('v—');
  });

  it('renders the daemon version once known', () => {
    expect(formatProvenance({ status: 'ready', version: '0.38.6' })).toBe('v0.38.6');
  });

  it('appends a short SHA on the same line when #4260 lands', () => {
    expect(formatProvenance({ status: 'ready', version: '0.38.6', commit: 'abc1234' })).toBe(
      'v0.38.6 · abc1234',
    );
  });
});

describe('provenanceTitle', () => {
  it('attributes the number to the daemon, not the frontend build', () => {
    expect(provenanceTitle({ status: 'ready', version: '0.38.6' })).toContain('/api/health');
    expect(provenanceTitle({ status: 'ready', version: '0.38.6' })).toContain('0.38.6');
  });

  it('distinguishes still-asking from gave-up', () => {
    expect(provenanceTitle({ status: 'loading' })).not.toEqual(
      provenanceTitle({ status: 'unavailable' }),
    );
  });
});

describe('fetchBuildInfo', () => {
  it('returns the parsed version on a 200', async () => {
    const f = stubFetch({ ok: true, json: async () => ({ status: 'ok', version: '0.38.6' }) });
    await expect(fetchBuildInfo(f)).resolves.toEqual({ status: 'ready', version: '0.38.6' });
  });

  it('reports unavailable on a non-2xx without reading the body', async () => {
    const f = stubFetch({
      ok: false,
      json: async () => {
        throw new Error('body must not be read on a non-ok response');
      },
    });
    await expect(fetchBuildInfo(f)).resolves.toEqual({ status: 'unavailable' });
  });

  it('resolves (never rejects) when the request throws', async () => {
    const f = (async () => {
      throw new Error('connection refused');
    }) as unknown as typeof fetch;
    await expect(fetchBuildInfo(f)).resolves.toEqual({ status: 'unavailable' });
  });

  it('resolves (never rejects) when the body is not JSON', async () => {
    const f = stubFetch({
      ok: true,
      json: async () => {
        throw new SyntaxError('Unexpected token < in JSON');
      },
    });
    await expect(fetchBuildInfo(f)).resolves.toEqual({ status: 'unavailable' });
  });
});
