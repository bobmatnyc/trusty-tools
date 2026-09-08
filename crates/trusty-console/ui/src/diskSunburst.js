/**
 * The Disk view's geometry, tier colours and narrow-width fallback (#6929).
 *
 * Why a module rather than markup: DOC-73 §16.3 rules out a charting library —
 * `BarGraph.svelte` is inline-SVG-only so the console's charts cannot drift
 * apart, and `d3` lives in a different bundle. This file is the sunburst's half
 * of that arrangement: every path string, every tier colour and the whole
 * fallback ordering are computed here, so `node --test` can assert them without
 * mounting a component.
 *
 * What it computes, and the two rules that shape it:
 *
 *   - **Colour means staleness, and only staleness.** Ring 2 (worktrees) is the
 *     only ring that carries a tier colour, mapped onto tokens the console
 *     already defines — no new colour role, per §16.3. Ring 1 (projects) is a
 *     neutral surface tone.
 *   - **A row is never dropped, only folded.** An arc under `MIN_ARC_DEGREES`
 *     collapses into its project's grey "other" wedge, which names how many
 *     rows and how many bytes it holds and expands to a flat list. That is what
 *     keeps an unmeasured row visible: a worktree the survey's budget was
 *     reached before carries `bytes: null` and no byte share at all, so without
 *     the fold it would be a zero-degree arc nobody could click. It is a
 *     `review` row (the tool never reports one `stale`), and it is listed.
 *
 * Test: `diskSunburst.test.js` — run `node --test src/diskSunburst.test.js`
 * from `crates/trusty-console/ui`.
 */

/**
 * The smallest arc drawn on its own, in degrees (DOC-73 §16.3).
 *
 * Below this an arc is a sliver too thin to hover and too thin to label, so it
 * joins the "other" wedge instead.
 */
export const MIN_ARC_DEGREES = 2;

/** The tiers `disk_survey` emits, in the order the legend lists them. */
export const TIERS = ['stale', 'review', 'keep', 'missing'];

/**
 * Tier → CSS custom property, per #6929 and DOC-73 §16.3.
 *
 * `missing` is the muted text token: git still registers the worktree but the
 * directory is gone, which is neither safe, nor pending review, nor held.
 */
export const TIER_COLORS = {
  stale: 'var(--trusty-success)',
  review: 'var(--trusty-warning)',
  keep: 'var(--trusty-danger)',
  missing: 'var(--trusty-text-muted)',
};

/** What the legend, the badges and the detail panel call each tier. */
export const TIER_LABELS = {
  stale: 'Safe to clear',
  review: 'Review',
  keep: 'Keep',
  missing: 'Missing',
};

/** Tier → `Badge.svelte` tone, so the fallback list stamps the same meaning. */
export const TIER_TONES = {
  stale: 'success',
  review: 'warning',
  keep: 'danger',
  missing: 'muted',
};

/** Ring 1 and the "other" wedge carry no staleness, so they carry no tier hue. */
export const NEUTRAL_COLOR = 'var(--trusty-surface-raised)';
export const OTHER_COLOR = 'var(--trusty-border-strong)';

/** The colour for `tier`; an unknown tier reads as muted, never as safe. */
export function tierColor(tier) {
  return TIER_COLORS[tier] ?? TIER_COLORS.missing;
}

/** The label for `tier`; an unknown tier is named, not blanked. */
export function tierLabel(tier) {
  return TIER_LABELS[tier] ?? String(tier ?? 'unknown');
}

/** The `Badge.svelte` tone for `tier`. */
export function tierTone(tier) {
  return TIER_TONES[tier] ?? 'muted';
}

/** Bytes as a short human string; `null` is an em dash, never `0 B`. */
export function formatBytes(bytes) {
  if (typeof bytes !== 'number' || !Number.isFinite(bytes)) return '—';
  if (bytes < 1024) return `${bytes} B`;
  if (bytes < 1024 ** 2) return `${(bytes / 1024).toFixed(1)} KB`;
  if (bytes < 1024 ** 3) return `${(bytes / 1024 ** 2).toFixed(1)} MB`;
  if (bytes < 1024 ** 4) return `${(bytes / 1024 ** 3).toFixed(2)} GB`;
  return `${(bytes / 1024 ** 4).toFixed(2)} TB`;
}

/** A measured byte figure, or 0 — the weight an unmeasured row carries. */
function weight(node) {
  const bytes = node?.bytes;
  return typeof bytes === 'number' && Number.isFinite(bytes) && bytes > 0 ? bytes : 0;
}

/** Three decimals is below a device pixel at any rendered size. */
function round(value) {
  return Number(value.toFixed(3));
}

/** The point at `degrees` clockwise from twelve o'clock, `r` from the centre. */
function polar(cx, cy, r, degrees) {
  const rad = ((degrees - 90) * Math.PI) / 180;
  return { x: cx + r * Math.cos(rad), y: cy + r * Math.sin(rad) };
}

