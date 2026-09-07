/**
 * The stale-registration cleanup decisions (#6941).
 *
 * Why: the panel destroys index registrations, and two of its decisions have a
 * recorded incident behind them. `size_bytes` / `disk_bytes` read `0` for a
 * healthy 71,433-chunk index (#4706), so a gate that consults one calls a live
 * index empty; and `DELETE /indexes/{id}` answers `removed: false` when it
 * removed nothing (#6363), so a caller that trusts the status code records
 * removals that did not happen. Both live in `cleanup.js` as pure functions
 * precisely so they can be asserted here rather than in a browser.
 * Test: `pnpm test` from `crates/trusty-console/ui-search`.
 */

import { describe, it, expect } from 'vitest';
import {
  CENSUS_PATH,
  PRUNE_DELETE_DATA_DEFAULT,
  censusSummary,
  chunkCountOf,
  destructiveSignals,
  impactPhrase,
  pruneConfirmMessage,
  pruneEligibility,
  readDeleteOutcome,
  selectableOrphans,
  summarizeBatch,
  unjudgedConfirmMessage,
  unjudgedReviewNote,
  unjudgedRows
} from './cleanup.js';

/** A census in the shape `GET /registry/orphans` serves. */
const census = {
  orphans: [
    { id: 'wiped', root_path: '/gone/wiped', colocated: false, repo_identity: 'acme/wiped' },
    { id: 'scratch', root_path: '/tmp/scratch', colocated: true, repo_identity: null }
  ],
  indeterminate: [
    {
      id: 'retired',
      root_path: '/retired/.base/.worktrees/x',
      reason: 'the root is missing and so is its parent directory',
      colocated: true,
      repo_identity: 'acme/retired'
    }
  ],
  live_count: 9,
  total: 12
};

describe('census reading', () => {
  it('offers the gone roots and never the unjudgeable ones', () => {
    expect(selectableOrphans(census).map((r) => r.id)).toEqual(['wiped', 'scratch']);
    expect(unjudgedRows(census).map((r) => r.id)).toEqual(['retired']);
  });

  it('tags each row with the classification the daemon gave it', () => {
    expect(selectableOrphans(census).every((r) => r.rootState === 'orphaned')).toBe(true);
    expect(unjudgedRows(census).every((r) => r.rootState === 'indeterminate')).toBe(true);
  });

  it('reads a malformed or absent census as empty rather than guessing', () => {
    for (const bad of [null, undefined, {}, { orphans: null }, { orphans: 'nope' }]) {
      expect(selectableOrphans(bad)).toEqual([]);
      expect(unjudgedRows(bad)).toEqual([]);
    }
    expect(selectableOrphans({ orphans: [{ root_path: '/gone' }, { id: '' }] })).toEqual([]);
  });

  it('summarises the counts without summing anything itself', () => {
    expect(censusSummary(census)).toBe(
      '2 stale of 12 registered; 1 could not be checked and are not in the batch.'
    );
    expect(censusSummary({ orphans: [], indeterminate: [], total: 12 })).toBe(
      'No stale registrations. 12 registered.'
    );
  });

  it('reads the daemon census, not a console proxy route', () => {
    expect(CENSUS_PATH).toBe('/registry/orphans');
  });
});

// ── #4706: the gate reads chunk_count and root state, never a byte metric ────

