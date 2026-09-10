/** Serialize attachment writes per assistant so an older attach cannot overwrite a later detach. */
import type { KnowledgeChatProjects } from './knowledgeProjects';
type Snapshot = { chats: KnowledgeChatProjects[]; fingerprint: string };
type Queue = { pending?: Snapshot; running?: Promise<void>; acknowledged?: string };

export function createKnowledgeProjectSync(
  send: (assistant: string, chats: KnowledgeChatProjects[]) => Promise<void>,
  report: (assistant: string, error: string | null) => void,
) {
  const queues = new Map<string, Queue>();
  return {
    update(assistant: string, chats: KnowledgeChatProjects[]): Promise<void> {
      const queue = queues.get(assistant) ?? {};
      queues.set(assistant, queue);
      const snapshot = { chats: structuredClone(chats), fingerprint: JSON.stringify(chats) };
      if (queue.acknowledged === snapshot.fingerprint && !queue.running) return Promise.resolve();
      queue.pending = snapshot;
      if (!queue.running) {
        queue.running = (async () => {
          while (queue.pending) {
            const next = queue.pending;
            queue.pending = undefined;
            if (queue.acknowledged === next.fingerprint) continue;
            try {
              await send(assistant, next.chats);
              queue.acknowledged = next.fingerprint;
              report(assistant, null);
            } catch (error) {
              report(assistant, error instanceof Error ? error.message : String(error));
            }
          }
        })().finally(() => { queue.running = undefined; });
      }
      return queue.running;
    },
  };
}