/**
 * SVG path data for one annular segment.
 *
 * A 360° sweep is drawn as 359.999° — one arc command cannot express a full
 * circle, and a path whose start and end points coincide renders as nothing.
 */
export function arcPath(cx, cy, rInner, rOuter, startDeg, endDeg) {
  const sweep = endDeg - startDeg;
  if (!(sweep > 0)) return '';
  const stop = sweep >= 360 ? startDeg + 359.999 : endDeg;
  const large = stop - startDeg > 180 ? 1 : 0;
  const o0 = polar(cx, cy, rOuter, startDeg);
  const o1 = polar(cx, cy, rOuter, stop);
  const i1 = polar(cx, cy, rInner, stop);
  const i0 = polar(cx, cy, rInner, startDeg);
  return (
    `M${round(o0.x)} ${round(o0.y)}` +
    `A${rOuter} ${rOuter} 0 ${large} 1 ${round(o1.x)} ${round(o1.y)}` +
    `L${round(i1.x)} ${round(i1.y)}` +
    `A${rInner} ${rInner} 0 ${large} 0 ${round(i0.x)} ${round(i0.y)}Z`
  );
}

/**
 * Split `span` degrees among `items` by byte weight, folding the slivers.
 *
 * Returns `{ kept, collapsed, otherSweep }`, where `kept` are the items that
 * earned at least `minDegrees` and `collapsed` are the ones that did not. The
 * "other" wedge takes the collapsed items' combined share, floored at
 * `minDegrees` so a wedge of nothing but unmeasured rows is still wide enough
 * to click; the kept arcs are rescaled into what is left.
 *
 * Items with no measured bytes weigh nothing, so they always collapse — which
 * is the point: they are listed in the wedge rather than drawn as a 0° arc.
 */
function apportion(items, span, minDegrees) {
  const total = items.reduce((sum, item) => sum + weight(item), 0);
  if (items.length === 0 || span <= 0) {
    return { kept: [], collapsed: [], otherSweep: 0 };
  }
  // With no measured bytes anywhere, byte share says nothing — divide evenly
  // rather than folding the whole project into one grey wedge.
  const share = (item) => (total > 0 ? (weight(item) / total) * span : span / items.length);

  const kept = [];
  const collapsed = [];
  for (const item of items) {
    if (share(item) >= minDegrees) kept.push(item);
    else collapsed.push(item);
  }
  if (collapsed.length === 0) {
    return { kept: kept.map((item) => ({ item, sweep: share(item) })), collapsed, otherSweep: 0 };
  }
  const collapsedShare = collapsed.reduce((sum, item) => sum + share(item), 0);
  const otherSweep = Math.min(Math.max(collapsedShare, minDegrees), span);
  const keptShare = kept.reduce((sum, item) => sum + share(item), 0);
  const room = span - otherSweep;
  const scale = keptShare > 0 ? room / keptShare : 0;
  return {
    kept: kept.map((item) => ({ item, sweep: share(item) * scale })),
    collapsed,
    otherSweep,
  };
}

/**
 * The two rings the Disk view draws, from one `disk_survey` payload.
 *
 * @param {object} survey the `/api/console/disk/tree` body
 * @param {{ size?: number, minDegrees?: number }} [options]
 * @returns {{
 *   size: number, cx: number, cy: number, centre: object,
 *   projects: object[], worktrees: object[], empty: boolean
 * }}
 *
 * Every returned segment carries the row it was drawn from (`node`), so the
 * detail panel renders from this payload and never re-fetches — DOC-73 §16.3's
 * "a direct render of the struct, not a new computation."
 */
