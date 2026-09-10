import { afterEach, expect, it, vi } from 'vitest';
vi.mock('./transport', () => ({ isDesktop: () => false, invoke: vi.fn() }));
vi.mock('../stores/app', () => ({ tmApi: vi.fn() }));
import { tmApi } from '../stores/app';
import { registerProjectFolder } from './workspaceFiles';
afterEach(() => vi.clearAllMocks());
it('registers an arbitrary server folder in a browser using the canonical response', async () => {
  vi.mocked(tmApi).mockResolvedValue({ id: 'folder', path: '/canonical/non-git', name: 'Documents' });
  expect(await registerProjectFolder(' /alias/non-git ')).toEqual({ id: 'folder', path: '/canonical/non-git', name: 'Documents', available: true });
  expect(tmApi).toHaveBeenCalledWith('/api/projects', { method: 'POST', body: '{"path":"/alias/non-git"}' });
});
it('surfaces rejected registration without creating an attachment', async () => {
  vi.mocked(tmApi).mockRejectedValue(new Error('not a directory'));
  await expect(registerProjectFolder('/file')).rejects.toThrow('not a directory');
});

it('browser roots use validated availability, including missing and unknown status', async () => {
  const { listWorkspaceRoots } = await import('./workspaceFiles');
  vi.mocked(tmApi).mockResolvedValue([{ id: 'a', name: 'A', path: '/a', available: false, availability_reason: 'Missing' }, { id: 'b', name: 'B', path: '/b' }, { id: 'c', name: 'C', path: '/c', available: true }]);
  expect((await listWorkspaceRoots(['/a','/b','/c'])).map(root => root.available)).toEqual([false, false, true]);
});
