/*
 * Why: #11749 — the page reads the instance list and one instance's turns,
 * and posts one question at a time. The console answers each failure with a
 * status and a JSON `error` (plus `hint`, `kind` or `available`), and the
 * page must show those texts, never anything it sent itself.
 * What: `createApi(fetchImpl)` returns `list()`, `turns(id, limit)` and
 * `ask(id, question)`. Every call is a same-origin `fetch` with the default
 * credentials mode; `ask` is a JSON `POST`, whose `Origin` the browser adds.
 * A non-2xx answer throws `AssistantsApiError` with the status and the body's
 * strings. `errorLines(err)` turns one into the lines the page shows.
 * Test: `src/lib/api.test.js`.
 */

import { askUrl, listUrl, turnsUrl } from './base.js';

/** What a 429 shows (#11748: the console's ask pool is full). */
export const BUSY_TEXT = 'assistants busy, try again';

/** What a failed fetch shows: no status came back. */
export const UNREACHABLE_TEXT = 'the console could not be reached';

const str = (v) => (typeof v === 'string' ? v : undefined);

export class AssistantsApiError extends Error {
  /**
   * @param {number} status  0 when the console could not be reached
   * @param {{error?: string, kind?: string, hint?: string, available?: string[]}} body
   */
  constructor(status, body) {
    super(str(body?.error) ?? `HTTP ${status}`);
    this.status = status;
    this.error = str(body?.error);
    this.kind = str(body?.kind);
    this.hint = str(body?.hint);
    this.available = Array.isArray(body?.available) ? body.available.filter((a) => typeof a === 'string') : undefined;
  }
}

/**
 * The lines the page shows for a failed call.
 *
 * 503 shows the console's `hint` verbatim; 429 the fixed busy text; 404 the
 * error and the instances the console named; every other status the error
 * text with its `kind` when there is one.
 * @param {unknown} err
 * @returns {string[]}
 */
export function errorLines(err) {
  if (!(err instanceof AssistantsApiError)) return [String(err)];
  if (err.status === 0) return [UNREACHABLE_TEXT];
  if (err.status === 429) return [BUSY_TEXT];
  const head = err.error ?? `HTTP ${err.status}`;
  if (err.status === 503 && err.hint) return [head, err.hint];
  if (err.status === 404 && err.available) {
    return [head, `available: ${err.available.length ? err.available.join(', ') : 'none'}`];
  }
  return [err.kind ? `${head} (${err.kind})` : head];
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
  async function send(url, init) {
    let res;
    try {
      res = await fetchImpl(url, init);
    } catch {
      throw new AssistantsApiError(0, {});
    }
    const json = await readJson(res);
    if (!res.ok) throw new AssistantsApiError(res.status, json);
    return json;
  }

  return {
    list: () => send(listUrl(), { method: 'GET' }),
    turns: (id, limit) => send(turnsUrl(id, limit), { method: 'GET' }),
    ask: (id, question) =>
      send(askUrl(id), {
        method: 'POST',
        headers: { 'Content-Type': 'application/json' },
        body: JSON.stringify({ question }),
      }),
  };
}
