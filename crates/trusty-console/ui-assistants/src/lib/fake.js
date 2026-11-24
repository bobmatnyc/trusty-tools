/*
 * Test-only: a fake console for the page and client tests (#11749).
 * What: `fakeConsole(overrides)` returns `{ fetchImpl, calls, hold }`. Each
 * call records its route (`list`, `turns`, `ask`), instance id, method, URL,
 * init and decoded body. Each route answers from a handler
 * `({ id, body, url }) => [status, json]`, overridable per test. `hold(route)`
 * holds the next call of that route until the returned `release()` runs.
 */

export const TURNS = {
  izzie: [
    { at: '2026-11-20T10:00:00Z', question: 'first question', answer: 'first answer' },
    { at: '2026-11-21T10:00:00Z', question: 'second question', answer: 'second answer' },
  ],
  cto: [{ at: '2026-11-22T10:00:00Z', question: 'cto question', answer: 'cto answer' }],
};

function turnsBody(id, extra = {}) {
  const turns = TURNS[id] ?? [];
  return {
    instance: id,
    order: 'oldest_first',
    limit: 50,
    total: turns.length,
    skipped: 0,
    truncated: false,
    turns,
    ...extra,
  };
}

export function defaultHandlers() {
  return {
    list: () => [200, { instances: [{ id: 'cto' }, { id: 'izzie' }] }],
    turns: ({ id }) => [200, turnsBody(id)],
    ask: ({ id, body }) => [200, { instance: id, answer: `answer to ${body.question}` }],
  };
}

export { turnsBody };

function route(url) {
  const path = new URL(url, 'http://console.invalid').pathname;
  const m = path.match(/^\/api\/console\/assistants(?:\/([^/]+)\/(turns|ask))?$/);
  if (!m) return { name: 'unknown', id: undefined };
  return { name: m[2] ?? 'list', id: m[1] === undefined ? undefined : decodeURIComponent(m[1]) };
}

export function fakeConsole(overrides = {}) {
  const handlers = { ...defaultHandlers(), ...overrides };
  const calls = [];
  const holds = {};

  async function fetchImpl(url, init = {}) {
    const { name, id } = route(url);
    const body = init.body ? JSON.parse(init.body) : undefined;
    calls.push({ route: name, id, url, method: init.method, init, body });
    const held = holds[name]?.shift();
    if (held) await held;
    const handler = handlers[name];
    const [status, json] = handler ? handler({ id, body, url }) : [404, { error: 'no route' }];
    if (status === 0) throw new TypeError('network down');
    return {
      ok: status >= 200 && status < 300,
      status,
      json: async () => json,
    };
  }

  /** Hold the next `name` call until `release()`. */
  function hold(name) {
    let release;
    const gate = new Promise((resolve) => {
      release = resolve;
    });
    (holds[name] ??= []).push(gate);
    return release;
  }

  return { fetchImpl, calls, hold };
}
