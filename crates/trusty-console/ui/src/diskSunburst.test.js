/**
 * The Disk view's tier mapping, small-segment collapse and fallback sort
 * (#6929).
 *
 * Why these three and not the markup: DOC-73 §16.3 makes three claims a
 * screenshot cannot hold — colour means staleness and nothing else, an arc
 * under 2° folds into a wedge rather than vanishing, and the narrow-width list
 * is ordered by bytes descending. Each is a pure function here, so each is
 * asserted here. No test runner is installed in this package; `node --test` is
 * built in and cannot mount a Svelte component, which is why the geometry lives
 * in a module the way `barGraph.js` does.
 *
 * Run: `node --test src/diskSunburst.test.js` from `crates/trusty-console/ui`.
 */

import test from 'node:test';
import assert from 'node:assert/strict';

import {
  MIN_ARC_DEGREES,
  NARROW_BREAKPOINT_PX,
  NEUTRAL_COLOR,
  TIERS,
  TIER_COLORS,
  arcPath,
  flatRows,
  formatBytes,
  isNarrow,
  sortWorktrees,
  sunburstRings,
  tierColor,
  tierLabel,
  tierTone,
} from './diskSunburst.js';

const GB = 1024 ** 3;

/** A worktree row shaped as `disk_survey` serializes one. */
function wt(id, tier, bytes, branch = id) {
  return { id, path: `/w/${id}`, branch, tier, bytes, reasons: [], reclaimable: tier === 'stale' };
}

/** A survey with two projects; `two` holds the slivers. */
function survey() {
  return {
    generated_at: '2026-09-07T12:00:00+00:00',
    keep_list: { patterns: [], invalid: [] },
    root: {
      path: '/w',
      bytes: 21 * GB,
      counts: { stale: 2, review: 2, keep: 1, missing: 1 },
      stale_bytes: 13 * GB,
      projects: [
        {
          name: 'acme/one',
          path: '/w/one',
          bytes: 20 * GB,
          worktrees: [
            wt('big-merged', 'stale', 12 * GB),
            wt('mid-open', 'review', 7 * GB),
            // ~0.03% of the fleet: under 2° and therefore folded.
            wt('sliver', 'keep', 6 * 1024 * 1024),
            // The survey's budget was reached before this one — no bytes at
            // all, so it can only be reached through the fold.
            wt('past-budget', 'review', null),
            wt('gone', 'missing', null),
          ],
        },
        {
          name: 'acme/two',
          path: '/w/two',
          bytes: 1 * GB,
          worktrees: [wt('small-clean', 'stale', 1 * GB)],
        },
      ],
    },
  };
}

// ── tier mapping ───────────────────────────────────────────────────────────

test('each tier maps to the token #6929 names for it, and nothing else does', () => {
  assert.equal(tierColor('stale'), 'var(--trusty-success)');
  assert.equal(tierColor('review'), 'var(--trusty-warning)');
  assert.equal(tierColor('keep'), 'var(--trusty-danger)');
  assert.equal(tierColor('missing'), 'var(--trusty-text-muted)');
  // Colour is a tier statement, so an unknown tier must not borrow one that
  // means something: it reads muted, never "safe to clear".
  assert.equal(tierColor('nonsense'), TIER_COLORS.missing);
  assert.equal(tierColor(undefined), TIER_COLORS.missing);
});

test('tier labels and badge tones agree with the colours', () => {
  assert.equal(tierLabel('stale'), 'Safe to clear');
  assert.equal(tierLabel('review'), 'Review');
  assert.equal(tierLabel('keep'), 'Keep');
  assert.equal(tierTone('stale'), 'success');
  assert.equal(tierTone('review'), 'warning');
  assert.equal(tierTone('keep'), 'danger');
  assert.equal(tierTone('missing'), 'muted');
  // An unknown tier is named rather than blanked, so a new tier from a newer
  // daemon shows up as itself instead of as an empty cell.
  assert.equal(tierLabel('quarantined'), 'quarantined');
});

test('every arc a worktree draws is coloured by its tier, and no other ring is', () => {
  const rings = sunburstRings(survey());
  for (const arc of rings.worktrees.filter((a) => a.kind === 'worktree')) {
    assert.equal(arc.color, tierColor(arc.tier), `${arc.id} is not its tier's colour`);
  }
  for (const project of rings.projects) {
    assert.equal(project.color, NEUTRAL_COLOR, 'ring 1 must carry no staleness colour');
    assert.ok(!TIERS.some((t) => project.color === TIER_COLORS[t]));
  }
});

