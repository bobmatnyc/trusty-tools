import { describe, expect, test } from 'vitest';
import { apiUrl } from './base.js';

describe('apiUrl', () => {
  test('resolves a verb under /api/secrets/ on the page origin', () => {
    const url = new URL(apiUrl('session'));
    expect(url.pathname).toBe('/api/secrets/session');
    expect(url.origin).toBe(new URL(document.baseURI).origin);
  });
});
