/**
 * Tests for the palace actions the dashboard took over from the console tab
 * (#6928). Ported with the behaviour they pin, from `ui/src/deleteFlow.test.js`
 * and `ui/src/cleanupFlow.test.js`.
 */

import { describe, expect, it } from 'vitest';

import {
  compactConfirmMessage,
  compactPath,
  deleteConfirmMessage,
  deletePath,
  readCompactResult,
  readDeleteResult,
} from './palaceActions.js';

/** The base the console injects as `window.__MEMORY_BASE__` (`tools_ui.rs`). */
const BASE = 'http://127.0.0.1:7788/api/memory/';

describe('routes', () => {
  it('resolve to the console\'s own palace routes, not the memory bridge', () => {
    expect(new URL(compactPath('scratch'), BASE).pathname).toBe(
      '/api/console/memory/palaces/scratch/compact',
    );
    expect(new URL(deletePath('scratch', false), BASE).pathname).toBe(
      '/api/console/memory/palaces/scratch',
    );
  });

  it('survive a proxy sub-path, which an origin-absolute path would not', () => {
    const proxied = 'http://host/console/api/memory/';
    expect(new URL(compactPath('scratch'), proxied).pathname).toBe(
      '/console/api/console/memory/palaces/scratch/compact',
    );
  });

  it('carry the force flag explicitly, in both states', () => {
    expect(deletePath('scratch', false)).toMatch(/\?force=false$/);
    expect(deletePath('scratch', true)).toMatch(/\?force=true$/);
    // Anything that is not exactly `true` is not a force.
    expect(deletePath('scratch', undefined)).toMatch(/\?force=false$/);
  });

  it('percent-encode an id so a refusal reads as a refusal', () => {
    expect(compactPath('a/b')).toContain('a%2Fb');
    expect(deletePath('a b', false)).toContain('a%20b');
  });
});

describe('confirm steps', () => {
  it('name the exact palace they are about to act on', () => {
    expect(deleteConfirmMessage('scratch')).toContain('"scratch"');
    expect(deleteConfirmMessage('scratch')).toMatch(/cannot be undone/);
    expect(compactConfirmMessage('scratch')).toContain('"scratch"');
  });
});

describe('reading an answer', () => {
  it('believes the ok field, not the status code', () => {
    // trusty-search answers 200 for an index it never had; the console route
    // reduces that to `ok`, and this must read that field.
    expect(readDeleteResult(200, { ok: false, error: 'no such palace' })).toEqual({
      ok: false,
      message: 'no such palace',
    });
    expect(readDeleteResult(200, { ok: true, id: 'scratch' })).toEqual({
      ok: true,
      message: 'Deleted "scratch".',
    });
  });

  it('reports the daemon\'s own words on a failure', () => {
    expect(readCompactResult(502, { error: '  palace is held open  ' }).message).toBe(
      'palace is held open',
    );
  });

  it('says so plainly when the daemon gave no reason', () => {
    const out = readCompactResult(500, null);
    expect(out.ok).toBe(false);
    expect(out.message).toMatch(/HTTP 500/);
  });

  it('attaches the reclaimed counts to a compaction', () => {
    expect(
      readCompactResult(200, {
        ok: true,
        id: 'scratch',
        detail: { orphans_removed: 4, total_checked: 120 },
      }).message,
    ).toBe('Compacted "scratch": reclaimed 4 of 120 vector entries.');
  });

  it('still confirms a compaction that reported no counts', () => {
    expect(readCompactResult(200, { ok: true, id: 'scratch' }).message).toBe(
      'Compacted "scratch".',
    );
  });
});
