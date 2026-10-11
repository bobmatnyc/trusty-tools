import { afterEach, beforeEach, describe, expect, test } from 'vitest';
import { flushSync, mount, unmount } from 'svelte';
import App from './App.svelte';
import { createApi } from './lib/api.js';
import { AGENT_HINT, KIND_TEXT } from './lib/kinds.js';
import { PROJECT, VAULT, fakeConsole } from './lib/fake.js';

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

/** A click with its click count: detail 2 is the second click of a double-click. */
function click(el, detail) {
  el.dispatchEvent(new MouseEvent('click', { bubbles: true, cancelable: true, detail }));
  flushSync();
}

const button = (text) => [...target.querySelectorAll('button')].find((b) => b.textContent.trim() === text);

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

  test('copy offers the reported available backends', async () => {
    await open();
    const options = [...q('copy-from').querySelectorAll('option')].map((o) => o.value);
    expect(options).toEqual(['keychain', 'file']);
  });

  // #9067: copy overwrites every key of the project in the destination.
  test('copy sends no call until the confirmation', async () => {
    const fake = await open();
    await submit(q('copy-form'));
    expect(fake.calls.some((c) => c.verb === 'copy')).toBe(false);
    expect(q('confirm-copy')).not.toBeNull();
  });

  test('the copy confirmation names both backends and the key count', async () => {
    const fake = await open({
      list: (body) => [200, { vault: body.vault, keys: ['A', 'B', 'C'].map((name) => ({ name, length: 1, updated_at: 't' })) }],
    });
    await submit(q('copy-form'));
    const text = q('copy-confirm').textContent;
    expect(text).toContain('keychain');
    expect(text).toContain('file');
    expect(text).toContain('3 keys');
    // The count is the project vault's: the one copy reads from.
    expect(fake.calls.filter((c) => c.verb === 'list').pop().body.vault).toBe(VAULT);
  });

  test('confirming a copy sends exactly one call, with no vault', async () => {
    const fake = await open();
    await submit(q('copy-form'));
    click(q('confirm-copy'), 2);
    await settle();
    expect(fake.calls.some((c) => c.verb === 'copy')).toBe(false);
    click(q('confirm-copy'), 1);
    await settle();
    const copies = fake.calls.filter((c) => c.verb === 'copy');
    expect(copies).toHaveLength(1);
    expect(copies[0].body).toEqual({ project: PROJECT, from_backend: 'keychain', to_backend: 'file', keys: [] });
    expect('vault' in copies[0].body).toBe(false);
    expect(q('copied').textContent).toContain('Copied 1');
  });

  test('cancelling a copy sends nothing', async () => {
    const fake = await open();
    await submit(q('copy-form'));
    click(q('cancel-copy'), 1);
    await settle();
    expect(q('copy-confirm')).toBeNull();
    expect(fake.calls.some((c) => c.verb === 'copy')).toBe(false);
  });

  test('copy defaults from to the selected backend', async () => {
    await open({
      backends: () => [200, {
        selected: 'file',
        backends: [{ id: 'keychain', available: true }, { id: 'file', available: true }],
      }],
    });
    expect(q('copy-from').value).toBe('file');
    expect(q('copy-to').value).toBe('keychain');
  });

  // #9067: a Keychain delete cannot be undone.
  test('a double-click on Delete sends no delete call', async () => {
    const fake = await open();
    const slot = q('delete').closest('td');
    click(q('delete'), 1);
    // The second click lands where the pointer is, or on a confirm button
    // that appeared there.
    click(slot.querySelector('button'), 2);
    if (q('confirm-delete')) click(q('confirm-delete'), 2);
    await settle();
    expect(fake.calls.some((c) => c.verb === 'delete')).toBe(false);
  });

  test('delete then confirm sends exactly one delete call', async () => {
    const fake = await open();
    click(q('delete'), 1);
    expect(fake.calls.some((c) => c.verb === 'delete')).toBe(false);
    click(q('confirm-delete'), 1);
    await settle();
    const deletes = fake.calls.filter((c) => c.verb === 'delete');
    expect(deletes).toHaveLength(1);
    expect(deletes[0].body).toEqual({ project: PROJECT, vault: VAULT, key: 'API_KEY' });
  });

  test('cancel or Escape leaves the key untouched', async () => {
    const fake = await open();
    click(q('delete'), 1);
    click(button('Cancel'), 1);
    expect(q('confirm-delete')).toBeNull();
    click(q('delete'), 1);
    window.dispatchEvent(new KeyboardEvent('keydown', { key: 'Escape', bubbles: true }));
    flushSync();
    expect(q('confirm-delete')).toBeNull();
    await settle();
    expect(fake.calls.some((c) => c.verb === 'delete')).toBe(false);
    expect(q('keys').querySelector('[data-key="API_KEY"]')).not.toBeNull();
  });

  test('a console refusal with no kind renders its text', async () => {
    const fake = fakeConsole({ projects: () => [403, { error: "the request is not from this console's own page" }] });
    app = mount(App, { target, props: { api: createApi(fake.fetchImpl) } });
    await settle();
    expect(q('notice').textContent).toContain("the request is not from this console's own page");
  });
});
