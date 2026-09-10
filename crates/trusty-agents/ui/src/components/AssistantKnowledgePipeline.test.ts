import { afterEach, expect, it, vi } from 'vitest';
import { mount, unmount, tick } from 'svelte';
vi.mock('../lib/assistantKnowledge', () => ({ fetchAssistantKnowledge: vi.fn(), reconcileAssistantKnowledge: vi.fn(), setKnowledgePaused: vi.fn(), extendKnowledgeHistory: vi.fn() }));
import { fetchAssistantKnowledge, setKnowledgePaused, extendKnowledgeHistory } from '../lib/assistantKnowledge';
import AssistantKnowledgePipeline from './AssistantKnowledgePipeline.svelte';
const blocked = { status: 'blocked_on_dependency' as const, reason: 'Extraction worker unavailable' };
const fixture = { assistant: 'izzie', store_issue: null, index: { connected: false, reason: 'Index not ready' }, sources: [{ id: 'mail', revision: 's1', kind: 'gmail' as const, display_name: 'Gmail', dependency_reasons: ['Connector history unavailable'] }], pipeline: { schema_version: 1, assistant_id: 'izzie', revision: 'r1', anchor_at: '2026-09-09T12:00:00Z', history_months: 1, paused: false, store: { root: '/private/izzie/okg', index_id: 'izzie-kb', protected: true }, projects_by_chat: {}, sources: [], jobs: [{ id: 'j1', source_id: 'mail', source_revision: 's1', window: { start: '2026-08-09T12:00:00Z', end: '2026-09-09T12:00:00Z' }, status: 'blocked_on_dependency' as const, indexing: blocked, extraction: blocked, cleanup: blocked, publication: blocked, dependency_reasons: ['Extraction worker unavailable'] }] } };
let component: ReturnType<typeof mount> | undefined;
async function settle() { await Promise.resolve(); await tick(); await Promise.resolve(); await tick(); }
const button = (name: string) => [...document.querySelectorAll('button')].find(el => el.textContent?.includes(name))!;
afterEach(async () => { if (component) await unmount(component); component = undefined; document.body.innerHTML = ''; vi.resetAllMocks(); });
it('shows protected ownership, requested windows and blockers without claiming completed extraction', async () => {
  vi.mocked(fetchAssistantKnowledge).mockResolvedValue(fixture);
  component = mount(AssistantKnowledgePipeline, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  expect(document.body.textContent).toContain('Protected assistant OKG');
  expect(document.body.textContent).toContain('Requested history: 1 month');
  expect(document.body.textContent).toContain('Index not ready');
  expect(document.body.textContent).toContain('Extraction worker unavailable');
  expect(document.body.textContent).not.toContain('Completed');
});
it('passes the current revision when pausing and requesting one earlier month', async () => {
  vi.mocked(fetchAssistantKnowledge).mockResolvedValue(fixture);
  vi.mocked(setKnowledgePaused).mockResolvedValue({ ...fixture, pipeline: { ...fixture.pipeline, paused: true, revision: 'r2' } });
  vi.mocked(extendKnowledgeHistory).mockResolvedValue({ ...fixture, pipeline: { ...fixture.pipeline, paused: true, history_months: 2, revision: 'r3' } });
  component = mount(AssistantKnowledgePipeline, { target: document.body, props: { agentName: 'izzie' } }); await settle();
  button('Pause').click(); await settle();
  expect(setKnowledgePaused).toHaveBeenCalledWith('izzie', 'r1', true);
  button('Go back one month').click(); await settle();
  expect(extendKnowledgeHistory).toHaveBeenCalledWith('izzie', 'r2', 1);
  expect(document.body.textContent).toContain('Requested history: 2 months');
});
it('shows failed reads as unavailable rather than empty or initialized', async () => {
  vi.mocked(fetchAssistantKnowledge).mockRejectedValue(new Error('Only assistant instances can own knowledge'));
  component = mount(AssistantKnowledgePipeline, { target: document.body, props: { agentName: 'specialist' } }); await settle();
  expect(document.body.textContent).toContain('Only assistant instances can own knowledge');
  expect(document.body.textContent).not.toContain('Initialize knowledge');
});
it('does not show an old assistant response after selection changes', async () => {
  const { createClassComponent } = await import('svelte/legacy');
  let resolveOld!: (value: typeof fixture) => void;
  vi.mocked(fetchAssistantKnowledge).mockImplementationOnce(() => new Promise(resolve => { resolveOld = resolve; })).mockResolvedValueOnce({ ...fixture, assistant: 'other', pipeline: { ...fixture.pipeline, assistant_id: 'other', store: { ...fixture.pipeline.store, root: '/private/other/okg' } } });
  const legacy = createClassComponent({ component: AssistantKnowledgePipeline, target: document.body, props: { agentName: 'izzie' } });
  await settle(); legacy.$set({ agentName: 'other' }); await settle();
  resolveOld(fixture); await settle();
  expect(document.body.textContent).toContain('/private/other/okg');
  expect(document.body.textContent).not.toContain('/private/izzie/okg');
  legacy.$destroy();
});
