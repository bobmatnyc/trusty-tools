/*
 * Why: `__SEARCH_BASE__` is both the console's injected bridge prefix and a
 * general override (base.test.js "override still wins"). Only the console's
 * own prefix on this page's origin means "reached over the socket bridge".
 * What: table of injected values against the console-served verdict.
 * Test: this file.
 */
import { describe, expect, it } from 'vitest';
import { chatUnavailableReason, isConsoleServed } from './transport.js';

const win = (base) => ({ __SEARCH_BASE__: base, location: { origin: 'http://localhost:7788' } });

describe('isConsoleServed', () => {
  it.each([
    ['/api/search/', true],
    ['http://localhost:7788/api/search/', true], // what tools_ui.rs injects
    ['http://example.test:9000/custom', false], // an absolute override elsewhere
    ['http://example.test:9000/api/search/', false], // right path, another host
    ['/proxy/search/', false],
    ['', false],
    [undefined, false]
  ])('%s -> %s', (base, expected) => {
    expect(isConsoleServed(win(base))).toBe(expected);
  });
});

describe('chatUnavailableReason (#9030)', () => {
  it.each(['console', 'daemon-http'])('%s: never claims a missing socket method', (mode) => {
    expect(chatUnavailableReason(mode)).not.toMatch(/no .*socket method/i);
  });

  it('console: names the daemon provider as the cause', () => {
    expect(chatUnavailableReason('console')).toMatch(/no chat provider configured on the daemon/);
  });
});
