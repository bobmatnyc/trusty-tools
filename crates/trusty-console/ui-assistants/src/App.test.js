import { afterEach, beforeEach, describe, expect, test } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import App from './App.svelte';
import { BUSY_TEXT, createApi } from './lib/api.js';
import { fakeConsole, turnsBody } from './lib/fake.js';

let target;
let app;

beforeEach(() => {
  target = document.createElement('div');
  document.body.appendChild(target);
});

afterEach(() => {
  if (app) unmount(app);
  app = null;
  target.remove();
});

/** Let every pending fetch resolve and Svelte flush. */
async function settle() {
  for (let i = 0; i < 10; i += 1) {
    await new Promise((resolve) => setTimeout(resolve, 0));
    flushSync();
  }
}

const q = (id) => target.querySelector(`[data-testid="${id}"]`);
const all = (id) => [...target.querySelectorAll(`[data-testid="${id}"]`)];
const texts = (id) => all(id).map((el) => el.textContent.trim());
const instance = (id) => target.querySelector(`[data-testid="instance"][data-id="${id}"]`);

/** Mount the page over a fake console and let the first load settle. */
async function open(overrides) {
  const fake = fakeConsole(overrides);
  app = mount(App, { target, props: { api: createApi(fake.fetchImpl) } });
  await settle();
  return fake;
}

function type(value) {
  const el = q('question');
  el.value = value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
}

function submit() {
  q('ask-form').dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
  flushSync();
}

describe('instance list', () => {
  test('lists the instances and opens the first one, oldest turn first', async () => {
    await open();
    expect(all('instance').map((b) => b.dataset.id)).toEqual(['cto', 'izzie']);
    expect(instance('cto').getAttribute('aria-current')).toBe('true');
    instance('izzie').click();
    await settle();
    expect(texts('turn-question')).toEqual(['first question', 'second question']);
    expect(texts('turn-answer')).toEqual(['first answer', 'second answer']);
  });

  test('an empty list says "no assistant instances" and asks for no history', async () => {
    const fake = await open({ list: () => [200, { instances: [] }] });
    expect(q('no-instances').textContent.trim()).toBe('no assistant instances');
    expect(fake.calls.filter((c) => c.route === 'turns')).toHaveLength(0);
    expect(q('ask-form')).toBeNull();
  });
});

describe('history notes', () => {
  test('a skipped count and a truncated read are both shown', async () => {
    await open({ turns: ({ id }) => [200, turnsBody(id, { skipped: 3, truncated: true })] });
    expect(q('skipped-note').textContent).toContain('3 lines of the history could not be read');
    expect(q('truncated-note').textContent).toContain('showing the newest part of the history');
  });

  test('a clean read shows neither note', async () => {
    await open();
    expect(q('skipped-note')).toBeNull();
    expect(q('truncated-note')).toBeNull();
  });

  test("a late answer for an instance no longer selected is dropped", async () => {
    const fake = await open();
    const releaseIzzie = fake.hold('turns');
    const releaseCto = fake.hold('turns');
    instance('izzie').click();
    await settle();
    instance('cto').click();
    await settle();
    releaseCto();
    await settle();
    releaseIzzie();
    await settle();
    expect(texts('turn-question')).toEqual(['cto question']);
  });
});

describe('ask', () => {
  test('submit is disabled and pending while an ask runs; a 200 appends the turn', async () => {
    const fake = await open();
    instance('izzie').click();
    await settle();
    expect(q('ask').disabled).toBe(true);
    type('what is on fire');
    expect(q('ask').disabled).toBe(false);
    const release = fake.hold('ask');
    submit();
    await settle();
    expect(q('ask').disabled).toBe(true);
    expect(q('ask').textContent.trim()).toBe('Asking…');
    expect(q('ask-pending').textContent).toContain('asking izzie');
    submit();
    await settle();
    expect(fake.calls.filter((c) => c.route === 'ask')).toHaveLength(1);
    release();
    await settle();
    expect(texts('turn-question')).toEqual(['first question', 'second question', 'what is on fire']);
    expect(texts('turn-answer').at(-1)).toBe('answer to what is on fire');
    expect(q('question').value).toBe('');
    expect(q('ask').textContent.trim()).toBe('Ask');
  });

  test('an answer with no history loaded is shown in a notice, not lost', async () => {
    await open({ turns: () => [500, { error: 'cannot read the turn log' }] });
    expect(q('history-error')).not.toBeNull();
    type('still there?');
    submit();
    await settle();
    expect(q('ask-answer').textContent).toContain('answer to still there?');
    expect(q('ask-answer').textContent).toContain('still there?');
  });

  test('a reselection mid-ask reloads the history instead of appending a duplicate', async () => {
    let logged = false;
    const fake = await open({
      turns: ({ id }) => {
        const extra = id === 'cto' && logged ? [{ at: 't', question: 'mid', answer: 'answer to mid' }] : [];
        return [200, turnsBody(id, { turns: [...turnsBody(id).turns, ...extra] })];
      },
    });
    type('mid');
    const release = fake.hold('ask');
    submit();
    await settle();
    // The daemon has logged the turn; its reply is still on the way.
    logged = true;
    instance('izzie').click();
    await settle();
    instance('cto').click();
    await settle();
    release();
    await settle();
    expect(texts('turn-question')).toEqual(['cto question', 'mid']);
  });

  test('model output renders as text, never as HTML', async () => {
    const evil = '<img src=x onerror="window.__pwned=1"><b>bold</b>';
    await open({ ask: () => [200, { instance: 'cto', answer: evil }] });
    type('hi');
    submit();
    await settle();
    expect(texts('turn-answer').at(-1)).toBe(evil);
    expect(target.querySelector('img')).toBeNull();
    expect(target.querySelector('b')).toBeNull();
  });

  const hint = 'run `ta ask-daemon cto` in a terminal and leave it running; the console never starts an assistant';
  const failures = [
    ['503 shows the hint verbatim', 503, { error: 'assistant not running', instance: 'cto', hint }, hint],
    ['429 says busy', 429, { error: 'assistants busy', kind: 'busy' }, BUSY_TEXT],
    ['504 shows the error and kind', 504, { error: 'the assistant did not answer in time', kind: 'timeout' }, 'the assistant did not answer in time (timeout)'],
    ['502 shows the error and kind', 502, { error: 'the assistant could not answer', kind: 'ask' }, 'the assistant could not answer (ask)'],
    ['413 shows the error', 413, { error: 'a question is at most 16384 bytes' }, 'a question is at most 16384 bytes'],
    ['400 shows the error', 400, { error: 'the question may not be blank' }, 'the question may not be blank'],
  ];
  test.each(failures)('%s', async (_name, status, body, line) => {
    await open({ ask: () => [status, body] });
    type('hi');
    submit();
    await settle();
    const shown = [...q('ask-error').querySelectorAll('p')].map((p) => p.textContent);
    expect(shown).toContain(line);
    // The failed turn is not appended, and the question stays for a retry.
    expect(texts('turn-question')).toEqual(['cto question']);
    expect(q('question').value).toBe('hi');
    expect(q('ask').disabled).toBe(false);
  });
});
