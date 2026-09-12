import { afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
import { writable } from 'svelte/store';
vi.mock('../stores/app', () => ({ activeAgentId: writable<string | null>('izzie'), agentRoster: writable([{ id: 'izzie', kind: 'assistant' }, { id: 'other', kind: 'assistant' }]) }));
vi.mock('../stores/workspace', () => ({ setAssistantDefaultProjects: vi.fn(), assistantKnowledgeAttachments: writable([{ chat_id: 'local-chat', projects: ['/attached'] }]) }));
vi.mock('../lib/assistantKnowledge', () => ({ fetchAssistantKnowledge: vi.fn().mockResolvedValue({assistant:'izzie',pipeline:null}), assistantDefaultRoots: vi.fn().mockReturnValue([]), syncKnowledgeProjects: vi.fn() }));
import { activeAgentId } from '../stores/app';
import { assistantKnowledgeAttachments } from '../stores/workspace';
import { syncKnowledgeProjects } from '../lib/assistantKnowledge';
import KnowledgeProjectSync from './KnowledgeProjectSync.svelte';
let component: ReturnType<typeof mount> | undefined;
async function settle() { await Promise.resolve(); await tick(); await Promise.resolve(); await tick(); }
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.innerHTML = ''; vi.clearAllMocks(); activeAgentId.set('izzie'); });
it('waits for API readiness before sending existing attachments', async () => {
  component = mount(KnowledgeProjectSync, { target: document.body, props: { ready: false } }); await settle();
  expect(syncKnowledgeProjects).not.toHaveBeenCalled();
});
it('keeps late failures associated with the old assistant and preserves local attachment state', async () => {
  let rejectOld!: (error: Error) => void;
  vi.mocked(syncKnowledgeProjects).mockImplementationOnce(() => new Promise((_resolve, reject) => { rejectOld = reject; })).mockResolvedValue(undefined);
  component = mount(KnowledgeProjectSync, { target: document.body, props: { ready: true } }); await settle();
  activeAgentId.set('other'); await settle();
  rejectOld(new Error('Old assistant conflict')); await settle();
  expect(document.body.textContent).not.toContain('Old assistant conflict');
  const { get } = await import('svelte/store');
  expect(get(assistantKnowledgeAttachments)).toEqual([{ chat_id: 'local-chat', projects: ['/attached'] }]);
});
it('offers retry after a failed attachment synchronization', async () => {
  vi.mocked(syncKnowledgeProjects).mockRejectedValueOnce(new Error('Service unavailable')).mockResolvedValue(undefined);
  component = mount(KnowledgeProjectSync, { target: document.body, props: { ready: true } }); await settle();
  expect(document.body.textContent).toContain('Service unavailable');
  document.querySelector('button')!.click(); await settle();
  expect(syncKnowledgeProjects).toHaveBeenCalledTimes(2);
  expect(document.querySelector('[role="alert"]')).toBeNull();
});

it('retries failed defaults reads independently of successful attachment writes', async () => {
  const { fetchAssistantKnowledge } = await import('../lib/assistantKnowledge');
  vi.mocked(fetchAssistantKnowledge).mockRejectedValueOnce(new Error('defaults offline')).mockResolvedValue({ assistant: 'izzie', pipeline: null } as never);
  vi.mocked(syncKnowledgeProjects).mockResolvedValue(undefined);
  component = mount(KnowledgeProjectSync, { target: document.body, props: { ready: true } }); await settle();
  expect(document.body.textContent).toContain('defaults offline');
  const reads = vi.mocked(fetchAssistantKnowledge).mock.calls.length;
  document.querySelector('button')!.click(); await settle();
  expect(fetchAssistantKnowledge).toHaveBeenCalledTimes(reads + 1);
  expect(document.querySelector('[role=alert]')).toBeNull();
});