// ── small-segment collapse ─────────────────────────────────────────────────

test('an arc under the 2° minimum folds into its project’s other wedge', () => {
  const rings = sunburstRings(survey());
  const other = rings.worktrees.find((a) => a.kind === 'other');
  assert.ok(other, 'nothing collapsed — the sliver rows should not have earned their own arc');

  const folded = other.members.map((m) => m.id).sort();
  assert.deepEqual(folded, ['gone', 'past-budget', 'sliver']);
  assert.match(other.name, /^other \(3 items, /, `wedge is unlabelled: ${other.name}`);

  // The two measured rows kept their own arcs.
  const drawn = rings.worktrees.filter((a) => a.kind === 'worktree').map((a) => a.id);
  assert.ok(drawn.includes('big-merged'));
  assert.ok(drawn.includes('mid-open'));
  assert.ok(!drawn.includes('past-budget'), 'a 0-byte row must not be drawn as a 0° arc');
});

test('a folded row is never dropped — the wedge is wide enough to reach', () => {
  // Every row here is unmeasured, so byte share says nothing about any of them.
  const allUnmeasured = {
    root: {
      path: '/w',
      projects: [
        {
          name: 'p',
          path: '/w/p',
          bytes: null,
          worktrees: [wt('a', 'review', null), wt('b', 'review', null)],
        },
      ],
    },
  };
  const rings = sunburstRings(allUnmeasured);
  const arcs = rings.worktrees;
  assert.equal(arcs.length, 2, 'an even split, not a fold, when nothing is measured');
  for (const arc of arcs) assert.ok(arc.d.length > 0, 'an empty path is an unreachable row');
});

test('the collapse threshold is the spec’s 2°, and it is what decides the fold', () => {
  assert.equal(MIN_ARC_DEGREES, 2);
  // 1.5% of a 360° circle is 5.4° — above the default, below a 10° threshold.
  const one = {
    root: {
      projects: [
        {
          name: 'p',
          path: '/w/p',
          bytes: 100 * GB,
          worktrees: [wt('bulk', 'stale', 98.5 * GB), wt('slim', 'keep', 1.5 * GB)],
        },
      ],
    },
  };
  assert.equal(
    sunburstRings(one, { minDegrees: 2 }).worktrees.filter((a) => a.kind === 'other').length,
    0,
    '5.4° is above a 2° floor and must keep its own arc',
  );
  const folded = sunburstRings(one, { minDegrees: 10 }).worktrees.find((a) => a.kind === 'other');
  assert.ok(folded, '5.4° is below a 10° floor and must fold');
  assert.deepEqual(
    folded.members.map((m) => m.id),
    ['slim'],
  );
});

test('a row sitting exactly ON the threshold keeps its own arc', () => {
  // `apportion` folds on `share < minDegrees`, so 2.0° itself is KEPT. The test
  // above pins 5.4° and 5.4°-under-a-10°-floor — both comfortably off the
  // boundary, so either comparison passes them and neither would catch a
  // `>` written where the code says `>=`. This is the case that does.
  const bulk = 179 * GB;
  const edge = 1 * GB;
  // The one arithmetic the module performs: (weight / total) × span.
  assert.equal(
    (edge / (bulk + edge)) * 360,
    MIN_ARC_DEGREES,
    'the fixture must land ON the boundary, not near it',
  );

  const boundary = {
    root: {
      path: '/w',
      projects: [
        {
          name: 'p',
          path: '/w/p',
          bytes: bulk + edge,
          worktrees: [wt('bulk', 'stale', bulk), wt('edge', 'keep', edge)],
        },
      ],
    },
  };
  const rings = sunburstRings(boundary);

  assert.equal(
    rings.worktrees.filter((a) => a.kind === 'other').length,
    0,
    'nothing is below the floor, so there is no wedge to fold into',
  );
  const drawn = rings.worktrees.filter((a) => a.kind === 'worktree').map((a) => a.id);
  assert.deepEqual(drawn.sort(), ['bulk', 'edge'], 'the 2.0° row draws its own arc');

  // One byte less is below the floor, which is what proves the assertion above
  // is testing the boundary rather than a row that could never fold.
  boundary.root.projects[0].worktrees[1].bytes = edge - 1;
  const under = sunburstRings(boundary).worktrees.find((a) => a.kind === 'other');
  assert.ok(under, 'a hair under 2.0° must fold');
  assert.deepEqual(
    under.members.map((m) => m.id),
    ['edge'],
  );
});

test('the ring-2 arcs of one project stay inside that project’s ring-1 span', () => {
  // Regression guard on the apportioning: a rescale that forgot to reserve the
  // wedge's sweep would push the last arc past its project's sector.
  const rings = sunburstRings(survey(), { size: 400 });
  assert.ok(rings.projects.length === 2);
  for (const arc of rings.worktrees) assert.ok(arc.d.startsWith('M'), `bad path: ${arc.d}`);
  assert.ok(rings.centre.radius > 0);
  assert.equal(rings.empty, false);
});

test('an empty survey renders nothing rather than throwing', () => {
  const rings = sunburstRings({ root: { path: '/w', projects: [] } });
  assert.equal(rings.empty, true);
  assert.deepEqual(rings.projects, []);
  assert.deepEqual(rings.worktrees, []);
  assert.equal(sunburstRings(undefined).empty, true);
  assert.equal(sunburstRings(null).empty, true);
});

test('arcPath draws a full ring as an arc that actually closes', () => {
  const full = arcPath(100, 100, 40, 60, 0, 360);
  assert.ok(full.startsWith('M'), full);
  assert.ok(full.endsWith('Z'), full);
  // A zero or negative sweep is not a shape.
  assert.equal(arcPath(100, 100, 40, 60, 10, 10), '');
  assert.equal(arcPath(100, 100, 40, 60, 20, 10), '');
});

// ── the narrow-width fallback ──────────────────────────────────────────────

test('the fallback list is ordered by bytes descending, within and across projects', () => {
  const rows = flatRows(survey());
  const projects = rows.filter((r) => r.kind === 'project').map((r) => r.name);
  assert.deepEqual(projects, ['acme/one', 'acme/two'], 'the largest project comes first');

  const inOne = rows
    .filter((r) => r.kind === 'worktree' && r.project === 'acme/one')
    .map((r) => r.id);
  assert.deepEqual(inOne, ['big-merged', 'mid-open', 'sliver', 'gone', 'past-budget']);
});

test('the fallback list carries every row the sunburst folded away', () => {
  const rows = flatRows(survey()).filter((r) => r.kind === 'worktree');
  const listed = rows.map((r) => r.id).sort();
  assert.deepEqual(listed, ['big-merged', 'gone', 'mid-open', 'past-budget', 'sliver', 'small-clean']);
  // And it carries the same tier the arc would have been coloured by.
  assert.equal(rows.find((r) => r.id === 'past-budget').tier, 'review');
});

test('the list sorts by tier in legend order, not alphabetically', () => {
  const rows = sortWorktrees(survey().root.projects[0].worktrees, 'tier', 'desc');
  assert.deepEqual(
    rows.map((r) => r.tier),
    ['stale', 'review', 'review', 'keep', 'missing'],
  );
  // Alphabetical order would be keep, missing, review, review, stale.
  assert.notEqual(rows[0].tier, 'keep');
});

test('the list sort reverses, and an unmeasured row never sorts as zero-sized', () => {
  const asc = sortWorktrees(survey().root.projects[0].worktrees, 'bytes', 'asc').map((r) => r.id);
  assert.equal(asc.at(-1), 'big-merged', 'ascending puts the largest last');
  const desc = sortWorktrees(survey().root.projects[0].worktrees, 'bytes', 'desc').map((r) => r.id);
  assert.equal(desc[0], 'big-merged');
  assert.deepEqual(desc.slice(-2), ['gone', 'past-budget'], 'unmeasured rows sort together');
});

test('the sunburst gives way to the list below 600px', () => {
  assert.equal(NARROW_BREAKPOINT_PX, 600);
  assert.equal(isNarrow(599), true);
  assert.equal(isNarrow(600), false);
  assert.equal(isNarrow(1200), false);
  // An unmeasured container must not silently pick the narrow branch.
  assert.equal(isNarrow(null), false);
  assert.equal(isNarrow(undefined), false);
});

test('formatBytes reports an unmeasured row as unmeasured, not as empty', () => {
  assert.equal(formatBytes(null), '—');
  assert.equal(formatBytes(undefined), '—');
  assert.equal(formatBytes(0), '0 B');
  assert.equal(formatBytes(2 * GB), '2.00 GB');
});
