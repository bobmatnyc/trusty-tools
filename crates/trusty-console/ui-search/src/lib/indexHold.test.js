/*
 * Why: the held-index banner (#9059) is only as right as its reading of the
 * daemon's two hold fields.
 * What: checks `holdFromConfig` and `holdFromStatus` against held and not-held
 * bodies shaped like trusty-search's `IndexConfigView` and status report.
 * Test: this file.
 */
import { describe, expect, it } from 'vitest';

import { holdFromConfig, holdFromStatus } from './indexHold.js';

// The reason `ExcludeHold::reason` writes into `last_walk_error`.
const DAEMON_REASON =
  'index \'proj\' is held: exclude glob(s) ["**.min.js"] do not parse, so the paths they ' +
  'name cannot be excluded. Nothing is indexed until PATCH /indexes/proj/config sets ' +
  'valid exclude_globs; that PATCH then starts a catch-up reindex for the changes ' +
  'refused while held. Search keeps serving what is already indexed (#9059)';

describe('holdFromConfig', () => {
  it('reports the invalid globs of a held index', () => {
    const hold = holdFromConfig('proj', {
      exclude_globs: ['**/.env', '**.min.js'],
      invalid_exclude_globs: ['**.min.js']
    });
    expect(hold).toMatchObject({ indexId: 'proj', patterns: ['**.min.js'] });
    expect(hold.reason).toContain('does not parse');
  });

  it('reports no hold for an empty list, a missing field, or no body', () => {
    expect(holdFromConfig('proj', { exclude_globs: ['**/gen/**'], invalid_exclude_globs: [] })).toBeNull();
    expect(holdFromConfig('proj', { exclude_globs: [] })).toBeNull();
    expect(holdFromConfig('proj', null)).toBeNull();
  });
});

describe('holdFromStatus', () => {
  it('carries the daemon reason for a held index', () => {
    expect(holdFromStatus('proj', { status: 'held', last_walk_error: DAEMON_REASON })).toEqual({
      indexId: 'proj',
      patterns: [],
      reason: DAEMON_REASON
    });
  });

  it('falls back to a generic reason when the daemon sent none', () => {
    const hold = holdFromStatus('proj', { status: 'held', last_walk_error: null });
    expect(hold.reason).toContain('exclude glob does not parse');
  });

  it('reports no hold for any other status', () => {
    for (const status of ['ready', 'indexing', 'degraded']) {
      expect(holdFromStatus('proj', { status, last_walk_error: 'walk failed' })).toBeNull();
    }
    expect(holdFromStatus('proj', null)).toBeNull();
  });
});
