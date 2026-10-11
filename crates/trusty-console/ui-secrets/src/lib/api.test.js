import { describe, expect, test } from 'vitest';
import { CSRF_HEADER, SecretsApiError, createApi } from './api.js';
import { PROJECT, TOKEN, VAULT, fakeConsole } from './fake.js';

describe('createApi', () => {
  // #9067: one session fetch, then the token on every call.
  test('sends the CSRF header on every call after one session fetch', async () => {
    const { fetchImpl, calls } = fakeConsole();
    const api = createApi(fetchImpl);
    await api.projects();
    await api.scopes(PROJECT);
    await api.backends(PROJECT);
    await api.list(PROJECT, VAULT);
    await api.set(PROJECT, VAULT, 'K', 'v');
    await api.remove(PROJECT, VAULT, 'K');
    await api.copy(PROJECT, 'keychain', 'file');
    await api.agentsMayUse(PROJECT, VAULT, 'K', false);

    const sessions = calls.filter((c) => c.verb === 'session');
    expect(sessions).toHaveLength(1);
    expect(calls[0].verb).toBe('session');
    expect(calls[0].headers[CSRF_HEADER]).toBeUndefined();
    const rest = calls.slice(1);
    expect(rest.map((c) => c.verb)).toEqual([
      'projects', 'scopes', 'backends', 'list', 'set', 'delete', 'copy', 'agents_may_use',
    ]);
    for (const c of rest) {
      expect(c.method).toBe('POST');
      expect(c.headers[CSRF_HEADER]).toBe(TOKEN);
      expect(c.headers['Content-Type']).toBe('application/json');
      expect(new URL(c.url).pathname).toBe(`/api/secrets/${c.verb}`);
    }
  });

  test('copy sends backend ids and keys, and no vault', async () => {
    const { fetchImpl, calls } = fakeConsole();
    await createApi(fetchImpl).copy(PROJECT, 'keychain', 'file');
    const copy = calls.find((c) => c.verb === 'copy');
    expect(copy.body).toEqual({ project: PROJECT, from_backend: 'keychain', to_backend: 'file', keys: [] });
    expect('vault' in copy.body).toBe(false);
  });

  test('a refusal throws its status, text and kind', async () => {
    const { fetchImpl } = fakeConsole({
      list: () => [403, { error: 'the secrets server refused the request', kind: 'vault_out_of_scope' }],
    });
    const err = await createApi(fetchImpl).list(PROJECT, VAULT).catch((e) => e);
    expect(err).toBeInstanceOf(SecretsApiError);
    expect(err.status).toBe(403);
    expect(err.kind).toBe('vault_out_of_scope');
  });

  test('a failed session fetch is retried by the next call', async () => {
    let first = true;
    const { fetchImpl, calls } = fakeConsole({
      session: () => {
        if (first) {
          first = false;
          return [503, { error: 'the secrets bridge is not available' }];
        }
        return [200, { csrf: TOKEN }];
      },
    });
    const api = createApi(fetchImpl);
    await expect(api.projects()).rejects.toBeInstanceOf(SecretsApiError);
    await api.projects();
    expect(calls.filter((c) => c.verb === 'session')).toHaveLength(2);
  });
});