export function sunburstRings(survey, { size = 420, minDegrees = MIN_ARC_DEGREES } = {}) {
  const root = survey?.root ?? {};
  const projectList = Array.isArray(root.projects) ? root.projects : [];
  const cx = size / 2;
  const cy = size / 2;
  const rCentre = size * 0.16;
  const r1Inner = size * 0.2;
  const r1Outer = size * 0.3;
  const r2Inner = size * 0.32;
  const r2Outer = size * 0.47;

  const centre = {
    kind: 'root',
    path: root.path ?? '',
    bytes: root.bytes ?? null,
    counts: root.counts ?? null,
    staleBytes: root.stale_bytes ?? 0,
    radius: round(rCentre),
    node: root,
  };

  const projects = [];
  const worktrees = [];
  const apportioned = apportion(projectList, 360, minDegrees);
  let cursor = 0;

  const pushProject = (project, sweep) => {
    const start = cursor;
    const end = cursor + sweep;
    cursor = end;
    projects.push({
      kind: 'project',
      id: project.path ?? project.name ?? `project-${projects.length}`,
      name: project.name ?? project.path ?? '(unnamed)',
      bytes: project.bytes ?? null,
      color: NEUTRAL_COLOR,
      d: arcPath(cx, cy, r1Inner, r1Outer, start, end),
      node: project,
    });
    const inner = apportion(
      Array.isArray(project.worktrees) ? project.worktrees : [],
      sweep,
      minDegrees,
    );
    let wtCursor = start;
    for (const { item, sweep: wtSweep } of inner.kept) {
      const wtStart = wtCursor;
      wtCursor += wtSweep;
      worktrees.push({
        kind: 'worktree',
        id: item.id,
        name: item.branch ?? item.path ?? item.id,
        tier: item.tier,
        bytes: item.bytes ?? null,
        project: project.name ?? project.path ?? '',
        color: tierColor(item.tier),
        d: arcPath(cx, cy, r2Inner, r2Outer, wtStart, wtCursor),
        node: item,
      });
    }
    if (inner.collapsed.length > 0) {
      const otherStart = wtCursor;
      wtCursor += inner.otherSweep;
      worktrees.push({
        kind: 'other',
        id: `${project.path ?? project.name}::other`,
        name: `other (${inner.collapsed.length} items, ${formatBytes(
          inner.collapsed.reduce((sum, item) => sum + weight(item), 0),
        )})`,
        tier: null,
        bytes: inner.collapsed.reduce((sum, item) => sum + weight(item), 0),
        project: project.name ?? project.path ?? '',
        color: OTHER_COLOR,
        d: arcPath(cx, cy, r2Inner, r2Outer, otherStart, wtCursor),
        members: inner.collapsed,
        node: null,
      });
    }
  };

  for (const { item, sweep } of apportioned.kept) pushProject(item, sweep);
  // A collapsed PROJECT still gets an arc — folding a whole project into a grey
  // wedge would hide every worktree under it, and the fold exists to keep rows
  // reachable, not to hide them. It gets the floor width instead.
  const floorEach = apportioned.collapsed.length
    ? apportioned.otherSweep / apportioned.collapsed.length
    : 0;
  for (const project of apportioned.collapsed) pushProject(project, floorEach);

  return {
    size,
    cx,
    cy,
    centre,
    projects,
    worktrees,
    empty: projectList.length === 0,
  };
}

/** `bytes` descending, with unmeasured rows last and ties broken by name. */
function byBytesDesc(a, b) {
  const wa = weight(a);
  const wb = weight(b);
  if (wa !== wb) return wb - wa;
  const na = a?.branch ?? a?.name ?? a?.path ?? a?.id ?? '';
  const nb = b?.branch ?? b?.name ?? b?.path ?? b?.id ?? '';
  return String(na).localeCompare(String(nb));
}

/**
 * Sort worktrees for the narrow-width list (DOC-73 §16.3's fallback).
 *
 * `key` is `'bytes'`, `'tier'` or `'name'`; `direction` is `'desc'` or `'asc'`.
 * Tier order is the legend's, not the alphabet's — a "Safe to clear" column
 * that sorted `keep` above `review` would read as noise.
 */
export function sortWorktrees(worktrees, key = 'bytes', direction = 'desc') {
  const rows = [...(worktrees ?? [])];
  const flip = direction === 'asc' ? -1 : 1;
  if (key === 'tier') {
    rows.sort((a, b) => {
      const ia = TIERS.indexOf(a?.tier);
      const ib = TIERS.indexOf(b?.tier);
      if (ia !== ib) return (ia - ib) * flip;
      return byBytesDesc(a, b);
    });
    return rows;
  }
  if (key === 'name') {
    rows.sort((a, b) => {
      const na = String(a?.branch ?? a?.path ?? a?.id ?? '');
      const nb = String(b?.branch ?? b?.path ?? b?.id ?? '');
      return na.localeCompare(nb) * flip;
    });
    return rows;
  }
  rows.sort((a, b) => byBytesDesc(a, b) * flip);
  return rows;
}

/**
 * The flat list the narrow-width fallback renders (DOC-73 §16.3).
 *
 * Projects come out largest first, each followed by its own worktrees in the
 * requested order — the same rows and the same tiers the sunburst draws, with
 * nothing collapsed, so a phone-width console shows the whole fleet.
 */
export function flatRows(survey, { key = 'bytes', direction = 'desc' } = {}) {
  const projects = [...(survey?.root?.projects ?? [])].sort(byBytesDesc);
  const rows = [];
  for (const project of projects) {
    rows.push({
      kind: 'project',
      id: project.path ?? project.name,
      name: project.name ?? project.path ?? '(unnamed)',
      bytes: project.bytes ?? null,
      node: project,
    });
    for (const wt of sortWorktrees(project.worktrees, key, direction)) {
      rows.push({
        kind: 'worktree',
        id: wt.id,
        name: wt.branch ?? wt.path ?? wt.id,
        tier: wt.tier,
        bytes: wt.bytes ?? null,
        project: project.name ?? project.path ?? '',
        node: wt,
      });
    }
  }
  return rows;
}

/** The width below which the sunburst is replaced by the list (§16.3). */
export const NARROW_BREAKPOINT_PX = 600;

/** True when `width` is too narrow for a readable sunburst. */
export function isNarrow(width) {
  return typeof width === 'number' && Number.isFinite(width) && width < NARROW_BREAKPOINT_PX;
}
