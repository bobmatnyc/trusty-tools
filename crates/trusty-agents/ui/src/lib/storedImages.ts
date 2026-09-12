import { apiBase } from './api-config';
import { getCurrentApiToken } from '../stores/app';
import { ATTACHMENT_LIMITS } from './chatAttachments';

export const IMAGE_CACHE_LIMITS = { concurrent: 2, entries: 4, bytes: 10 * 1024 * 1024 } as const;
type Lease = { controller: AbortController; reserved: number; url: string; slot: boolean; inFlight: boolean; released: boolean; run: () => void; failed: (error: string) => void };
const queue: Lease[] = [];
const admitted = new Set<Lease>();
let requests = 0, bytes = 0;
function free(lease: Lease): void {
  if (!lease.slot) return;
  bytes -= lease.reserved; lease.reserved = 0; lease.slot = false; admitted.delete(lease);
}
function drop(lease: Lease): void {
  if (lease.released) return;
  lease.released = true; lease.controller.abort();
  const queued = queue.indexOf(lease); if (queued >= 0) queue.splice(queued, 1);
  if (lease.url) { URL.revokeObjectURL(lease.url); lease.url = ''; }
  if (!lease.inFlight) free(lease);
}
function evictOldest(): boolean {
  const victim = [...admitted].find(lease => !lease.inFlight && !!lease.url && !lease.released);
  if (!victim) return false;
  drop(victim);
  victim.failed('Preview released to load another image. Load it again when needed.');
  return true;
}
function drain(): void {
  while (queue.length && requests < IMAGE_CACHE_LIMITS.concurrent) {
    while (admitted.size >= IMAGE_CACHE_LIMITS.entries || bytes >= IMAGE_CACHE_LIMITS.bytes) {
      if (!evictOldest()) return;
    }
    const lease = queue.shift()!;
    if (lease.released) continue;
    // #7370: reserve available bytes; a small queued image need not reserve 5 MiB.
    lease.reserved = Math.min(ATTACHMENT_LIMITS.fileBytes, IMAGE_CACHE_LIMITS.bytes - bytes);
    lease.slot = true; lease.inFlight = true;
    bytes += lease.reserved; admitted.add(lease); requests++; lease.run();
  }
}
function reserveActual(lease: Lease, count: number): void {
  if (count <= lease.reserved) return;
  const extra = count - lease.reserved;
  while (bytes + extra > IMAGE_CACHE_LIMITS.bytes) {
    if (!evictOldest()) throw new Error('Image preview budget is full. Try loading this image again.');
  }
  bytes += extra; lease.reserved = count;
}

/** Why: mounted history must not eagerly retain every image or starve small queued images.
 * What: bound requests, actual streamed bytes and URLs; evict completed previews when capacity is needed.
 * Test: storedImages.test.ts covers 4+4+1 MiB progress, bounded eviction and cancellation.
 */
export function acquireStoredImage(assistant: string, asset: string, ready: (url: string) => void, failed: (error: string) => void): () => void {
  const lease: Lease = { controller: new AbortController(), reserved: 0, url: '', slot: false, inFlight: false, released: false, run: () => {}, failed };
  lease.run = () => {
    const token = getCurrentApiToken();
    const timer = setTimeout(() => lease.controller.abort(), 30_000);
    void fetch(`${apiBase()}/api/agents/${encodeURIComponent(assistant)}/chat-assets/${encodeURIComponent(asset)}`, {
      headers: token ? { Authorization: `Bearer ${token}` } : {}, signal: lease.controller.signal,
    }).then(response => readImage(response, lease)).then(blob => {
      if (lease.released) return;
      bytes += blob.size - lease.reserved; lease.reserved = blob.size;
      lease.url = URL.createObjectURL(blob); ready(lease.url);
    }).catch(cause => { if (!lease.released) failed(String(cause)); }).finally(() => {
      clearTimeout(timer); requests--; lease.inFlight = false;
      if (!lease.url || lease.released) free(lease);
      drain();
    });
  };
  queue.push(lease); drain();
  return () => { drop(lease); drain(); };
}

async function readImage(response: Response, lease: Lease): Promise<Blob> {
  if (!response.ok) throw new Error(`Stored image unavailable (${response.status}).`);
  const mime = response.headers.get('content-type')?.split(';')[0];
  if (!mime || !['image/png','image/jpeg','image/webp'].includes(mime)) throw new Error('Stored asset is not a supported image.');
  const reader = response.body?.getReader();
  if (!reader) throw new Error('Stored image response is empty.');
  const chunks: Uint8Array<ArrayBuffer>[] = []; let count = 0;
  try {
    while (true) {
      const { value, done } = await reader.read(); if (done) break;
      if (lease.released || lease.controller.signal.aborted) throw new Error('Image request cancelled.');
      count += value.byteLength;
      if (count > ATTACHMENT_LIMITS.fileBytes) throw new Error('Stored image exceeds 5 MiB.');
      reserveActual(lease, count);
      chunks.push(new Uint8Array(value));
    }
  } catch (error) { await reader.cancel(); throw error; }
  finally { reader.releaseLock(); }
  return new Blob(chunks, { type: mime });
}
