import { expect, it, vi } from 'vitest';
import { createKnowledgeProjectSync } from './knowledgeProjectSync';
const chat = (path: string) => [{ chat_id: 'chat-a', projects: path ? [path] : [] }];
it('serializes changes and sends the latest detach after an in-flight attach', async () => {
  let release!: () => void;
  const send = vi.fn().mockImplementationOnce(() => new Promise<void>(resolve => { release = resolve; })).mockResolvedValue(undefined);
  const sync = createKnowledgeProjectSync(send, vi.fn());
  const first = sync.update('izzie', chat('/a'));
  sync.update('izzie', chat('/b'));
  const last = sync.update('izzie', chat(''));
  expect(send).toHaveBeenCalledTimes(1);
  release(); await first; await last;
  expect(send.mock.calls).toEqual([['izzie', chat('/a')], ['izzie', chat('')]]);
});
it('keeps acknowledgement and errors scoped to the assistant, and can retry failed snapshots', async () => {
  const send = vi.fn().mockRejectedValueOnce(new Error('Conflict')).mockResolvedValue(undefined);
  const report = vi.fn();
  const sync = createKnowledgeProjectSync(send, report);
  await sync.update('izzie', chat('/a'));
  expect(report).toHaveBeenCalledWith('izzie', 'Conflict');
  await sync.update('other', chat('/a'));
  await sync.update('izzie', chat('/a'));
  await sync.update('izzie', chat('/a'));
  expect(send.mock.calls.map(call => call[0])).toEqual(['izzie', 'other', 'izzie']);
  expect(report).toHaveBeenLastCalledWith('izzie', null);
});
