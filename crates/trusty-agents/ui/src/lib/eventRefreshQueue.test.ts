import { afterEach, beforeEach, describe, expect, it, vi } from 'vitest';
import { createEventRefreshQueue } from './eventRefreshQueue';
const context = { ready: true, running: false, agent: 'a', project: 'p' };
const settle = async () => { for (let i = 0; i < 10; i++) await Promise.resolve(); };
describe('durable event refresh queue', () => {
  beforeEach(() => vi.useFakeTimers());
  afterEach(() => vi.useRealTimers());
  it('retains unavailable notifications and retries without spinning, then acknowledges success', async () => {
    const fetch = vi.fn().mockRejectedValueOnce(new Error('busy')).mockResolvedValue(0);
    const queue = createEventRefreshQueue(fetch);
    queue.update(context); queue.notify('a'); await settle();
    expect(fetch).toHaveBeenCalledTimes(1);
    queue.update(context); await settle();
    expect(fetch).toHaveBeenCalledTimes(1);
    await vi.advanceTimersByTimeAsync(1000);
    expect(fetch).toHaveBeenCalledTimes(2);
    await vi.advanceTimersByTimeAsync(60_000);
    queue.update(context); await settle();
    expect(fetch).toHaveBeenCalledTimes(2);
    queue.dispose();
  });
  it('does not overlap requests and refreshes again for an arrival during fetch', async () => {
    let resolve!: (value: number) => void;
    const fetch = vi.fn().mockImplementationOnce(() => new Promise<number>(done => { resolve = done; })).mockResolvedValue(1);
    const queue = createEventRefreshQueue(fetch);
    queue.update(context); queue.notify('a'); await settle();
    queue.notify('a'); queue.notify('a'); queue.update(context); await settle();
    expect(fetch).toHaveBeenCalledTimes(1);
    resolve(1); await settle();
    expect(fetch).toHaveBeenCalledTimes(2);
    queue.dispose();
  });
  it('refreshes the new project after an old-project request completes', async () => {
    let resolve!: (value: number) => void;
    const fetch = vi.fn().mockImplementationOnce(() => new Promise<number>(done => { resolve = done; })).mockResolvedValue(0);
    const queue = createEventRefreshQueue(fetch);
    queue.update(context); queue.notify('a'); await settle();
    queue.update({ ...context, project: 'next' }); await settle();
    expect(fetch.mock.calls).toEqual([['a', 'p']]);
    resolve(0); await settle();
    expect(fetch.mock.calls).toEqual([['a', 'p'], ['a', 'next']]);
    queue.dispose();
  });
  it('refreshes after switching away and back to the assistant in a new project', async () => {
    let resolve!: (value: number) => void;
    const fetch = vi.fn().mockImplementationOnce(() => new Promise<number>(done => { resolve = done; })).mockResolvedValue(0);
    const queue = createEventRefreshQueue(fetch);
    queue.update(context); queue.notify('a'); await settle();
    queue.update({ ...context, agent: 'b' });
    queue.update({ ...context, project: 'next' }); await settle();
    expect(fetch.mock.calls).toEqual([['a', 'p']]);
    resolve(0); await settle();
    expect(fetch.mock.calls).toEqual([['a', 'p'], ['a', 'next']]);
    queue.dispose();
  });
  it('waits for readiness and idle, preserves other assistants, and dispatches with current project', async () => {
    const fetch = vi.fn().mockResolvedValue(0);
    const queue = createEventRefreshQueue(fetch);
    queue.notify('a'); queue.notify('b');
    queue.update({ ...context, ready: false }); await settle();
    queue.update({ ...context, running: true }); await settle();
    expect(fetch).not.toHaveBeenCalled();
    queue.update(context); await settle();
    expect(fetch.mock.calls).toEqual([['a', 'p']]);
    queue.update({ ...context, agent: 'b', project: 'next' }); await settle();
    expect(fetch.mock.calls).toEqual([['a', 'p'], ['b', 'next']]);
    queue.dispose();
  });
  it('bounds backoff at thirty seconds and cancels retry on disposal', async () => {
    const fetch = vi.fn().mockRejectedValue(new Error('unavailable'));
    const queue = createEventRefreshQueue(fetch);
    queue.update(context); queue.notify('a'); await settle();
    for (const delay of [1000, 2000, 4000, 8000, 16000, 30000, 30000]) {
      const calls = fetch.mock.calls.length;
      await vi.advanceTimersByTimeAsync(delay - 1);
      expect(fetch).toHaveBeenCalledTimes(calls);
      await vi.advanceTimersByTimeAsync(1);
      expect(fetch).toHaveBeenCalledTimes(calls + 1);
    }
    queue.dispose(); const calls = fetch.mock.calls.length;
    await vi.advanceTimersByTimeAsync(100000);
    expect(fetch).toHaveBeenCalledTimes(calls);
  });
  it('does not retry an in-flight request after disposal', async () => {
    let reject!: (error: Error) => void;
    const fetch = vi.fn(() => new Promise<number>((_, fail) => { reject = fail; }));
    const queue = createEventRefreshQueue(fetch);
    queue.update(context); queue.notify('a'); await settle();
    queue.dispose(); reject(new Error('closed')); await settle();
    await vi.advanceTimersByTimeAsync(60000);
    expect(fetch).toHaveBeenCalledTimes(1);
  });
});