describe('destructive gating', () => {
  it('destructive_signals_carry_no_byte_metric', () => {
    const signals = destructiveSignals(
      { id: 'wiped', rootState: 'orphaned', size_bytes: 0, disk_bytes: 0 },
      { chunk_count: 71433, disk_bytes: 0, size_bytes: 0 }
    );
    expect(Object.keys(signals).sort()).toEqual(['chunkCount', 'rootState']);
    expect(signals).toEqual({ rootState: 'orphaned', chunkCount: 71433 });
  });

  it('only_a_gone_root_is_eligible', () => {
    const [wiped] = selectableOrphans(census);
    expect(pruneEligibility(wiped, { chunk_count: 12 })).toMatchObject({
      eligible: true,
      chunkCount: 12
    });
  });

  it('an_unjudged_row_is_never_eligible_however_large', () => {
    const [retired] = unjudgedRows(census);
    const verdict = pruneEligibility(retired, { chunk_count: 900000, disk_bytes: 5_000_000_000 });
    expect(verdict.eligible).toBe(false);
    expect(verdict.reason).toMatch(/could not check/i);
  });

  it('refuses a row the census did not classify at all', () => {
    expect(pruneEligibility({ id: 'nowhere' }, null).eligible).toBe(false);
  });

  it('eligibility_is_unchanged_by_any_size_metric', () => {
    // #4706: a healthy 71,433-chunk colocated index reported size_bytes 0, and
    // an operator read that as "empty, safe to delete". Flipping both byte
    // fields across their whole range must not move a single verdict.
    const [wiped] = selectableOrphans(census);
    const [retired] = unjudgedRows(census);
    for (const bytes of [0, null, undefined, 527_000_000, Number.MAX_SAFE_INTEGER]) {
      const status = { chunk_count: 71433, disk_bytes: bytes, size_bytes: bytes };
      expect(pruneEligibility({ ...wiped, size_bytes: bytes, disk_bytes: bytes }, status)).toEqual({
        eligible: true,
        reason: 'trusty-search reports this root is gone from disk.',
        chunkCount: 71433
      });
      expect(
        pruneEligibility({ ...retired, size_bytes: bytes, disk_bytes: bytes }, status).eligible
      ).toBe(false);
    }
  });

  it('reports an unread chunk count as unknown, never as zero', () => {
    // An allowlist-excluded registration is absent from the live registry, so
    // its `/status` is a 404 and this panel has no count for it.
    expect(chunkCountOf(null)).toBeNull();
    expect(chunkCountOf({ error: 'unknown index' })).toBeNull();
    expect(chunkCountOf({ chunk_count: 0 })).toBe(0);
    expect(impactPhrase(null)).toMatch(/unknown/);
    expect(impactPhrase(0)).toBe('no indexed chunks');
    expect(impactPhrase(1)).toBe('1 indexed chunk');
    expect(impactPhrase(71433)).toBe('71,433 indexed chunks');
  });
});

describe('confirm step', () => {
  it('confirm_message_names_chunks_never_bytes', () => {
    const msg = pruneConfirmMessage(['a', 'b'], true, [12, 71433]);
    expect(msg).toContain('Remove 2 stale registrations?');
    expect(msg).toContain('71,445 indexed chunks');
    expect(msg).toContain('deleted too');
    expect(msg).toContain('cannot be undone');
    expect(msg).not.toMatch(/byte|MB|GB/i);
  });

  it('confirm_message_flags_an_unknown_chunk_count', () => {
    const msg = pruneConfirmMessage(['a', 'b'], false, [12, null]);
    expect(msg).toContain('1 whose chunk count the daemon did not report');
    expect(msg).toContain('left in place');
  });

  it('names the singular registration and needs no counts', () => {
    expect(pruneConfirmMessage(['a'], false)).toBe(
      'Remove 1 stale registration? Their on-disk index data will be left in place. This cannot be undone.'
    );
  });

  it('purges by default, and keeping the corpus is the opt-out', () => {
    expect(PRUNE_DELETE_DATA_DEFAULT).toBe(true);
  });

  it('tells a colocated unjudged row apart from a non-colocated one', () => {
    const colocated = unjudgedConfirmMessage({
      id: 'retired',
      root_path: '/retired/x',
      colocated: true
    });
    expect(colocated).toContain('/retired/x');
    expect(colocated).toContain('beside that root');
    expect(unjudgedConfirmMessage({ id: 'a', root_path: '/a', colocated: false })).toContain(
      "trusty-search's own directory"
    );
    // The note and the confirmation are one fact rendered twice (#6423 round 2).
    expect(unjudgedReviewNote({ colocated: true })).toContain('beside that root');
  });
});

