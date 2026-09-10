import { afterEach, expect, it, vi } from 'vitest';
vi.mock('./api-config', () => ({ apiBase: () => '' }));
vi.mock('../stores/app', () => ({ getCurrentApiToken: () => 'fixture-token' }));
import { acquireStoredImage, IMAGE_CACHE_LIMITS } from './storedImages';
const releases: (() => void)[] = [];
afterEach(async () => { releases.splice(0).forEach(release => release()); await new Promise(resolve => setTimeout(resolve,0)); vi.unstubAllGlobals(); });
it('bounds requests and retained bytes while every nearby image makes progress', async () => {
  let active = 0, maxActive = 0, maxBytes = 0, serial = 0;
  const retained = new Map<string,number>();
  const fetcher = vi.fn().mockImplementation(async () => {
    active++; maxActive = Math.max(maxActive,active);
    await Promise.resolve(); active--;
    return new Response(new Uint8Array(5 * 1024 * 1024),{headers:{'Content-Type':'image/png'}});
  });
  vi.stubGlobal('fetch',fetcher);
  vi.stubGlobal('URL',{
    createObjectURL:vi.fn((blob: Blob) => { const url = 'blob:' + serial++; retained.set(url,blob.size); maxBytes = Math.max(maxBytes,[...retained.values()].reduce((sum,size) => sum+size,0)); return url; }),
    revokeObjectURL:vi.fn((url: string) => retained.delete(url)),
  });
  const ready = vi.fn(), evicted = vi.fn();
  for (let i = 0; i < 6; i++) releases.push(acquireStoredImage('alice',String(i),ready,evicted));
  expect(fetcher).toHaveBeenCalledTimes(IMAGE_CACHE_LIMITS.concurrent);
  await vi.waitFor(() => expect(ready).toHaveBeenCalledTimes(6));
  expect(maxActive).toBeLessThanOrEqual(IMAGE_CACHE_LIMITS.concurrent);
  expect(maxBytes).toBeLessThanOrEqual(IMAGE_CACHE_LIMITS.bytes);
  expect(retained.size).toBe(2);
  expect(evicted).toHaveBeenCalledTimes(4);
});
it('retains at most four small image URLs and cancels queued work on release', async () => {
  const fetcher = vi.fn().mockImplementation(() => Promise.resolve(new Response(new Uint8Array([137,80,78,71]),{headers:{'Content-Type':'image/png'}})));
  vi.stubGlobal('fetch',fetcher);
  const revoke = vi.fn();
  vi.stubGlobal('URL',{createObjectURL:vi.fn().mockReturnValue('blob:small'),revokeObjectURL:revoke});
  const ready = vi.fn();
  for (let i = 0; i < 6; i++) releases.push(acquireStoredImage('alice',String(i),ready,vi.fn()));
  releases[5]();
  await vi.waitFor(() => expect(ready).toHaveBeenCalledTimes(5));
  expect(fetcher).toHaveBeenCalledTimes(5);
  expect(revoke).toHaveBeenCalledTimes(1);
});
it.each([1,4])('loads nearby 4+4+%i MiB images without a scroll or unmount', async last => {
  const sizes = [4,4,last];
  const fetcher = vi.fn().mockImplementation(() => Promise.resolve(new Response(new Uint8Array(sizes.shift()! * 1024 * 1024),{headers:{'Content-Type':'image/png'}})));
  vi.stubGlobal('fetch',fetcher);
  const revoke = vi.fn(), ready = vi.fn();
  vi.stubGlobal('URL',{createObjectURL:vi.fn().mockReturnValue('blob:image'),revokeObjectURL:revoke});
  for (let i = 0; i < 3; i++) releases.push(acquireStoredImage('alice',String(i),ready,vi.fn()));
  await vi.waitFor(() => expect(ready).toHaveBeenCalledTimes(3));
  expect(fetcher).toHaveBeenCalledTimes(3);
  expect(revoke).toHaveBeenCalledTimes(last === 1 ? 0 : 1);
});
