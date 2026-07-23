// Unit tests for the pure agent-roster helpers (#3223, #3224 — Trusty
// Assistant agent roster, epic #3052). See `roster.ts` for the Why/What of
// each function under test.

import { describe, expect, it } from 'vitest';
import {
  BASE_AGENT_ID,
  BASE_AGENT_LABEL,
  buildOverlayMarkdown,
  buildRoster,
  parseOverlayMarkdown,
  rosterDisplayName,
  slugify,
  type CatalogAgent,
  type OverlayAgent,
} from './roster';

describe('slugify', () => {
  it('lowercases and hyphenates spaces', () => {
    expect(slugify('My CTO Assistant')).toBe('my-cto-assistant');
  });

  it('strips characters outside [a-z0-9_-]', () => {
    expect(slugify('Izzie!! 🤖')).toBe('izzie');
  });

  it('collapses runs of whitespace/invalid chars into a single hyphen', () => {
    expect(slugify('a   b---c')).toBe('a-b-c');
  });

  it('trims leading and trailing hyphens', () => {
    expect(slugify('  -leading and trailing-  ')).toBe('leading-and-trailing');
  });

  it('returns empty string for input that slugifies to nothing', () => {
    expect(slugify('!!!')).toBe('');
    expect(slugify('   ')).toBe('');
  });

  it('preserves underscores and existing hyphens/digits', () => {
    expect(slugify('my_assistant-2')).toBe('my_assistant-2');
  });
});

describe('buildRoster', () => {
  it('always includes the base assistant entry first, even with empty inputs', () => {
    const roster = buildRoster([], []);
    expect(roster).toHaveLength(1);
    expect(roster[0].id).toBe(BASE_AGENT_ID);
    expect(roster[0].source).toBe('base');
  });

  it('appends catalog agents after the base entry', () => {
    const catalog: CatalogAgent[] = [
      { name: 'cto-assistant', description: 'CTO Assistant' },
      { name: 'python-engineer' },
    ];
    const roster = buildRoster(catalog, []);
    expect(roster.map((r) => r.id)).toEqual([BASE_AGENT_ID, 'cto-assistant', 'python-engineer']);
    expect(roster[1].source).toBe('catalog');
    expect(roster[1].description).toBe('CTO Assistant');
  });

  it('dedupes a catalog entry literally named "assistant" against the base entry', () => {
    const catalog: CatalogAgent[] = [{ name: 'assistant' }, { name: 'engineer' }];
    const roster = buildRoster(catalog, []);
    expect(roster.filter((r) => r.id === BASE_AGENT_ID)).toHaveLength(1);
    expect(roster.map((r) => r.id)).toEqual([BASE_AGENT_ID, 'engineer']);
  });

  it('appends overlay agents, preferring display_name then name then slug as the label', () => {
    const overlays: OverlayAgent[] = [
      { slug: 'my-cto', name: 'my-cto', display_name: 'My CTO', extends: 'cto-assistant' },
      { slug: 'no-display-name', name: 'friendly-name' },
      { slug: 'bare-slug-only', name: '' },
    ];
    const roster = buildRoster([], overlays);
    const labels = roster.filter((r) => r.source === 'overlay').map((r) => r.label);
    expect(labels).toContain('My CTO');
    expect(labels).toContain('friendly-name');
    expect(labels).toContain('bare-slug-only');
    const myCto = roster.find((r) => r.id === 'my-cto');
    expect(myCto?.extends).toBe('cto-assistant');
  });

  it('dedupes an overlay slug that shadows a catalog entry or the base id', () => {
    const catalog: CatalogAgent[] = [{ name: 'cto-assistant' }];
    const overlays: OverlayAgent[] = [
      { slug: 'cto-assistant', name: 'cto-assistant' },
      { slug: BASE_AGENT_ID, name: 'assistant' },
      { slug: 'my-cto', name: 'my-cto' },
    ];
    const roster = buildRoster(catalog, overlays);
    expect(roster.filter((r) => r.id === 'cto-assistant')).toHaveLength(1);
    expect(roster.filter((r) => r.id === BASE_AGENT_ID)).toHaveLength(1);
    expect(roster.map((r) => r.id)).toContain('my-cto');
  });

  it('sorts overlay entries by label', () => {
    const overlays: OverlayAgent[] = [
      { slug: 'z-agent', name: 'Zed' },
      { slug: 'a-agent', name: 'Alpha' },
    ];
    const roster = buildRoster([], overlays);
    const overlayLabels = roster.filter((r) => r.source === 'overlay').map((r) => r.label);
    expect(overlayLabels).toEqual(['Alpha', 'Zed']);
  });

  it('prefers a catalog entry display_name over its dispatchable name for the label (#3737)', () => {
    const catalog: CatalogAgent[] = [
      { name: 'cto-assistant', display_name: 'CTO Assistant' },
      { name: 'izzie', display_name: 'Izzie' },
      { name: 'python-engineer' }, // no display_name → falls back to name
      { name: 'blank-display', display_name: '   ' }, // whitespace → falls back
    ];
    const roster = buildRoster(catalog, []);
    expect(roster.find((r) => r.id === 'cto-assistant')?.label).toBe('CTO Assistant');
    expect(roster.find((r) => r.id === 'izzie')?.label).toBe('Izzie');
    expect(roster.find((r) => r.id === 'python-engineer')?.label).toBe('python-engineer');
    expect(roster.find((r) => r.id === 'blank-display')?.label).toBe('blank-display');
  });
});

