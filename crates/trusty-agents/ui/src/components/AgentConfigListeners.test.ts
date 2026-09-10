import { afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
vi.mock('../lib/listeners', () => ({ fetchListeners: vi.fn(), saveListeners: vi.fn() }));
import { fetchListeners, saveListeners, type ListenerConfiguration } from '../lib/listeners';
import AgentConfigListeners from './AgentConfigListeners.svelte';
const configuration: ListenerConfiguration = { agent: 'fixture', revision: 'rev-1', available_listeners: [{ name: 'mail', connector: 'gmail', identity: null, enabled: true }], listeners: [{ name: 'mail', enabled: true, event_types: ['message.received'], filter: { from: ['*@example.com'], include_labels: [], exclude_labels: ['SPAM'], subject_contains: [], snippet_contains: [] }, instructions: 'Summarize matched messages.' }] };
let component: ReturnType<typeof mount> | undefined;
async function settle() { await Promise.resolve(); await tick(); await Promise.resolve(); await tick(); }
function button(text: string) { return [...document.querySelectorAll('button')].find(item => item.textContent?.includes(text))!; }
function input(label: string, text: string) { const el = document.querySelector(`[aria-label="${label}"]`) as HTMLTextAreaElement; el.value = text; el.dispatchEvent(new Event('input', { bubbles: true })); }
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.innerHTML = ''; vi.resetAllMocks(); });
it('saves deterministic fields and listener instructions with the fetched revision', async () => {
  vi.mocked(fetchListeners).mockResolvedValue(configuration);
  vi.mocked(saveListeners).mockImplementation(async (_agent, _revision, listeners) => ({ ...configuration, revision: 'rev-2', listeners }));
  component = mount(AgentConfigListeners, { target: document.body, props: { agentName: 'fixture' } }); await settle();
  input('mail Include labels', 'INBOX\nIMPORTANT'); input('mail instructions', 'Draft a concise summary.'); await settle();
  button('Save listeners').click(); await settle();
  expect(saveListeners).toHaveBeenCalledWith('fixture', 'rev-1', [expect.objectContaining({ instructions: 'Draft a concise summary.', filter: expect.objectContaining({ include_labels: ['INBOX', 'IMPORTANT'], exclude_labels: ['SPAM'] }) })]);
  expect(document.body.textContent).toContain('Listener settings saved.');
});
it('retains edits on a conflicting save', async () => {
  vi.mocked(fetchListeners).mockResolvedValue(configuration); vi.mocked(saveListeners).mockRejectedValue(new Error('Configuration changed; reload before saving.'));
  component = mount(AgentConfigListeners, { target: document.body, props: { agentName: 'fixture' } }); await settle();
  input('mail instructions', 'Keep my draft'); await settle(); button('Save listeners').click(); await settle();
  expect((document.querySelector('[aria-label="mail instructions"]') as HTMLTextAreaElement).value).toBe('Keep my draft');
  expect(document.body.textContent).toContain('Configuration changed'); expect(button('Save listeners').disabled).toBe(false);
});
it('does not present failed reads as empty configuration', async () => {
  vi.mocked(fetchListeners).mockRejectedValue(new Error('Service unavailable'));
  component = mount(AgentConfigListeners, { target: document.body, props: { agentName: 'fixture' } }); await settle();
  expect(document.body.textContent).toContain('Service unavailable'); expect(document.body.textContent).not.toContain('has no listener bindings'); expect(button('Save listeners').disabled).toBe(true);
});
it('adds known listeners disabled until explicitly enabled', async () => {
  vi.mocked(fetchListeners).mockResolvedValue({ ...configuration, listeners: [] });
  vi.mocked(saveListeners).mockImplementation(async (_a, _r, listeners) => ({ ...configuration, listeners }));
  component = mount(AgentConfigListeners, { target: document.body, props: { agentName: 'fixture' } }); await settle();
  const select = document.querySelector('select')!; select.value = 'mail'; select.dispatchEvent(new Event('change', { bubbles: true })); await settle();
  (document.querySelector('[aria-label="Add listener binding"]') as HTMLButtonElement).click(); await settle(); button('Save listeners').click(); await settle();
  expect(saveListeners).toHaveBeenCalledWith('fixture', 'rev-1', [expect.objectContaining({ name: 'mail', enabled: false, instructions: '' })]);
});
it('keeps inherited listeners visible with an explicit receive-events switch', async () => {
  vi.mocked(fetchListeners).mockResolvedValue({ ...configuration, inherited_names: ['mail'] });
  component = mount(AgentConfigListeners, { target: document.body, props: { agentName: 'fixture' } }); await settle();
  expect(document.querySelector('[aria-label="Remove mail"]')).toBeNull();
  expect(document.body.textContent).toContain('Turn off Receive events');
  expect(document.querySelector('input[type="checkbox"]')).not.toBeNull();
});
