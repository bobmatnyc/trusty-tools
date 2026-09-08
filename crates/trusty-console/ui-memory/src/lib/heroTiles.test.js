/**
 * Tests for the dashboard hero row (#6928).
 *
 * The acceptance criterion is verbatim: "The hero row renders exactly seven
 * tiles, including RAM and Disk, both with byte units."
 */

import { describe, expect, it } from 'vitest';

import {
  HERO_TILE_COUNT,
  HERO_TILE_IDS,
  UNKNOWN,
  formatBytes,
  heroTiles,
} from './heroTiles.js';

/** The live figures quoted in the owner ruling, plus schema 5's two additions. */
const LIVE = {
  palace_count: 94,
  counted_palace_count: 94,
  total_drawers: 5176,
  total_vectors: 5018,
  total_rooms: 131,
  total_kg_triples: 60375,
  ram_bytes: 1815939688,
  disk_bytes: 1797156864,
};

describe('the hero row', () => {
  it('renders exactly seven tiles', () => {
    expect(heroTiles(LIVE)).toHaveLength(HERO_TILE_COUNT);
    expect(HERO_TILE_COUNT).toBe(7);
  });

  it('still carries the five counts the ruling was made in front of', () => {
    const by = Object.fromEntries(heroTiles(LIVE).map((t) => [t.id, t]));
    expect(by.palaces.value).toBe('94');
    expect(by.palaces.sub).toBe('of 94 on disk');
    expect(by.drawers.value).toBe('5,176');
    expect(by.vectors.value).toBe('5,018');
    expect(by.rooms.value).toBe('131');
    expect(by['kg-triples'].value).toBe('60,375');
  });

  it('includes RAM and Disk, both with byte units', () => {
    const tiles = heroTiles(LIVE);
    expect(tiles.map((t) => t.id)).toEqual(HERO_TILE_IDS);

    const byteTiles = tiles.filter((t) => t.unit === 'bytes');
    expect(byteTiles.map((t) => t.id)).toEqual(['ram', 'disk']);
    for (const t of byteTiles) {
      expect(t.value).toMatch(/^[\d.]+ (B|KB|MB|GB)$/);
    }
    expect(byteTiles[0].value).toBe('1.69 GB');
    expect(byteTiles[1].value).toBe('1.67 GB');
  });

  it('is still seven tiles when the daemon reports neither figure', () => {
    // A pre-schema-5 daemon: the row keeps its shape and says so per tile,
    // rather than dropping a tile and silently becoming a five-tile row again.
    const tiles = heroTiles({ palace_count: 3 });
    expect(tiles).toHaveLength(HERO_TILE_COUNT);
    const by = Object.fromEntries(tiles.map((t) => [t.id, t]));
    expect(by.ram.value).toBe(UNKNOWN);
    expect(by.disk.value).toBe(UNKNOWN);
  });

  it('is still seven tiles with no payload at all', () => {
    expect(heroTiles(undefined)).toHaveLength(HERO_TILE_COUNT);
    expect(heroTiles(null).every((t) => typeof t.value === 'string')).toBe(true);
  });
});

describe('byte formatting', () => {
  it('crosses each unit boundary once', () => {
    expect(formatBytes(0)).toBe('0 B');
    expect(formatBytes(1023)).toBe('1023 B');
    expect(formatBytes(1024)).toBe('1.0 KB');
    expect(formatBytes(1024 * 1024)).toBe('1.0 MB');
    expect(formatBytes(1024 ** 3)).toBe('1.00 GB');
  });

  it('never turns an unreadable figure into zero bytes', () => {
    for (const bad of [null, undefined, NaN, -1, '5']) {
      expect(formatBytes(bad)).toBe(UNKNOWN);
    }
  });
});