describe('rosterDisplayName (#3737)', () => {
  const roster = buildRoster(
    [
      { name: 'cto-assistant', display_name: 'CTO Assistant' },
      { name: 'izzie', display_name: 'Izzie' },
    ],
    [],
  );

  it('returns the base label "Assistant" when no agent is selected (null id)', () => {
    expect(rosterDisplayName(roster, null)).toBe(BASE_AGENT_LABEL);
    expect(rosterDisplayName(roster, '')).toBe(BASE_AGENT_LABEL);
  });

  it('resolves a selected roster entry to its friendly display name', () => {
    expect(rosterDisplayName(roster, 'izzie')).toBe('Izzie');
    expect(rosterDisplayName(roster, 'cto-assistant')).toBe('CTO Assistant');
    expect(rosterDisplayName(roster, BASE_AGENT_ID)).toBe(BASE_AGENT_LABEL);
  });

  it('falls back to the base label for an unknown/stale id', () => {
    expect(rosterDisplayName(roster, 'deleted-overlay')).toBe(BASE_AGENT_LABEL);
    expect(rosterDisplayName([], 'anything')).toBe(BASE_AGENT_LABEL);
  });
});

describe('buildOverlayMarkdown', () => {
  it('includes all fields when present', () => {
    const md = buildOverlayMarkdown('my-cto', {
      displayName: 'My CTO',
      extends: 'cto-assistant',
      instructions: 'Be terse.',
    });
    expect(md).toBe('---\nname: my-cto\nextends: cto-assistant\ndisplay_name: My CTO\n---\n\nBe terse.\n');
  });

  it('omits blank optional fields', () => {
    const md = buildOverlayMarkdown('bare', {
      displayName: '',
      extends: '',
      instructions: 'Hi.',
    });
    expect(md).toBe('---\nname: bare\n---\n\nHi.\n');
  });
});

describe('parseOverlayMarkdown', () => {
  it('round-trips buildOverlayMarkdown output', () => {
    const draft = { displayName: 'My CTO', extends: 'cto-assistant', instructions: 'Be terse.' };
    const parsed = parseOverlayMarkdown(buildOverlayMarkdown('my-cto', draft));
    expect(parsed).toEqual(draft);
  });

  it('treats content without leading frontmatter delimiter as instructions', () => {
    const parsed = parseOverlayMarkdown('Just some prose, no frontmatter.');
    expect(parsed).toEqual({
      displayName: '',
      extends: '',
      instructions: 'Just some prose, no frontmatter.',
    });
  });
});
