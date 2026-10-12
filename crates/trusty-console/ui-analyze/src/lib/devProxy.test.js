// The dev proxy reaches the console as a non-browser client (#9792).
//
// Why: the console refuses a request whose Host is not its own, or whose
// Origin is not http://<Host>. A dev proxy forwarding the browser's
// localhost:517x Host or Origin would be refused on every call.
// What: loads this package's real vite.config.js and checks every proxy row
// targets the console's /api/analyze bridge, rewrites Host, and drops only its own Origin.
// Test: this file — `pnpm test`. It runs under node: importing the config
// loads esbuild, which refuses jsdom's TextEncoder.
// @vitest-environment node

import { describe, expect, it } from 'vitest';

import config from '../../vite.config.js';

describe('vite dev proxy (#9792)', () => {
  const rows = Object.entries(config.server.proxy);

  it('has rows, each to the console bridge with changeOrigin', () => {
    expect(rows.length).toBeGreaterThan(0);
    for (const [, row] of rows) {
      expect(row.target).toBe('http://127.0.0.1:7788/api/analyze');
      expect(row.changeOrigin).toBe(true);
    }
  });

  // Runs every row's proxyReq handler for a browser request and returns the
  // headers the console would receive.
  function forward(row, host, origin) {
    const handlers = {};
    row.configure({ on: (event, fn) => (handlers[event] = fn) });
    const sent = origin === undefined ? {} : { origin };
    handlers.proxyReq(
      { removeHeader: (name) => delete sent[name.toLowerCase()] },
      { headers: origin === undefined ? { host } : { host, origin } },
    );
    return sent;
  }

  const host = `localhost:${config.server.port}`;

  it('removes the dev server\'s own Origin', () => {
    for (const [path, row] of rows) {
      expect(forward(row, host, `http://${host}`), path).toEqual({});
    }
  });

  it('forwards any other Origin unchanged so the console refuses it', () => {
    const foreign = [
      'https://evil.example',
      'http://localhost:9999',
      `https://${host}`,
      'http://127.0.0.1:' + config.server.port,
      'null',
    ];
    for (const [path, row] of rows) {
      for (const origin of foreign) {
        expect(forward(row, host, origin), `${path} ${origin}`).toEqual({ origin });
      }
    }
  });

  it('leaves a request with no Origin unchanged', () => {
    for (const [path, row] of rows) {
      expect(forward(row, host, undefined), path).toEqual({});
    }
  });
});
