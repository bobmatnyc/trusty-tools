/*
 * Why: #9067 — every `/api/secrets/*` call but `session` must carry the
 * per-launch CSRF token in `x-trusty-secrets-csrf` (the console's guard
 * refuses it otherwise, `projects` included), and the token must be fetched
 * once, not per call.
 * What: `createApi(fetchImpl)` returns one method per route. Each is a JSON
 * `POST`; the first call fetches `session` and every later call reuses that
 * token. A non-2xx answer throws `SecretsApiError` carrying the status and
 * the console's `error` and `kind`, never anything the page sent. `copy`
 * takes backend ids and keys only: there is no vault in its body.
 * Test: `src/lib/api.test.js`.
 */

import { apiUrl } from './base.js';

export const CSRF_HEADER = 'x-trusty-secrets-csrf';

export class SecretsApiError extends Error {
  /**
   * @param {number} status  0 when the console could not be reached
   * @param {{error?: string, kind?: string}} body
   */
  constructor(status, body) {
    super(typeof body?.error === 'string' ? body.error : `HTTP ${status}`);
    this.status = status;
    this.error = typeof body?.error === 'string' ? body.error : undefined;
    this.kind = typeof body?.kind === 'string' ? body.kind : undefined;
  }
}

async function readJson(res) {
  try {
    return await res.json();
  } catch {
    return {};
  }
}

/**
 * @param {typeof fetch} [fetchImpl]
 */
export function createApi(fetchImpl = (...args) => fetch(...args)) {
  let token = null;

  async function send(verb, body, headers) {
    let res;
    try {
      res = await fetchImpl(apiUrl(verb), {
        method: 'POST',
        credentials: 'same-origin',
        headers: { 'Content-Type': 'application/json', ...headers },
        body: JSON.stringify(body),
      });
    } catch {
      throw new SecretsApiError(0, {});
    }
    const json = await readJson(res);
    if (!res.ok) throw new SecretsApiError(res.status, json);
    return json;
  }

  function csrf() {
    // One session fetch per page; a failed one is retried by the next call.
    if (!token) {
      token = send('session', {}, {}).then(
        (answer) => answer.csrf,
        (err) => {
          token = null;
          throw err;
        },
      );
    }
    return token;
  }

  async function call(verb, body) {
    const value = await csrf();
    return send(verb, body, { [CSRF_HEADER]: value });
  }

  return {
    projects: () => call('projects', {}),
    scopes: (project) => call('scopes', { project }),
    backends: (project) => call('backends', { project }),
    list: (project, vault) => call('list', { project, vault }),
    set: (project, vault, key, value) => call('set', { project, vault, key, value }),
    remove: (project, vault, key) => call('delete', { project, vault, key }),
    copy: (project, fromBackend, toBackend, keys = []) =>
      call('copy', { project, from_backend: fromBackend, to_backend: toBackend, keys }),
    agentsMayUse: (project, vault, key, allowed) =>
      call('agents_may_use', { project, vault, key, allowed }),
  };
}
