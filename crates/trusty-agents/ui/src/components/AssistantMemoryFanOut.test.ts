import { afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
vi.mock('../lib/assistantMemory', () => ({ fetchAssistantMemory: vi.fn(), saveAssistantFanOut: vi.fn() }));
import { fetchAssistantMemory, saveAssistantFanOut } from '../lib/assistantMemory';
import AssistantMemoryFanOut from './AssistantMemoryFanOut.svelte';

const fixture = {
  assistant: 'izzie',
  palace: null,
  fan_out: [] as string[],
  resolved: { own: 'izzie', source: 'instance-id' as const, fan_out_palaces: [] as string[] },
  available: ['cto-assistant'],
};

let component: ReturnType<typeof mount> | undefined;
async function settle() { await Promise.resolve(); await tick(); await Promise.resolve(); await tick(); }
const box = () => document.querySelector('input[type="checkbox"]') as HTMLInputElement;
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.innerHTML = ''; vi.resetAllMocks(); });

it('shows the resolved palace rather than the empty stored value', async () => {
  vi.mocked(fetchAssistantMemory).mockResolvedValue(fixture);
  component = mount(AssistantMemoryFanOut, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  expect(document.body.textContent).toContain('izzie');
  expect(document.body.textContent).toContain('named after the assistant');
  expect(document.body.textContent).toContain('goes here and nowhere else');
  expect(box().checked).toBe(false);
});

it('sends the whole selection when another assistant is ticked, and shows its palace', async () => {
  vi.mocked(fetchAssistantMemory).mockResolvedValue(fixture);
  vi.mocked(saveAssistantFanOut).mockResolvedValue({
    ...fixture,
    fan_out: ['cto-assistant'],
    resolved: { ...fixture.resolved, fan_out_palaces: ['cto'] },
  });
  component = mount(AssistantMemoryFanOut, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  box().click(); await settle();
  expect(saveAssistantFanOut).toHaveBeenCalledWith('izzie', fixture, ['cto-assistant']);
  expect(document.body.textContent).toContain('reading palaces: cto');
  expect(box().checked).toBe(true);
});

it('unticking removes only that assistant', async () => {
  const selected = { ...fixture, fan_out: ['cto-assistant'], resolved: { ...fixture.resolved, fan_out_palaces: ['cto'] } };
  vi.mocked(fetchAssistantMemory).mockResolvedValue(selected);
  vi.mocked(saveAssistantFanOut).mockResolvedValue(fixture);
  component = mount(AssistantMemoryFanOut, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  box().click(); await settle();
  expect(saveAssistantFanOut).toHaveBeenCalledWith('izzie', selected, []);
});

it('shows a refused selection as an error and keeps the previous state', async () => {
  vi.mocked(fetchAssistantMemory).mockResolvedValue(fixture);
  vi.mocked(saveAssistantFanOut).mockRejectedValue(new Error('fan-out selects OTHER assistants only'));
  component = mount(AssistantMemoryFanOut, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  box().click(); await settle();
  expect(document.body.textContent).toContain('fan-out selects OTHER assistants only');
  expect(document.body.textContent).not.toContain('reading palaces');
});

it('shows a failed read as unavailable rather than as an assistant with no palace', async () => {
  vi.mocked(fetchAssistantMemory).mockRejectedValue(new Error('Assistant not found'));
  component = mount(AssistantMemoryFanOut, { target: document.body, props: { agentName: 'ghost' } }); await settle();
  expect(document.body.textContent).toContain('Assistant not found');
  expect(document.body.textContent).not.toContain('goes here and nowhere else');
});
