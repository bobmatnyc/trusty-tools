/**
 * The hash-route dispatch (#6941).
 *
 * Why: the roster arm matches `#/indexes/*` on its first segment alone, so the
 * cleanup route's precedence — and only its precedence — is what stops
 * `#/indexes/cleanup` rendering the index table.
 * Test: `pnpm test` from `crates/trusty-console/ui-search`.
 */

import { describe, it, expect } from 'vitest';
import { resolveView } from './routes.js';

describe('resolveView', () => {
  it('reaches the cleanup panel, not the roster', () => {
    expect(resolveView(['indexes', 'cleanup'])).toEqual({ kind: 'cleanup' });
    expect(resolveView(['index', 'cleanup'])).toEqual({ kind: 'cleanup' });
  });

  it('leaves an index literally named cleanup its own config screen', () => {
    expect(resolveView(['indexes', 'cleanup', 'config'])).toEqual({
      kind: 'index-config',
      id: 'cleanup'
    });
  });

  it('keeps every route the shell had before', () => {
    expect(resolveView([])).toEqual({ kind: 'dashboard' });
    expect(resolveView(['search'])).toEqual({ kind: 'search' });
    expect(resolveView(['indexes'])).toEqual({ kind: 'indexes' });
    expect(resolveView(['indexes', 'apex'])).toEqual({ kind: 'indexes' });
    expect(resolveView(['indexes', 'a%2Fb', 'config'])).toEqual({
      kind: 'index-config',
      id: 'a/b'
    });
    expect(resolveView(['config'])).toEqual({ kind: 'config' });
    expect(resolveView(['health'])).toEqual({ kind: 'health' });
    expect(resolveView(['logs'])).toEqual({ kind: 'logs' });
    expect(resolveView(['nonsense'])).toEqual({ kind: 'dashboard' });
  });
});
