export interface RefreshContext { ready: boolean; running: boolean; agent: string | null; project: string }

/** Acknowledge durable-history notifications only after successful catchup. */
export function createEventRefreshQueue(refresh: (agent: string, project: string) => Promise<number>, onError: (error: unknown) => void = () => {}) {
  type Pending = { version: number; running: boolean; failures: number; retryAt: number; activeProject?: string };
  const pending = new Map<string, Pending>();
  let context: RefreshContext = { ready: false, running: false, agent: null, project: '' };
  let disposed = false;
  let timer: ReturnType<typeof setTimeout> | undefined;
  function pump() {
    if (timer !== undefined) { clearTimeout(timer); timer = undefined; }
    const { ready, running, agent, project } = context;
    if (disposed || !ready || running || !agent) return;
    const item = pending.get(agent);
    if (!item || item.running) return;
    const delay = item.retryAt - Date.now();
    if (delay > 0) { timer = setTimeout(pump, delay); return; }
    item.running = true;
    item.activeProject = project;
    const version = item.version;
    void Promise.resolve().then(() => disposed ? 0 : refresh(agent, project)).then(() => {
      if (disposed) return;
      if (item.version === version) pending.delete(agent);
      else { item.failures = 0; item.retryAt = 0; }
    }).catch(error => {
      if (disposed) return;
      item.failures++;
      item.retryAt = Date.now() + Math.min(30_000, 1000 * 2 ** Math.min(item.failures - 1, 5));
      onError(error);
    }).finally(() => { item.running = false; if (!disposed) pump(); });
  }
  return {
    notify(agent: string) {
      if (disposed || !agent) return;
      const item = pending.get(agent);
      if (item) item.version++;
      else pending.set(agent, { version: 1, running: false, failures: 0, retryAt: 0 });
      pump();
    },
    update(next: RefreshContext) {
      // The in-flight result belongs to its captured project. Keep a second
      // catchup pending when this assistant moves to another project meanwhile.
      if (next.agent) {
        const item = pending.get(next.agent);
        if (item?.running && item.activeProject !== next.project) item.version++;
      }
      context = next;
      pump();
    },
    dispose() { disposed = true; if (timer !== undefined) clearTimeout(timer); pending.clear(); },
  };
}
