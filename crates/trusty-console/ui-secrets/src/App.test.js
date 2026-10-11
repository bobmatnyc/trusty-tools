import { afterEach, beforeEach, describe, expect, test } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import App from './App.svelte';
import { createApi } from './lib/api.js';
import { AGENT_HINT, KIND_TEXT } from './lib/kinds.js';
import { PROJECT, fakeConsole } from './lib/fake.js';

const SECRET = 'SPECIMEN-VALUE-9067-zz';

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

/** Mount the page over a fake console and pick its one project. */
async function open(overrides) {
  const fake = fakeConsole(overrides);
  app = mount(App, { target, props: { api: createApi(fake.fetchImpl) } });
  await settle();
  const select = q('project');
  select.value = PROJECT;
  select.dispatchEvent(new Event('change', { bubbles: true }));
  await settle();
  return fake;
}

function type(el, value) {
  el.value = value;
  el.dispatchEvent(new Event('input', { bubbles: true }));
  flushSync();
}

async function submit(form) {
  form.dispatchEvent(new Event('submit', { bubbles: true, cancelable: true }));
  await settle();
}

describe('the Secrets page', () => {
  test('lists project and owner scopes only, and keys with length and time', async () => {
    await open({
      scopes: () => [200, { scopes: [
        { kind: 'project', vault: 'trusty/acme/web' },
        { kind: 'machine', vault: 'trusty/machine' },
      ] }],
    });
    const scopeText = q('scope').textContent;
    expect(scopeText).toContain('trusty/acme/web');
    expect(scopeText).not.toContain('machine');
    const row = q('keys').querySelector('[data-key="API_KEY"]');
    expect(row.textContent).toContain('32');
    expect(row.textContent).toContain('2026-10-11T12:00:00Z');
  });

  // #9067: the value never stays on the page.
  test('set clears the value input and leaves the value nowhere in the DOM', async () => {
    const fake = await open();
    type(q('set-key'), 'NEW_KEY');
    type(q('set-value'), SECRET);
    await submit(q('set-form'));

    const set = fake.calls.find((c) => c.verb === 'set');
    expect(set.body.value).toBe(SECRET);
    expect(q('set-value').value).toBe('');
    expect(document.body.innerHTML).not.toContain(SECRET);
    for (const input of document.querySelectorAll('input, textarea, select')) {
      expect(input.value).not.toContain(SECRET);
    }
    expect(q('masked').textContent).toContain('sk-live-… [40 chars]');
  });

  test('the value is cleared even when the set is refused', async () => {
    await open({ set: () => [403, { error: 'the secrets server refused the request', kind: 'vault_out_of_scope' }] });
    type(q('set-key'), 'NEW_KEY');
    type(q('set-value'), SECRET);
    await submit(q('set-form'));
    expect(q('set-value').value).toBe('');
    expect(document.body.innerHTML).not.toContain(SECRET);
    expect(q('notice').textContent).toContain(KIND_TEXT.vault_out_of_scope);
  });

  test('a short value shows only [N chars]', async () => {
    await open({ set: () => [200, { outcome: 'updated', masked: '[5 chars]' }] });
    type(q('set-key'), 'PIN');
    type(q('set-value'), 'abcde');
    await submit(q('set-form'));
    const shown = q('masked').textContent;
    expect(shown).toContain('[5 chars]');
    expect(shown).not.toContain('abcde');
    expect(document.body.innerHTML).not.toContain('abcde');
  });

  test('the agents toggle defaults OFF and reverts on agent_use_refused', async () => {
    const fake = await open({
      list: () => [200, { vault: 'trusty/acme/web', keys: [{ name: 'API_KEY', length: 32, updated_at: 't' }] }],
      agents_may_use: () => [403, { error: 'the secrets server refused the request', kind: 'agent_use_refused' }],
    });
    const toggle = q('agents-toggle');
    expect(toggle.checked).toBe(false);
    toggle.checked = true;
    toggle.dispatchEvent(new Event('change', { bubbles: true }));
    await settle();

    expect(fake.calls.find((c) => c.verb === 'agents_may_use').body.allowed).toBe(true);
    expect(q('agents-toggle').checked).toBe(false);
    const lines = [...q('notice').querySelectorAll('p')].map((p) => p.textContent);
    // #9067 P4: the fixed text plus one hint line, no pid, no path.
    expect(lines).toEqual([KIND_TEXT.agent_use_refused, AGENT_HINT]);
    expect(q('notice').textContent).not.toMatch(/pid|\/proc|\d{3,}/i);
  });

  test('copy offers the reported available backends and sends no vault', async () => {
    const fake = await open();
    const options = [...q('copy-from').querySelectorAll('option')].map((o) => o.value);
    expect(options).toEqual(['keychain', 'file']);
    await submit(q('copy-form'));
    const copy = fake.calls.find((c) => c.verb === 'copy');
    expect(copy.body).toEqual({ project: PROJECT, from_backend: 'keychain', to_backend: 'file', keys: [] });
    expect('vault' in copy.body).toBe(false);
    expect(q('copied').textContent).toContain('Copied 1');
  });

  test('delete asks once more, then deletes', async () => {
    const fake = await open();
    q('delete').click();
    flushSync();
    expect(fake.calls.some((c) => c.verb === 'delete')).toBe(false);
    q('confirm-delete').click();
    await settle();
    expect(fake.calls.find((c) => c.verb === 'delete').body).toEqual({
      project: PROJECT, vault: 'trusty/acme/web', key: 'API_KEY',
    });
  });

  test('a console refusal with no kind renders its text', async () => {
    const fake = fakeConsole({ projects: () => [403, { error: "the request is not from this console's own page" }] });
    app = mount(App, { target, props: { api: createApi(fake.fetchImpl) } });
    await settle();
    expect(q('notice').textContent).toContain("the request is not from this console's own page");
  });
});