// ── #6363 / #4846: the body decides, not the status code ────────────────────

describe('readDeleteOutcome', () => {
  it('accepts a delete the daemon confirmed', () => {
    const out = readDeleteOutcome(200, {
      id: 'wiped',
      ok: true,
      removed: true,
      data_deleted: true,
      quiesced: true
    });
    expect(out).toMatchObject({ ok: true, removed: true, dataDeleted: true });
    expect(out.message).toContain('Removed "wiped"');
    expect(out.message).toContain('data was deleted');
  });

  it('a_removed_false_answer_is_never_success', () => {
    // A 200 whose body says nothing was removed is not a removal, whatever the
    // status line says.
    const out = readDeleteOutcome(200, { id: 'ghost', ok: true, removed: false });
    expect(out.ok).toBe(false);
    expect(out.removed).toBe(false);
    expect(out.message).toContain('was NOT removed');
    expect(out.message).toContain('removed: false');
  });

  it('an_unloaded_registration_reports_the_daemon_answer_verbatim', () => {
    // #6363: an id in no store and no `indexes.toml` row answers 404 with the
    // daemon's own message. The panel shows that message rather than inventing
    // one, and never records the registration as gone.
    const out = readDeleteOutcome(404, {
      id: 'unloaded',
      error: 'unknown index: unloaded',
      ok: false,
      removed: false,
      data_deleted: false,
      quiesced: true
    });
    expect(out.ok).toBe(false);
    expect(out.removed).toBe(false);
    expect(out.message).toContain('unknown index: unloaded');
  });

  it('a_failed_durable_cleanup_is_not_a_removal', () => {
    // #6363: the in-memory deregistration happened, the `indexes.toml` rewrite
    // did not, so the row comes back on the next warm boot.
    const out = readDeleteOutcome(500, {
      id: 'half',
      ok: false,
      removed: true,
      data_deleted: false,
      error: 'could not rewrite indexes.toml: permission denied'
    });
    expect(out.ok).toBe(false);
    expect(out.removed).toBe(true);
    expect(out.message).toContain('did not finish');
    expect(out.message).toContain('permission denied');
  });

  it('a_body_that_did_not_parse_names_the_status', () => {
    const out = readDeleteOutcome(502, null);
    expect(out).toMatchObject({ ok: false, removed: false });
    expect(out.message).toContain('HTTP 502');
  });

  it('a_bare_200_with_no_body_is_never_success', () => {
    // The 2xx path is the one a future edit is most likely to shortcut, and a
    // bare `200` carries no confirmation at all — no `ok`, no `removed`. The
    // daemon never answers a delete that way, so reading one as success would
    // record a removal nothing performed, which is the #6363 leak. Both shapes
    // must refuse, and neither may claim the registration is gone.
    for (const body of [null, {}]) {
      const out = readDeleteOutcome(200, body);
      expect(out).toMatchObject({ ok: false, removed: false, dataDeleted: false });
      expect(out.message).not.toMatch(/^Removed/);
      expect(out.message).not.toMatch(/deregistered/i);
    }
  });
});

describe('summarizeBatch', () => {
  it('reports a partial batch as partial, never as cleaned', () => {
    const out = summarizeBatch([
      { id: 'a', ok: true, message: 'Removed "a".' },
      { id: 'b', ok: false, message: '"b" was NOT removed' }
    ]);
    expect(out).toMatchObject({ ok: false, removed: 1, failed: 1 });
    expect(out.message).toContain('Removed 1; 1 could not be removed');
  });

  it('reports a clean batch as clean', () => {
    expect(summarizeBatch([{ id: 'a', ok: true }])).toMatchObject({
      ok: true,
      removed: 1,
      failed: 0,
      message: 'Removed 1 stale registration.'
    });
  });

  it('an empty batch is not a success', () => {
    expect(summarizeBatch([]).ok).toBe(false);
    expect(summarizeBatch(null).ok).toBe(false);
  });
});
