import { describe, expect, test } from 'vitest';
import { AssistantsApiError, BUSY_TEXT, UNREACHABLE_TEXT, createApi, errorLines } from './api.js';
import { fakeConsole } from './fake.js';

describe('createApi', () => {
  test('ask is a same-origin JSON POST carrying only the question', async () => {
    const fake = fakeConsole();
    const answer = await createApi(fake.fetchImpl).ask('izzie', 'what is on fire');
    expect(answer.answer).toBe('answer to what is on fire');
    const [call] = fake.calls;
    expect(call.route).toBe('ask');
    expect(call.method).toBe('POST');
    expect(call.body).toEqual({ question: 'what is on fire' });
    expect(call.init.headers['Content-Type']).toBe('application/json');
    // The default credentials mode only; no cross-origin URL.
    expect(call.init.credentials).toBeUndefined();
    expect(new URL(call.url).origin).toBe(new URL(document.baseURI).origin);
  });

  test('a non-2xx answer throws with the status and the body strings', async () => {
    const fake = fakeConsole({
      ask: () => [503, { error: 'assistant not running', instance: 'izzie', hint: 'run `ta ask-daemon izzie`' }],
    });
    const err = await createApi(fake.fetchImpl).ask('izzie', 'q').catch((e) => e);
    expect(err).toBeInstanceOf(AssistantsApiError);
    expect(err.status).toBe(503);
    expect(err.hint).toBe('run `ta ask-daemon izzie`');
  });

  test('a failed fetch throws status 0', async () => {
    const fake = fakeConsole({ list: () => [0, {}] });
    const err = await createApi(fake.fetchImpl).list().catch((e) => e);
    expect(err.status).toBe(0);
    expect(errorLines(err)).toEqual([UNREACHABLE_TEXT]);
  });
});

describe('errorLines', () => {
  const cases = [
    [503, { error: 'assistant not running', hint: 'HINT verbatim `x`' }, ['assistant not running', 'HINT verbatim `x`']],
    [429, { error: 'assistants busy', kind: 'busy' }, [BUSY_TEXT]],
    [504, { error: 'the assistant did not answer in time', kind: 'timeout' }, ['the assistant did not answer in time (timeout)']],
    [502, { error: 'the assistant could not answer', kind: 'ask' }, ['the assistant could not answer (ask)']],
    [413, { error: 'a question is at most 16384 bytes' }, ['a question is at most 16384 bytes']],
    [400, { error: 'the question may not be blank' }, ['the question may not be blank']],
    [404, { error: 'unknown assistant instance `x`', available: ['cto', 'izzie'] }, ['unknown assistant instance `x`', 'available: cto, izzie']],
    [500, {}, ['HTTP 500']],
  ];
  test.each(cases)('status %i', (status, body, want) => {
    expect(errorLines(new AssistantsApiError(status, body))).toEqual(want);
  });
});
