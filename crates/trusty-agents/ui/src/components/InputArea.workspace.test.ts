import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { flushSync, mount, unmount, tick } from 'svelte';
const transport = vi.hoisted(() => ({ invoke: vi.fn(), listenEvent: vi.fn(), cancelTask: vi.fn() }));
vi.mock('../lib/transport', () => ({ ...transport, isDesktop: () => false }));
import { get } from 'svelte/store';
import InputArea from './InputArea.svelte';
import { activeAgentId, activeProjectId, activeTaskId, isRunning, activeMessages, updateMessageByTask, messages, conversationForTask } from '../stores/app';
import { attachRootToChat } from '../stores/workspace';
let target: HTMLDivElement;
let instance: ReturnType<typeof mount>;
function deferred<T>() { let resolve!: (value: T) => void; const promise = new Promise<T>(r => resolve = r); return { promise, resolve }; }
function submit() {
  const input = target.querySelector('textarea')!;
  flushSync(() => { input.value = 'Check the project'; input.dispatchEvent(new Event('input', { bubbles: true })); });
  (target.querySelector('[aria-label="Send message"]') as HTMLButtonElement).click();
}
beforeEach(() => {
  vi.clearAllMocks();
  messages.set(new Map());
  vi.stubGlobal('fetch', vi.fn(async () => ({ ok: true, json: async () => ({ providers: [], local: { provider_id: 'local', default_model: 'local', available: false } }) })));
  vi.stubGlobal('confirm', () => true);
  transport.invoke.mockResolvedValue('Done');
  transport.listenEvent.mockResolvedValue(() => {});
  transport.cancelTask.mockResolvedValue({ id: 'old', http_status: 200 });
  activeProjectId.set('ctrl'); activeAgentId.set('izzie'); isRunning.set(false); activeTaskId.set(null);
  attachRootToChat({ id: 'first', name: 'First', path: '/first' });
  target = document.createElement('div'); document.body.appendChild(target);
  instance = mount(InputArea, { target });
});
afterEach(() => { unmount(instance); target.remove(); vi.unstubAllGlobals(); isRunning.set(false); activeTaskId.set(null); });
describe('message workspace snapshot', () => {
  it('keeps the submitted assistant and folder while listener registration is pending', async () => {
    const listener = deferred<() => void>(); transport.listenEvent.mockReturnValueOnce(listener.promise);
    submit();
    activeAgentId.set('other'); attachRootToChat({ id: 'second', name: 'Second', path: '/second' });
    listener.resolve(() => {});
    await vi.waitFor(() => expect(transport.invoke).toHaveBeenCalledWith('send_message', expect.objectContaining({ agent: 'izzie', projectPath: '/first' })));
  });
  it('keeps the submitted folder while retask cancellation is pending', async () => {
    const cancel = deferred<{ id: string; http_status: number }>(); transport.cancelTask.mockReturnValueOnce(cancel.promise);
    isRunning.set(true); activeTaskId.set('old');
    submit();
    attachRootToChat({ id: 'second', name: 'Second', path: '/second' });
    cancel.resolve({ id: 'old', http_status: 200 });
    await vi.waitFor(() => expect(transport.invoke).toHaveBeenCalledWith('send_message', expect.objectContaining({ agent: 'izzie', projectPath: '/first' })));
  });
});


it('does not submit in another directory when the attached working folder is missing', async () => {
  attachRootToChat({ id: 'missing', name: 'Missing', path: '/missing', available: false });
  await tick();
  submit();
  await tick();
  expect(transport.invoke).not.toHaveBeenCalledWith('send_message', expect.anything());
  const resolve = [...target.querySelectorAll('button')].find(button => button.textContent === 'Resolve folder')!;
  resolve.click();
  await tick();
  expect(target.querySelector('[role="alert"]')).toBeNull();
});

it.each(['', '   \n'])('preserves terminal failure text when native return is blank (%j)', async result => {
  const response = deferred<string>();
  transport.invoke.mockImplementation((command: string) => command === 'send_message' ? response.promise : Promise.resolve(''));
  submit();
  await vi.waitFor(() => expect(transport.invoke).toHaveBeenCalledWith('send_message', expect.anything()));
  const task = get(activeTaskId)!;
  expect(task).toBeTruthy();
  updateMessageByTask(conversationForTask(task)!, task, 'Error: Tool request failed');
  expect(get(activeMessages).find(message => message.taskId === task)?.content).toBe('Error: Tool request failed');
  response.resolve(result);
  await vi.waitFor(() => expect(get(isRunning)).toBe(false));
  expect(get(activeMessages).find(message => message.taskId === task)?.content).toBe('Error: Tool request failed');
});

it('forwards an attachment-only message with frozen image bytes and keeps its preview', async () => {
  const { composerDrafts, emptyDraft } = await import('../stores/composerDrafts');
  const { conversationKey } = await import('../stores/app');
  const attachment = { kind: 'image' as const, name: 'chart.png', mime_type: 'image/png' as const, data_base64: 'iVBORw0KGgo=' };
  composerDrafts.set({ [conversationKey('ctrl','izzie')]: { ...emptyDraft, items: [{id:'image',sourceBytes:8,attachment}] } });
  await tick();
  expect((target.querySelector('[aria-label="Send message"]') as HTMLButtonElement).disabled).toBe(false);
  (target.querySelector('[aria-label="Send message"]') as HTMLButtonElement).click();
  await vi.waitFor(() => expect(transport.invoke).toHaveBeenCalledWith('send_message', expect.objectContaining({ content: '', agent: 'izzie', inlineAttachments: [attachment] })));
  expect(get(activeMessages).find(message => message.role === 'user')?.inlineAttachments).toEqual([attachment]);
  composerDrafts.set({});
});
