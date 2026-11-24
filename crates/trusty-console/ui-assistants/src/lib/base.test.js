import { describe, expect, test } from 'vitest';
import { askUrl, listUrl, turnsUrl } from './base.js';

describe('assistants URLs', () => {
  test('resolve under /api/console/assistants on the page origin', () => {
    const origin = new URL(document.baseURI).origin;
    expect(new URL(listUrl()).pathname).toBe('/api/console/assistants');
    expect(new URL(listUrl()).origin).toBe(origin);
    const turns = new URL(turnsUrl('izzie', 50));
    expect(turns.pathname).toBe('/api/console/assistants/izzie/turns');
    expect(turns.searchParams.get('limit')).toBe('50');
    expect(new URL(askUrl('izzie')).pathname).toBe('/api/console/assistants/izzie/ask');
  });

  test('encode the instance id as one path segment', () => {
    expect(new URL(askUrl('a/b')).pathname).toBe('/api/console/assistants/a%2Fb/ask');
  });
});
