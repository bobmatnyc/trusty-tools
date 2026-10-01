/*
 * Why: the "Daemon details" panel showed "Daemon port 7878" (an index.html
 * default no server injects under the console) and "API base URL" (the
 * console's own origin) as if they were how the daemon is reached. Under the
 * console the daemon is reached over its Unix socket (ADR-0032).
 * What: mounts the real Configuration view with the API module mocked and
 * checks each serving mode shows only transport facts some source supplied.
 * Test: this file.
 */
import { afterEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';

vi.mock('../api.js', () => ({
  api: { getConfig: vi.fn().mockResolvedValue({}), health: vi.fn() }
}));

import { api } from '../api.js';
import { refreshHealth } from '../state.svelte.js';
import Config from './Config.svelte';

let target = null;
let instance = null;

afterEach(async () => {
  if (instance) unmount(instance);
  instance = null;
  target?.remove();
  target = null;
  delete window.__SEARCH_BASE__;
  delete window.__DAEMON_PORT__;
  api.health.mockResolvedValue({ status: 'ok' });
  await refreshHealth();
});

function details() {
  target = document.createElement('div');
  document.body.appendChild(target);
  instance = mount(Config, { target });
  flushSync();
  const card = [...target.querySelectorAll('.card')].find((c) =>
    c.querySelector('.card-header')?.textContent.includes('Daemon details')
  );
  return Object.fromEntries(
    [...card.querySelectorAll('tr')].map((tr) => [
      tr.querySelector('th').textContent.trim(),
      tr.querySelector('td').textContent.replace(/\s+/g, ' ').trim()
    ])
  );
}

describe('Daemon details', () => {
  it('under the console, shows the socket bridge and no daemon TCP port or localhost URL', () => {
    window.__SEARCH_BASE__ = 'http://localhost:7788/api/search/';
    const rows = details();
    expect(Object.keys(rows)).not.toContain('Daemon port');
    expect(Object.keys(rows)).not.toContain('API base URL');
    expect(Object.values(rows).join(' ')).not.toMatch(/7878|7788|localhost/);
    expect(rows['Dashboard reaches the daemon']).toContain("/api/search/ is bridged to the daemon's Unix socket");
    expect(rows['Daemon socket']).toBe('not reported by the daemon');
    expect(rows['Daemon HTTP listener']).toBe('not reported by the daemon');
    expect(rows['Chat']).toContain('not available through the console');
    expect(rows['Chat']).not.toContain('restart the daemon');
  });

  it('served by the daemon itself, shows the HTTP port the daemon injected', () => {
    window.__DAEMON_PORT__ = 7878;
    const rows = details();
    expect(rows['Dashboard reaches the daemon']).toContain("directly, over the daemon's HTTP listener");
    expect(rows['Daemon HTTP listener']).toBe('port 7878');
    expect(rows['Chat']).toContain('OPENROUTER_API_KEY');
  });

  it('shows the socket path and listener state the daemon reports', async () => {
    window.__SEARCH_BASE__ = 'http://localhost:7788/api/search/';
    api.health.mockResolvedValue({
      status: 'ok',
      transport: { socket_path: '/data/trusty-search/search.sock', http_addr: null }
    });
    await refreshHealth();
    const rows = details();
    expect(rows['Daemon socket']).toBe('/data/trusty-search/search.sock');
    expect(rows['Daemon HTTP listener']).toBe('none');
  });
});
