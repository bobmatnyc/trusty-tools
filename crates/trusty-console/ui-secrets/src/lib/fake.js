/*
 * Test-only: a fake console for the page and client tests (#9067).
 * What: `fakeConsole(overrides)` returns `{ fetchImpl, calls, hold }`. Each
 * call records its verb, headers and decoded body; each verb answers from a
 * handler `(body) => [status, json]`, overridable per test. `hold(verb)`
 * holds the next call of that verb until the returned `release()` runs.
 */

export const TOKEN = 'ab'.repeat(32);
export const PROJECT = '/work/acme-web';
export const VAULT = 'trusty/acme/web';

export function defaultHandlers() {
  return {
    session: () => [200, { csrf: TOKEN }],
    projects: () => [200, { projects: [{ name: 'acme-web', path: PROJECT }] }],
    scopes: () => [
      200,
      {
        scopes: [
          { kind: 'project', vault: VAULT },
          { kind: 'owner', vault: 'trusty/acme' },
        ],
      },
    ],
    backends: () => [
      200,
      {
        selected: 'keychain',
        backends: [
          { id: 'keychain', available: true },
          { id: 'file', available: true },
          { id: 'onepassword', available: false },
        ],
      },
    ],
    list: () => [
      200,
      {
        vault: VAULT,
        keys: [{ name: 'API_KEY', length: 32, updated_at: '2026-10-11T12:00:00Z', agents_may_use: false }],
      },
    ],
    set: () => [200, { outcome: 'new', masked: 'sk-live-… [40 chars]' }],
    delete: () => [200, { removed: true }],
    copy: () => [200, { copied: ['API_KEY'], failed: [] }],
    agents_may_use: (body) => [200, { vault: body.vault, key: body.key, allowed: body.allowed }],
  };
}

export function fakeConsole(overrides = {}) {
  const handlers = { ...defaultHandlers(), ...overrides };
  const calls = [];
  const gates = {};
  // #9712: a held call is recorded at once and answers only on release.
  function hold(verb) {
    let release;
    const gate = new Promise((resolve) => {
      release = resolve;
    });
    (gates[verb] ??= []).push(gate);
    return () => release();
  }
  async function fetchImpl(url, init) {
    const verb = new URL(url).pathname.split('/').pop();
    const body = JSON.parse(init.body);
    calls.push({ verb, url, method: init.method, headers: { ...init.headers }, body });
    const gate = gates[verb]?.shift();
    if (gate) await gate;
    const handler = handlers[verb];
    const [status, json] = handler ? handler(body) : [404, { error: 'no such secrets route' }];
    return {
      ok: status >= 200 && status < 300,
      status,
      json: async () => json,
    };
  }
  return { fetchImpl, calls, hold };
}
