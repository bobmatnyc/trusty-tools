import { afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
vi.mock('../lib/listeners', () => ({ fetchListeners: vi.fn(), saveListeners: vi.fn() }));
import { fetchListeners, saveListeners } from '../lib/listeners';
import AgentConfigPanel from './AgentConfigPanel.svelte';
let component: ReturnType<typeof mount> | undefined;
const configuration = { agent: 'fixture', revision: 'v1', available_listeners: [{ name: 'mail', connector: 'gmail', identity: null, enabled: true }], listeners: [{ name: 'mail', enabled: true, event_types: [], filter: { from: [], include_labels: [], exclude_labels: [], subject_contains: [], snippet_contains: [] }, instructions: 'Original' }] };
async function settle() { for (let i = 0; i < 12; i++) { await Promise.resolve(); await tick(); } }
function button(text: string) { return [...document.querySelectorAll('button')].find(el => el.textContent?.trim() === text)!; }
async function open(onExit = vi.fn()) {
  vi.stubGlobal('fetch', vi.fn(async (input: RequestInfo) => {
    const url = String(input), body = url.includes('/persona') ? { content: 'Persona', editable: true } : url.includes('/stores') ? { stores: [], issues: [] } : { name: 'fixture', display_name: 'Fixture', tools_allow: [], scopes: [] };
    return { ok: true, status: 200, json: async () => body, text: async () => JSON.stringify(body) } as Response;
  }));
  vi.mocked(fetchListeners).mockResolvedValue(configuration);
  component = mount(AgentConfigPanel, { target: document.body, props: { agentName: 'fixture', onExit } }); await settle();
  button('Listeners').click(); await settle();
  const textarea = document.querySelector('[aria-label="mail instructions"]') as HTMLTextAreaElement;
  textarea.value = 'Edited'; textarea.dispatchEvent(new Event('input', { bubbles: true })); await settle();
  return textarea;
}
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.innerHTML = ''; vi.resetAllMocks(); vi.unstubAllGlobals(); });
it('preserves listener drafts across preference tabs and guards exit', async () => {
  const exit = vi.fn(), original = await open(exit);
  button('Personality').click(); await settle(); button('Listeners').click(); await settle();
  expect(document.querySelector('[aria-label="mail instructions"]')).toBe(original); expect(original.value).toBe('Edited');
  button('Back to chat').click(); await settle(); expect(document.querySelector('[role="alertdialog"]')).not.toBeNull(); expect(exit).not.toHaveBeenCalled();
});
it('keeps configuration open when save-and-close conflicts', async () => {
  vi.mocked(saveListeners).mockRejectedValue(new Error('Configuration changed'));
  const exit = vi.fn(); await open(exit); button('Back to chat').click(); await settle(); button('Save and close').click(); await settle();
  expect(exit).not.toHaveBeenCalled(); expect(document.querySelector('[role="alertdialog"]')).not.toBeNull();
  expect((document.querySelector('[aria-label="mail instructions"]') as HTMLTextAreaElement).value).toBe('Edited');
});
it('locks Keep editing and other sections throughout a deferred save-and-close', async () => {
  let finish!: (value: typeof configuration) => void;
  vi.mocked(saveListeners).mockReturnValue(new Promise(resolve => { finish = resolve; }));
  const exit = vi.fn(); await open(exit); button('Back to chat').click(); await settle(); button('Save and close').click(); await settle();
  expect(button('Keep editing').disabled).toBe(true); expect(button('Personality').disabled).toBe(true); expect(button('Discard changes').disabled).toBe(true);
  expect(exit).not.toHaveBeenCalled();
  finish({ ...configuration, listeners: [{ ...configuration.listeners[0], instructions: 'Edited' }] }); await settle();
  expect(exit).toHaveBeenCalledOnce();
});
