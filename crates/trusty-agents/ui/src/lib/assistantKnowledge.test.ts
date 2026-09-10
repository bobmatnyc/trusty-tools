import { afterEach, expect, it, vi } from 'vitest';
vi.mock('../stores/app', () => ({ getCurrentApiToken: () => 'fixture-token' }));
vi.mock('./api-config', () => ({ apiBase: () => '' }));
import { fetchAssistantKnowledge, reconcileAssistantKnowledge, updateKnowledgeProjects } from './assistantKnowledge';
afterEach(() => vi.unstubAllGlobals());
it('authenticates scoped requests and carries exact revision plus per-chat attachments', async () => {
  const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ assistant: 'test name', pipeline: null }) });
  vi.stubGlobal('fetch', fetcher);
  await updateKnowledgeProjects('test name', 'r1', { chat_id: 'chat-a', projects: ['/project'] });
  expect(fetcher).toHaveBeenCalledWith('/api/agents/test%20name/knowledge/pipeline/projects', expect.objectContaining({ method: 'PUT', headers: expect.objectContaining({ Authorization: 'Bearer fixture-token' }), body: JSON.stringify({ revision: 'r1', chat_id: 'chat-a', projects: ['/project'] }) }));
});
it('keeps reads side-effect free and initializes only through explicit reconcile', async () => {
  const fetcher = vi.fn().mockResolvedValue({ ok: true, json: async () => ({ assistant: 'izzie', pipeline: null }) });
  vi.stubGlobal('fetch', fetcher);
  await fetchAssistantKnowledge('izzie');
  expect(fetcher.mock.calls[0][1].method).toBe('GET');
  await reconcileAssistantKnowledge('izzie', null);
  expect(fetcher.mock.calls[1][1]).toMatchObject({ method: 'POST', body: '{"revision":null}' });
});
it('surfaces API errors and rejects cross-assistant responses', async () => {
  vi.stubGlobal('fetch', vi.fn().mockResolvedValueOnce({ ok: false, status: 409, json: async () => ({ error: 'Reload before retrying' }) }).mockResolvedValueOnce({ ok: true, json: async () => ({ assistant: 'other', pipeline: null }) }));
  await expect(fetchAssistantKnowledge('izzie')).rejects.toThrow('Reload before retrying');
  await expect(fetchAssistantKnowledge('izzie')).rejects.toThrow('another assistant');
});
